//! EPUB decoder: cover, book metadata, and a first-chapter excerpt,
//! in pure Rust — no shell-outs, no zip crate.
//!
//! An EPUB is a ZIP whose members use only two compression methods
//! (stored and deflate), so a small central-directory reader plus the
//! already-locked `flate2` covers the whole format. Structure follows
//! the spec: `META-INF/container.xml` names the OPF; the OPF carries
//! the Dublin Core title/creator, the manifest, and the spine. The
//! excerpt walks the **spine order** (the reading order), not archive
//! order — the approximation the old bsdtar extractor had to make.
//!
//! Failure is a note, never an error: every public function returns an
//! [`EpubPreview`] whose `note` explains, in plain language, why the
//! rich preview is limited (DRM, malformed file, no cover…).

use serde::Serialize;
use std::collections::HashMap;
use std::io::Read;

/// Cap on a single decompressed member — preview material never needs
/// more, and the cap keeps a hostile archive from unzipping forever.
const MAX_MEMBER_BYTES: u64 = 32 * 1024 * 1024;
/// Cap on how much excerpt text the decoder collects.
const MAX_EXCERPT_CHARS: usize = 6000;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EpubCover {
    pub media_type: String,
    /// Raw image bytes — carried to the shell, never into JSON payloads.
    #[serde(skip)]
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct EpubPreview {
    pub title: Option<String>,
    pub author: Option<String>,
    pub cover: Option<EpubCover>,
    /// Opening text from the first spine documents, in reading order.
    pub excerpt: String,
    /// Number of linear spine documents (chapters).
    pub spine_count: usize,
    /// The book's content is encrypted (DRM) — metadata may still read.
    pub drm_protected: bool,
    /// Plain-language reason the preview is limited, when it is.
    pub note: Option<String>,
}

// ---------------------------------------------------------------------------
// ZIP reader (stored + deflate only — the two methods EPUB allows).
// ---------------------------------------------------------------------------

struct ZipEntry {
    name: String,
    method: u16,
    compressed_size: u64,
    uncompressed_size: u64,
    local_offset: u64,
}

struct ZipArchive<'a> {
    data: &'a [u8],
    entries: Vec<ZipEntry>,
}

impl<'a> ZipArchive<'a> {
    fn open(data: &'a [u8]) -> Option<Self> {
        // End Of Central Directory: scan the tail (max comment 64 KiB).
        let tail = data.len().saturating_sub(65_558);
        let mut eocd = None;
        let mut i = data.len().saturating_sub(22);
        while i >= tail {
            if read_u32(data, i) == Some(0x0605_4b50) {
                eocd = Some(i);
                break;
            }
            if i == 0 {
                break;
            }
            i -= 1;
        }
        let eocd = eocd?;
        let count = read_u16(data, eocd + 10)? as usize;
        let mut pos = read_u32(data, eocd + 16)? as usize;
        let mut entries = Vec::with_capacity(count.min(10_000));
        for _ in 0..count {
            if read_u32(data, pos) != Some(0x0201_4b50) {
                break;
            }
            let method = read_u16(data, pos + 10)?;
            let compressed_size = read_u32(data, pos + 20)? as u64;
            let uncompressed_size = read_u32(data, pos + 24)? as u64;
            let name_len = read_u16(data, pos + 28)? as usize;
            let extra_len = read_u16(data, pos + 30)? as usize;
            let comment_len = read_u16(data, pos + 32)? as usize;
            let local_offset = read_u32(data, pos + 42)? as u64;
            let name_start = pos + 46;
            let name_end = name_start.checked_add(name_len)?;
            let name = String::from_utf8_lossy(data.get(name_start..name_end)?).into_owned();
            entries.push(ZipEntry {
                name,
                method,
                compressed_size,
                uncompressed_size,
                local_offset,
            });
            pos = name_end + extra_len + comment_len;
        }
        Some(Self { data, entries })
    }

    fn get(&self, name: &str) -> Option<&ZipEntry> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// Extract one member's bytes (stored or deflated), size-capped.
    fn read(&self, entry: &ZipEntry) -> Option<Vec<u8>> {
        if entry.uncompressed_size > MAX_MEMBER_BYTES {
            return None;
        }
        let pos = entry.local_offset as usize;
        if read_u32(self.data, pos) != Some(0x0403_4b50) {
            return None;
        }
        let name_len = read_u16(self.data, pos + 26)? as usize;
        let extra_len = read_u16(self.data, pos + 28)? as usize;
        let start = pos + 30 + name_len + extra_len;
        let end = start.checked_add(entry.compressed_size as usize)?;
        let raw = self.data.get(start..end)?;
        match entry.method {
            0 => Some(raw.to_vec()),
            8 => {
                let mut out = Vec::with_capacity(entry.uncompressed_size as usize);
                flate2::read::DeflateDecoder::new(raw)
                    .take(MAX_MEMBER_BYTES)
                    .read_to_end(&mut out)
                    .ok()?;
                Some(out)
            }
            _ => None,
        }
    }

    fn read_text(&self, name: &str) -> Option<String> {
        let entry = self.get(name)?;
        let bytes = self.read(entry)?;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }
}

fn read_u16(data: &[u8], at: usize) -> Option<u16> {
    let b = data.get(at..at + 2)?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}

fn read_u32(data: &[u8], at: usize) -> Option<u32> {
    let b = data.get(at..at + 4)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

// ---------------------------------------------------------------------------
// Small XML helpers — targeted extraction, in the codebase's string-scan
// style (see commands.rs `xml_to_text`). Not a general parser; the OPF
// constructs read here are flat and attribute-driven.
// ---------------------------------------------------------------------------

/// Inner text of the first `<tag …>…</tag>`, entities unescaped and
/// whitespace collapsed. Matches `dc:title` style namespaced tags too.
fn tag_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let start = xml.find(&open)?;
    let after = &xml[start + open.len()..];
    // The character after the tag name must end the name (space, >, /).
    let next = after.chars().next()?;
    if !(next == '>' || next == '/' || next.is_whitespace()) {
        return None;
    }
    let gt = after.find('>')?;
    let inner_start = gt + 1;
    let close = format!("</{tag}>");
    let end = after[inner_start..].find(&close)? + inner_start;
    let text = collapse_ws(&unescape(&after[inner_start..end]));
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Value of `attr="…"` (or single-quoted) inside one tag's text.
fn attr<'a>(tag: &'a str, attr: &str) -> Option<&'a str> {
    for quote in ['"', '\''] {
        let needle = format!("{attr}={quote}");
        if let Some(pos) = tag.find(&needle) {
            let rest = &tag[pos + needle.len()..];
            if let Some(end) = rest.find(quote) {
                return Some(&rest[..end]);
            }
        }
    }
    None
}

/// Iterate the raw `<tag …>` opening tags of `xml` (for manifest /
/// spine / guide / meta scans). Namespaced forms (`<enc:CipherReference`)
/// match too — the tag name may be preceded by `<` or `:`.
fn iter_tags<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(pos) = rest.find(tag) {
        let before_ok = pos > 0 && matches!(rest.as_bytes()[pos - 1], b'<' | b':');
        let after = &rest[pos + tag.len()..];
        let next_ok = matches!(after.chars().next(), Some(c) if c == '>' || c == '/' || c.is_whitespace());
        if before_ok && next_ok {
            match after.find('>') {
                Some(gt) => {
                    out.push(&after[..gt]);
                    rest = &after[gt + 1..];
                }
                None => break,
            }
        } else {
            rest = after;
        }
    }
    out
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        let Some(semi) = tail.find(';') else {
            out.push_str(tail);
            return out;
        };
        let entity = &tail[1..semi];
        let decoded = match entity {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                u32::from_str_radix(&entity[2..], 16).ok().and_then(char::from_u32)
            }
            _ if entity.starts_with('#') => {
                entity[1..].parse::<u32>().ok().and_then(char::from_u32)
            }
            _ => None,
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &tail[semi + 1..];
            }
            None => {
                // Unknown entity (incl. &amp; handled below by order):
                if entity == "amp" {
                    out.push('&');
                    rest = &tail[semi + 1..];
                } else {
                    out.push_str(&tail[..semi + 1]);
                    rest = &tail[semi + 1..];
                }
            }
        }
    }
    out.push_str(rest);
    out
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Percent-decode an OPF href (`My%20Book/ch%201.xhtml`).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Resolve `href` against the OPF's directory inside the archive,
/// normalizing `.` / `..` segments.
fn resolve_href(opf_dir: &str, href: &str) -> String {
    let href = percent_decode(href.split('#').next().unwrap_or(href));
    let mut parts: Vec<&str> = Vec::new();
    if !opf_dir.is_empty() {
        parts.extend(opf_dir.split('/').filter(|p| !p.is_empty()));
    }
    for seg in href.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// Strip an XHTML document to readable text: content before `<body>`
/// (head, title, styles) is skipped; block-level closers become line
/// breaks — the same rules as the shell's `xml_to_text`.
pub fn xhtml_to_text(xhtml: &str) -> String {
    let body = match xhtml.to_lowercase().find("<body") {
        Some(pos) => match xhtml[pos..].find('>') {
            Some(gt) => &xhtml[pos + gt + 1..],
            None => xhtml,
        },
        None => xhtml,
    };
    let mut out = String::with_capacity(body.len() / 2);
    let mut in_tag = false;
    let mut tag = String::new();
    let mut skip_depth = 0usize; // inside <script>/<style>
    for ch in body.chars() {
        match ch {
            '<' => {
                in_tag = true;
                tag.clear();
            }
            '>' => {
                in_tag = false;
                let t = tag.trim_start();
                let name = t
                    .trim_start_matches('/')
                    .split(|c: char| c.is_whitespace() || c == '/')
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if name == "script" || name == "style" {
                    if t.starts_with('/') {
                        skip_depth = skip_depth.saturating_sub(1);
                    } else if !t.ends_with('/') {
                        skip_depth += 1;
                    }
                    continue;
                }
                if skip_depth > 0 {
                    continue;
                }
                if t.starts_with("/p")
                    || t.starts_with("/h")
                    || t.starts_with("/li")
                    || t.starts_with("/tr")
                    || t.starts_with("/div")
                    || t.starts_with("/blockquote")
                    || t.starts_with("br")
                {
                    out.push('\n');
                }
            }
            _ => {
                if in_tag {
                    tag.push(ch);
                } else if skip_depth == 0 {
                    out.push(ch);
                }
            }
        }
    }
    unescape(&out)
}

// ---------------------------------------------------------------------------
// The decoder.
// ---------------------------------------------------------------------------

struct ManifestItem {
    href: String,
    media_type: String,
    properties: String,
}

/// Decode an EPUB from its raw bytes. Never fails hard: worst case is
/// a preview carrying only a plain-language note.
pub fn decode_epub(data: &[u8]) -> EpubPreview {
    let mut out = EpubPreview::default();
    let Some(zip) = ZipArchive::open(data) else {
        out.note = Some(
            "This file isn't a readable EPUB (the book archive couldn't be opened).".into(),
        );
        return out;
    };

    // DRM: encryption.xml encrypting anything but fonts means the
    // content itself is locked. Font-only entries are obfuscation.
    if let Some(enc) = zip.read_text("META-INF/encryption.xml") {
        let locked_content = iter_tags(&enc, "CipherReference").iter().any(|t| {
            let uri = attr(t, "URI").unwrap_or("");
            let lower = uri.to_lowercase();
            !(lower.ends_with(".ttf")
                || lower.ends_with(".otf")
                || lower.ends_with(".woff")
                || lower.ends_with(".woff2"))
        });
        if locked_content {
            out.drm_protected = true;
            out.note = Some(
                "This book is protected by DRM, so Fable can't show its cover or pages. \
                 It will still open in your usual reader app."
                    .into(),
            );
        }
    }

    // container.xml → OPF path (default: the conventional location).
    let opf_path = zip
        .read_text("META-INF/container.xml")
        .and_then(|c| {
            iter_tags(&c, "rootfile")
                .into_iter()
                .find_map(|t| attr(t, "full-path").map(str::to_owned))
        })
        .unwrap_or_else(|| "OEBPS/content.opf".to_string());
    let Some(opf) = zip.read_text(&opf_path) else {
        if out.note.is_none() {
            out.note = Some(
                "This EPUB is missing its book index (the OPF file), so no preview can be built."
                    .into(),
            );
        }
        return out;
    };
    let opf_dir = opf_path
        .rsplit_once('/')
        .map(|(d, _)| d.to_string())
        .unwrap_or_default();

    out.title = tag_text(&opf, "dc:title");
    out.author = tag_text(&opf, "dc:creator");

    // Manifest: id → item.
    let mut manifest: HashMap<String, ManifestItem> = HashMap::new();
    for t in iter_tags(&opf, "item") {
        if let (Some(id), Some(href)) = (attr(t, "id"), attr(t, "href")) {
            manifest.insert(
                id.to_string(),
                ManifestItem {
                    href: href.to_string(),
                    media_type: attr(t, "media-type").unwrap_or("").to_string(),
                    properties: attr(t, "properties").unwrap_or("").to_string(),
                },
            );
        }
    }

    // Spine: reading order (itemrefs whose linear != "no").
    let spine_ids: Vec<String> = iter_tags(&opf, "itemref")
        .into_iter()
        .filter(|t| attr(t, "linear") != Some("no"))
        .filter_map(|t| attr(t, "idref").map(str::to_owned))
        .collect();
    out.spine_count = spine_ids.len();

    if !out.drm_protected {
        out.cover = find_cover(&zip, &opf, &opf_dir, &manifest);
        out.excerpt = build_excerpt(&zip, &opf_dir, &manifest, &spine_ids);
        if out.excerpt.trim().is_empty() && out.note.is_none() {
            out.note = Some("No readable text was found in this book's opening pages.".into());
        }
    }
    out
}

fn find_cover(
    zip: &ZipArchive,
    opf: &str,
    opf_dir: &str,
    manifest: &HashMap<String, ManifestItem>,
) -> Option<EpubCover> {
    // 1. EPUB 3: manifest item flagged properties="cover-image".
    let mut candidate: Option<&ManifestItem> = manifest
        .values()
        .find(|i| i.properties.split_whitespace().any(|p| p == "cover-image"));
    // 2. EPUB 2: <meta name="cover" content="id"/>.
    if candidate.is_none() {
        if let Some(meta) = iter_tags(opf, "meta")
            .into_iter()
            .find(|t| attr(t, "name") == Some("cover"))
        {
            if let Some(id) = attr(meta, "content") {
                candidate = manifest.get(id);
            }
        }
    }
    // 3. Any image whose id or href says "cover".
    if candidate.is_none() {
        candidate = manifest.values().find(|i| {
            i.media_type.starts_with("image/")
                && (i.href.to_lowercase().contains("cover"))
        });
    }
    if let Some(item) = candidate {
        let path = resolve_href(opf_dir, &item.href);
        if let Some(entry) = zip.get(&path) {
            if let Some(data) = zip.read(entry) {
                return Some(EpubCover {
                    media_type: item.media_type.clone(),
                    data,
                });
            }
        }
    }
    // 4. Guide reference to a cover page → its first image.
    if let Some(r) = iter_tags(opf, "reference")
        .into_iter()
        .find(|t| attr(t, "type") == Some("cover"))
    {
        if let Some(href) = attr(r, "href") {
            let page_path = resolve_href(opf_dir, href);
            if let Some(page) = zip.read_text(&page_path) {
                let page_dir = page_path
                    .rsplit_once('/')
                    .map(|(d, _)| d.to_string())
                    .unwrap_or_default();
                for t in iter_tags(&page, "img")
                    .into_iter()
                    .chain(iter_tags(&page, "image"))
                {
                    let src = attr(t, "src").or_else(|| attr(t, "xlink:href"));
                    if let Some(src) = src {
                        let img_path = resolve_href(&page_dir, src);
                        if let Some(entry) = zip.get(&img_path) {
                            if let Some(data) = zip.read(entry) {
                                let media_type = manifest
                                    .values()
                                    .find(|i| resolve_href(opf_dir, &i.href) == img_path)
                                    .map(|i| i.media_type.clone())
                                    .unwrap_or_else(|| "image/jpeg".to_string());
                                return Some(EpubCover { media_type, data });
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

fn build_excerpt(
    zip: &ZipArchive,
    opf_dir: &str,
    manifest: &HashMap<String, ManifestItem>,
    spine_ids: &[String],
) -> String {
    let mut text = String::new();
    for id in spine_ids {
        if text.chars().count() >= MAX_EXCERPT_CHARS {
            break;
        }
        let Some(item) = manifest.get(id) else {
            continue;
        };
        if !item.media_type.contains("html") && !item.media_type.contains("xml") {
            continue;
        }
        let path = resolve_href(opf_dir, &item.href);
        let Some(xhtml) = zip.read_text(&path) else {
            continue;
        };
        let plain = xhtml_to_text(&xhtml);
        let plain = plain.trim();
        if !plain.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(plain);
        }
    }
    text.chars().take(MAX_EXCERPT_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // -- tiny ZIP writer for fixtures: local headers + central dir --
    struct FixtureEntry {
        name: &'static str,
        data: Vec<u8>,
        deflate: bool,
    }

    fn crc32(data: &[u8]) -> u32 {
        // Small table-free CRC32 (fixtures only).
        let mut crc = 0xFFFF_FFFFu32;
        for &b in data {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    fn zip_fixture(entries: &[FixtureEntry]) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        let mut central: Vec<u8> = Vec::new();
        for e in entries {
            let offset = out.len() as u32;
            let compressed = if e.deflate {
                let mut enc = flate2::write::DeflateEncoder::new(
                    Vec::new(),
                    flate2::Compression::default(),
                );
                enc.write_all(&e.data).unwrap();
                enc.finish().unwrap()
            } else {
                e.data.clone()
            };
            let method: u16 = if e.deflate { 8 } else { 0 };
            let crc = crc32(&e.data);
            // local header
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&method.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
            out.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(e.name.as_bytes());
            out.extend_from_slice(&compressed);
            // central record
            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&method.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
            central.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(e.name.as_bytes());
        }
        let cd_start = out.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd_start.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    const CONTAINER: &str = r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;

    const OPF: &str = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="bid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="bid">urn:fixture:1</dc:identifier>
    <dc:title>The Fixture Book</dc:title>
    <dc:creator>Jane Tester</dc:creator>
  </metadata>
  <manifest>
    <item id="cover" href="images/cover.jpg" media-type="image/jpeg" properties="cover-image"/>
    <item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
    <item id="ch2" href="ch2.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="ch1"/><itemref idref="ch2"/></spine>
</package>"#;

    const CH1: &str = r#"<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Ch 1 HEAD TEXT</title></head>
<body><h1>Chapter One</h1><p>It was a bright cold day &amp; the clocks were striking thirteen.</p></body></html>"#;
    const CH2: &str = r#"<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Ch 2</title></head>
<body><h1>Chapter Two</h1><p>The second chapter follows the first.</p></body></html>"#;

    fn book_fixture() -> Vec<u8> {
        // NB: ch2 deliberately sits BEFORE ch1 in archive order —
        // the excerpt must still open with chapter one (spine order).
        zip_fixture(&[
            FixtureEntry { name: "mimetype", data: b"application/epub+zip".to_vec(), deflate: false },
            FixtureEntry { name: "META-INF/container.xml", data: CONTAINER.into(), deflate: true },
            FixtureEntry { name: "OEBPS/content.opf", data: OPF.into(), deflate: true },
            FixtureEntry { name: "OEBPS/images/cover.jpg", data: vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3], deflate: false },
            FixtureEntry { name: "OEBPS/ch2.xhtml", data: CH2.into(), deflate: true },
            FixtureEntry { name: "OEBPS/ch1.xhtml", data: CH1.into(), deflate: true },
        ])
    }

    #[test]
    fn decodes_metadata_cover_and_spine_ordered_excerpt() {
        let p = decode_epub(&book_fixture());
        assert_eq!(p.title.as_deref(), Some("The Fixture Book"));
        assert_eq!(p.author.as_deref(), Some("Jane Tester"));
        assert_eq!(p.spine_count, 2);
        assert!(!p.drm_protected);
        let cover = p.cover.expect("cover extracted");
        assert_eq!(cover.media_type, "image/jpeg");
        assert_eq!(cover.data, vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3]);
        // Spine order, not archive order: chapter one leads.
        let one = p.excerpt.find("Chapter One").expect("ch1 in excerpt");
        let two = p.excerpt.find("Chapter Two").expect("ch2 in excerpt");
        assert!(one < two, "excerpt out of spine order: {:?}", p.excerpt);
        // Head/title text never leaks into the excerpt.
        assert!(!p.excerpt.contains("HEAD TEXT"));
        // Entities unescaped.
        assert!(p.excerpt.contains("bright cold day & the clocks"));
    }

    #[test]
    fn drm_book_reports_protection_and_withholds_content() {
        let enc = r#"<?xml version="1.0"?>
<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container"
  xmlns:enc="http://www.w3.org/2001/04/xmlenc#">
  <enc:EncryptedData><enc:CipherData><enc:CipherReference URI="OEBPS/ch1.xhtml"/></enc:CipherData></enc:EncryptedData>
</encryption>"#;
        let mut entries = vec![
            FixtureEntry { name: "mimetype", data: b"application/epub+zip".to_vec(), deflate: false },
            FixtureEntry { name: "META-INF/container.xml", data: CONTAINER.into(), deflate: false },
            FixtureEntry { name: "META-INF/encryption.xml", data: enc.into(), deflate: false },
            FixtureEntry { name: "OEBPS/content.opf", data: OPF.into(), deflate: false },
            FixtureEntry { name: "OEBPS/ch1.xhtml", data: CH1.into(), deflate: false },
            FixtureEntry { name: "OEBPS/ch2.xhtml", data: CH2.into(), deflate: false },
        ];
        entries.push(FixtureEntry { name: "OEBPS/images/cover.jpg", data: vec![9, 9], deflate: false });
        let p = decode_epub(&zip_fixture(&entries));
        assert!(p.drm_protected);
        assert!(p.excerpt.is_empty());
        assert!(p.cover.is_none());
        assert!(p.note.as_deref().unwrap_or("").contains("DRM"));
        // Metadata is not encrypted in real DRM books either.
        assert_eq!(p.title.as_deref(), Some("The Fixture Book"));
    }

    #[test]
    fn font_only_encryption_is_not_drm() {
        let enc = r#"<?xml version="1.0"?>
<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container"
  xmlns:enc="http://www.w3.org/2001/04/xmlenc#">
  <enc:EncryptedData><enc:CipherData><enc:CipherReference URI="OEBPS/fonts/serif.ttf"/></enc:CipherData></enc:EncryptedData>
</encryption>"#;
        let mut fixture = book_fixture_entries();
        fixture.push(FixtureEntry { name: "META-INF/encryption.xml", data: enc.into(), deflate: false });
        let p = decode_epub(&zip_fixture(&fixture));
        assert!(!p.drm_protected);
        assert!(p.excerpt.contains("Chapter One"));
    }

    fn book_fixture_entries() -> Vec<FixtureEntry> {
        vec![
            FixtureEntry { name: "mimetype", data: b"application/epub+zip".to_vec(), deflate: false },
            FixtureEntry { name: "META-INF/container.xml", data: CONTAINER.into(), deflate: true },
            FixtureEntry { name: "OEBPS/content.opf", data: OPF.into(), deflate: true },
            FixtureEntry { name: "OEBPS/images/cover.jpg", data: vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3], deflate: false },
            FixtureEntry { name: "OEBPS/ch2.xhtml", data: CH2.into(), deflate: true },
            FixtureEntry { name: "OEBPS/ch1.xhtml", data: CH1.into(), deflate: true },
        ]
    }

    #[test]
    fn garbage_and_truncated_files_degrade_to_notes() {
        let p = decode_epub(b"this is not a zip at all");
        assert!(p.note.is_some());
        assert!(p.cover.is_none());
        let truncated = decode_epub(&book_fixture()[..40]);
        assert!(truncated.note.is_some());
    }

    #[test]
    fn epub2_meta_cover_fallback() {
        let opf2 = OPF
            .replace(r#" properties="cover-image""#, "")
            .replace(
                "<dc:creator>Jane Tester</dc:creator>",
                "<dc:creator>Jane Tester</dc:creator><meta name=\"cover\" content=\"cover\"/>",
            );
        let mut entries = book_fixture_entries();
        entries[2].data = opf2.into_bytes();
        let p = decode_epub(&zip_fixture(&entries));
        let cover = p.cover.expect("meta cover found");
        assert_eq!(cover.data, vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3]);
    }
}
