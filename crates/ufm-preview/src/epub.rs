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

// ---------------------------------------------------------------------------
// The reader's engine (Phase 6): the whole book, chapter by chapter, in
// spine order. Same failure philosophy as the preview — a note, never
// an error — and the preview decoder above is deliberately untouched.
// ---------------------------------------------------------------------------

/// Cap on one chapter's extracted text (reader material, not a book's
/// worth of runaway markup).
const MAX_CHAPTER_CHARS: usize = 256_000;
/// Cap on the whole book's extracted text.
const MAX_BOOK_CHARS: usize = 4_000_000;
/// Cap on spine documents walked (hostile-archive rail).
const MAX_SPINE_DOCS: usize = 2_000;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct EpubChapter {
    pub title: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct EpubBook {
    pub title: Option<String>,
    pub author: Option<String>,
    /// Chapters in reading (spine) order. Empty when the book can't be
    /// read at all (see `note`) or is DRM-locked.
    pub chapters: Vec<EpubChapter>,
    pub drm_protected: bool,
    /// Plain-language reason the book is limited, when it is.
    pub note: Option<String>,
}

/// A chapter's display title: the first visible heading, else the
/// document `<title>`, else the first line of its text.
fn chapter_title(xhtml: &str, text: &str) -> Option<String> {
    for tag in ["h1", "h2", "title"] {
        if let Some(t) = tag_text(xhtml, tag) {
            let t = collapse_ws(&xhtml_to_text(&t));
            if !t.is_empty() {
                return Some(t.chars().take(120).collect());
            }
        }
    }
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(120).collect())
}

/// Decode a whole EPUB for the reader. Never fails hard: worst case
/// is a book with no chapters and a plain-language note.
pub fn decode_epub_book(data: &[u8]) -> EpubBook {
    let mut out = EpubBook::default();
    let Some(zip) = ZipArchive::open(data) else {
        out.note = Some(
            "This file isn't a readable EPUB (the book archive couldn't be opened).".into(),
        );
        return out;
    };

    // DRM: same rule as the preview — encryption.xml locking anything
    // but fonts locks the content itself.
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
                "This book is protected by DRM, so Fable can't open it in the reader. \
                 It will still open in your usual reader app."
                    .into(),
            );
        }
    }

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
                "This EPUB is missing its book index (the OPF file), so it can't be read."
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

    if out.drm_protected {
        return out;
    }

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
    let spine_ids: Vec<String> = iter_tags(&opf, "itemref")
        .into_iter()
        .filter(|t| attr(t, "linear") != Some("no"))
        .filter_map(|t| attr(t, "idref").map(str::to_owned))
        .take(MAX_SPINE_DOCS)
        .collect();

    let mut total_chars = 0usize;
    let mut truncated = false;
    for id in &spine_ids {
        let Some(item) = manifest.get(id) else {
            continue;
        };
        if !item.media_type.contains("html") && !item.media_type.contains("xml") {
            continue;
        }
        let path = resolve_href(&opf_dir, &item.href);
        let Some(xhtml) = zip.read_text(&path) else {
            continue;
        };
        let text = xhtml_to_text(&xhtml);
        let text = text.trim();
        let title = chapter_title(&xhtml, text);
        let room = MAX_BOOK_CHARS.saturating_sub(total_chars);
        let capped: String = text
            .chars()
            .take(MAX_CHAPTER_CHARS.min(room))
            .collect();
        if capped.chars().count() < text.chars().count() {
            truncated = true;
        }
        total_chars += capped.chars().count();
        out.chapters.push(EpubChapter {
            title,
            text: capped,
        });
    }
    if out.chapters.is_empty() && out.note.is_none() {
        out.note = Some("No readable chapters were found in this book.".into());
    } else if truncated && out.note.is_none() {
        out.note = Some(
            "This book is very large, so the reader loaded only its first portion.".into(),
        );
    }
    out
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

// ---------------------------------------------------------------------------
// Full book metadata (Ebook P1 — "The Library", 2026-10-17): the whole
// Dublin Core + series surface of the OPF, parsed for the catalog.
// The preview/reader decoders above deliberately stay untouched; this
// is an additive API the indexer consumes at identify time.
// ---------------------------------------------------------------------------

/// Everything the OPF says about a book, beyond the preview's
/// title/author pair. Series data arrives in two real-world dialects:
/// EPUB 3 `belongs-to-collection` (with `group-position` refinements)
/// and Calibre's `calibre:series` / `calibre:series_index` meta pair —
/// both are read, EPUB 3 first.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct EpubMetadata {
    pub title: Option<String>,
    /// Every `dc:creator`, in document order (deduplicated).
    pub authors: Vec<String>,
    pub series: Option<String>,
    /// Position within the series ("3", "3.0", "0.5" all occur).
    pub series_index: Option<f64>,
    pub publisher: Option<String>,
    /// The raw `dc:date` string ("2011", "2011-08-23", …), unparsed —
    /// the year extraction is [`published_year`].
    pub published_date: Option<String>,
    pub language: Option<String>,
    pub description: Option<String>,
    /// Every `dc:subject`, in document order (deduplicated).
    pub subjects: Vec<String>,
    /// The ISBN when any identifier declares itself one
    /// (`opf:scheme="ISBN"` or a `urn:isbn:` value).
    pub isbn: Option<String>,
    /// The package's unique identifier (the `dc:identifier` whose id
    /// matches `unique-identifier`), else the first identifier.
    pub identifier: Option<String>,
    /// Linear spine documents (chapters) — the reader's denominator.
    pub spine_count: usize,
}

/// The year a raw `dc:date` names: the leading 4-digit group, when it
/// is a plausible year. "2011-08-23" → 2011; "August 2011" → none.
pub fn published_year(raw: &str) -> Option<i64> {
    let head: String = raw.trim().chars().take(4).collect();
    if head.len() == 4 && head.chars().all(|c| c.is_ascii_digit()) {
        let y: i64 = head.parse().ok()?;
        if (1000..=2999).contains(&y) {
            return Some(y);
        }
    }
    None
}

/// Every occurrence of `<tag …>text</tag>` as (opening-tag attributes,
/// inner text) — the multi-value companion to [`tag_text`], for the
/// repeated Dublin Core elements (`dc:creator`, `dc:subject`,
/// `dc:identifier`) and text-bearing `meta` refinements. Self-closing
/// tags yield an empty text.
fn elements(xml: &str, tag: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let open = format!("<{tag}");
    let mut rest = xml;
    while let Some(pos) = rest.find(&open) {
        let after = &rest[pos + open.len()..];
        let next_ok = matches!(after.chars().next(), Some(c) if c == '>' || c == '/' || c.is_whitespace());
        if !next_ok {
            rest = after;
            continue;
        }
        let Some(gt) = after.find('>') else { break };
        let attrs = after[..gt].to_string();
        if attrs.trim_end().ends_with('/') {
            out.push((attrs, String::new()));
            rest = &after[gt + 1..];
            continue;
        }
        let close = format!("</{tag}>");
        let inner_start = gt + 1;
        match after[inner_start..].find(&close) {
            Some(end) => {
                let text = collapse_ws(&unescape(&after[inner_start..inner_start + end]));
                out.push((attrs, text));
                rest = &after[inner_start + end + close.len()..];
            }
            None => break,
        }
    }
    out
}

/// Inner texts of every `<tag …>…</tag>`, in document order.
fn tag_text_all(xml: &str, tag: &str) -> Vec<String> {
    elements(xml, tag)
        .into_iter()
        .map(|(_, t)| t)
        .filter(|t| !t.is_empty())
        .collect()
}

fn dedupe(values: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in values {
        if !out.contains(&v) {
            out.push(v);
        }
    }
    out
}

fn parse_series_index(raw: &str) -> Option<f64> {
    raw.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Parse the full metadata surface out of an OPF document. Pure and
/// total: missing data is `None` / empty, never an error.
pub fn parse_opf_metadata(opf: &str) -> EpubMetadata {
    let mut out = EpubMetadata::default();
    out.title = tag_text(opf, "dc:title");
    out.authors = dedupe(tag_text_all(opf, "dc:creator"));
    out.publisher = tag_text(opf, "dc:publisher");
    out.language = tag_text(opf, "dc:language");
    out.subjects = dedupe(tag_text_all(opf, "dc:subject"));

    // dc:date: prefer the explicit publication event when several
    // dates ride along (OPF 2 modification dates are common).
    let dates = elements(opf, "dc:date");
    out.published_date = dates
        .iter()
        .find(|(a, _)| {
            a.to_ascii_lowercase().contains("opf:event=\"publication\"")
                || a.to_ascii_lowercase().contains("opf:event='publication'")
        })
        .or_else(|| dates.first())
        .map(|(_, t)| t.clone())
        .filter(|t| !t.is_empty());

    // Description may carry markup — strip it to readable text.
    out.description = tag_text(opf, "dc:description").map(|d| {
        let plain = collapse_ws(&xhtml_to_text(&d));
        plain.chars().take(2000).collect::<String>()
    });

    // Identifiers: the primary is the one the package names via
    // unique-identifier; the ISBN is whichever declares itself one.
    let unique_id = iter_tags(opf, "package")
        .into_iter()
        .find_map(|t| attr(t, "unique-identifier").map(str::to_owned));
    let identifiers = elements(opf, "dc:identifier");
    let mut first_identifier: Option<String> = None;
    for (attrs, text) in &identifiers {
        if text.is_empty() {
            continue;
        }
        if first_identifier.is_none() {
            first_identifier = Some(text.clone());
        }
        if let (Some(want), Some(id)) = (&unique_id, attr(attrs, "id")) {
            if id == want {
                out.identifier = Some(text.clone());
            }
        }
        let scheme = attr(attrs, "opf:scheme").unwrap_or("");
        if scheme.eq_ignore_ascii_case("isbn") {
            out.isbn = Some(text.clone());
        } else if text.len() > 9 && text[..9].eq_ignore_ascii_case("urn:isbn:") {
            out.isbn = Some(text[9..].to_string());
        }
    }
    if out.identifier.is_none() {
        out.identifier = first_identifier;
    }

    // Series, EPUB 3 dialect: <belongs-to-collection id="c">Name</…>
    // refined by <meta refines="#c" property="group-position">N</…>.
    let metas = elements(opf, "meta");
    let group_position = |target: &str| -> Option<f64> {
        metas
            .iter()
            .find(|(a, _)| {
                attr(a, "refines") == Some(target)
                    && attr(a, "property") == Some("group-position")
            })
            .and_then(|(_, t)| parse_series_index(t))
    };
    let mut series: Option<(String, Option<f64>)> = None;
    for (attrs, text) in elements(opf, "belongs-to-collection") {
        if text.is_empty() {
            continue;
        }
        let pos = attr(&attrs, "id").and_then(|id| group_position(&format!("#{id}")));
        // Prefer a collection that carries a position; keep the
        // first named one otherwise.
        if series.is_none() || (series.as_ref().map(|(_, p)| p.is_none()).unwrap_or(false) && pos.is_some()) {
            series = Some((text, pos));
        }
    }
    if series.is_none() {
        // Text-form meta (early EPUB 3): <meta property="belongs-to-collection">Name</meta>.
        for (attrs, text) in &metas {
            if attr(attrs, "property") == Some("belongs-to-collection") && !text.is_empty() {
                let pos = attr(attrs, "id").and_then(|id| group_position(&format!("#{id}")));
                series = Some((text.clone(), pos));
                break;
            }
        }
    }
    if series.is_none() {
        // Calibre dialect: name + index as content attributes.
        let name = metas.iter().find_map(|(a, _)| {
            if attr(a, "name") == Some("calibre:series") {
                attr(a, "content").map(|c| collapse_ws(&unescape(c)))
            } else {
                None
            }
        });
        if let Some(name) = name.filter(|n| !n.is_empty()) {
            let pos = metas.iter().find_map(|(a, _)| {
                if attr(a, "name") == Some("calibre:series_index") {
                    attr(a, "content").and_then(parse_series_index)
                } else {
                    None
                }
            });
            series = Some((name, pos));
        }
    }
    if let Some((name, pos)) = series {
        out.series = Some(name);
        out.series_index = pos;
    }
    out
}

/// Decode ONLY a book's metadata (+ spine count) from its raw bytes —
/// the indexer's entry point: no cover extraction, no excerpt walk.
/// `None` when the archive or its OPF can't be read at all. Metadata
/// reads even on DRM books (it isn't the encrypted payload).
pub fn decode_epub_metadata(data: &[u8]) -> Option<EpubMetadata> {
    let zip = ZipArchive::open(data)?;
    let opf_path = zip
        .read_text("META-INF/container.xml")
        .and_then(|c| {
            iter_tags(&c, "rootfile")
                .into_iter()
                .find_map(|t| attr(t, "full-path").map(str::to_owned))
        })
        .unwrap_or_else(|| "OEBPS/content.opf".to_string());
    let opf = zip.read_text(&opf_path)?;
    let mut meta = parse_opf_metadata(&opf);
    meta.spine_count = iter_tags(&opf, "itemref")
        .into_iter()
        .filter(|t| attr(t, "linear") != Some("no"))
        .count();
    Some(meta)
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
    fn book_decodes_chapters_in_spine_order() {
        let b = decode_epub_book(&book_fixture());
        assert_eq!(b.title.as_deref(), Some("The Fixture Book"));
        assert_eq!(b.author.as_deref(), Some("Jane Tester"));
        assert!(!b.drm_protected);
        assert_eq!(b.chapters.len(), 2, "chapters: {:?}", b.chapters);
        // Spine order despite archive order (ch2 stored first).
        assert_eq!(b.chapters[0].title.as_deref(), Some("Chapter One"));
        assert!(b.chapters[0].text.contains("bright cold day & the clocks"));
        assert!(!b.chapters[0].text.contains("HEAD TEXT"));
        assert_eq!(b.chapters[1].title.as_deref(), Some("Chapter Two"));
        assert!(b.chapters[1].text.contains("second chapter follows"));
    }

    #[test]
    fn book_drm_withholds_chapters_but_keeps_metadata() {
        let enc = r#"<?xml version="1.0"?>
<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container"
  xmlns:enc="http://www.w3.org/2001/04/xmlenc#">
  <enc:EncryptedData><enc:CipherData><enc:CipherReference URI="OEBPS/ch1.xhtml"/></enc:CipherData></enc:EncryptedData>
</encryption>"#;
        let entries = vec![
            FixtureEntry { name: "mimetype", data: b"application/epub+zip".to_vec(), deflate: false },
            FixtureEntry { name: "META-INF/container.xml", data: CONTAINER.into(), deflate: false },
            FixtureEntry { name: "META-INF/encryption.xml", data: enc.into(), deflate: false },
            FixtureEntry { name: "OEBPS/content.opf", data: OPF.into(), deflate: false },
            FixtureEntry { name: "OEBPS/ch1.xhtml", data: CH1.into(), deflate: false },
            FixtureEntry { name: "OEBPS/ch2.xhtml", data: CH2.into(), deflate: false },
        ];
        let b = decode_epub_book(&zip_fixture(&entries));
        assert!(b.drm_protected);
        assert!(b.chapters.is_empty());
        assert_eq!(b.title.as_deref(), Some("The Fixture Book"));
        assert!(b.note.as_deref().unwrap_or("").contains("DRM"));
    }

    #[test]
    fn book_garbage_is_a_note_not_an_error() {
        let b = decode_epub_book(b"this is not a zip at all");
        assert!(b.chapters.is_empty());
        assert!(b.note.is_some());
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

    // -- Metadata (Ebook P1) ----------------------------------------

    const OPF_RICH: &str = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" xmlns:opf="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="bid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="bid" opf:scheme="ISBN">978-0-13-468599-1</dc:identifier>
    <dc:identifier>urn:uuid:12345</dc:identifier>
    <dc:title>Café &amp; Crème: A Tale</dc:title>
    <dc:creator>Søren Åberg</dc:creator>
    <dc:creator>Second Author</dc:creator>
    <dc:creator>Søren Åberg</dc:creator>
    <dc:publisher>Fixture House</dc:publisher>
    <dc:date opf:event="publication">2011-08-23</dc:date>
    <dc:date>2020-01-01</dc:date>
    <dc:language>en</dc:language>
    <dc:description>&lt;p&gt;A &lt;b&gt;rich&lt;/b&gt; description.&lt;/p&gt;</dc:description>
    <dc:subject>Fiction</dc:subject>
    <dc:subject>Sea stories</dc:subject>
    <dc:subject>Fiction</dc:subject>
    <meta name="calibre:series" content="The Alloy Wars"/>
    <meta name="calibre:series_index" content="3.0"/>
  </metadata>
  <manifest><item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/></manifest>
  <spine><itemref idref="ch1"/></spine>
</package>"#;

    #[test]
    fn metadata_reads_full_dublin_core_and_calibre_series() {
        let m = parse_opf_metadata(OPF_RICH);
        assert_eq!(m.title.as_deref(), Some("Café & Crème: A Tale"));
        // Multi-author, deduplicated, document order kept.
        assert_eq!(m.authors, vec!["Søren Åberg".to_string(), "Second Author".to_string()]);
        assert_eq!(m.publisher.as_deref(), Some("Fixture House"));
        // The publication-event date wins over the bare second date.
        assert_eq!(m.published_date.as_deref(), Some("2011-08-23"));
        assert_eq!(published_year("2011-08-23"), Some(2011));
        assert_eq!(m.language.as_deref(), Some("en"));
        assert_eq!(m.description.as_deref(), Some("A rich description."));
        assert_eq!(m.subjects, vec!["Fiction".to_string(), "Sea stories".to_string()]);
        assert_eq!(m.isbn.as_deref(), Some("978-0-13-468599-1"));
        // Primary identifier = the unique-identifier target, not the uuid.
        assert_eq!(m.identifier.as_deref(), Some("978-0-13-468599-1"));
        assert_eq!(m.series.as_deref(), Some("The Alloy Wars"));
        assert_eq!(m.series_index, Some(3.0));
    }

    #[test]
    fn metadata_reads_epub3_belongs_to_collection_with_group_position() {
        let opf = r##"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="bid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="bid">urn:isbn:9781234567890</dc:identifier>
    <dc:title>Second Verse</dc:title>
    <dc:creator>Jane Tester</dc:creator>
    <belongs-to-collection id="c01">The Long Shelf</belongs-to-collection>
    <meta refines="#c01" property="collection-type">series</meta>
    <meta refines="#c01" property="group-position">2</meta>
  </metadata>
  <manifest><item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/></manifest>
  <spine><itemref idref="ch1"/></spine>
</package>"##;
        let m = parse_opf_metadata(opf);
        assert_eq!(m.series.as_deref(), Some("The Long Shelf"));
        assert_eq!(m.series_index, Some(2.0));
        assert_eq!(m.isbn.as_deref(), Some("9781234567890"));
        assert_eq!(m.identifier.as_deref(), Some("urn:isbn:9781234567890"));
    }

    #[test]
    fn metadata_epub3_wins_over_calibre_when_both_present() {
        let opf = OPF_RICH.replace(
            "<meta name=\"calibre:series\" content=\"The Alloy Wars\"/>",
            "<belongs-to-collection id=\"c9\">EPUB3 Series</belongs-to-collection><meta refines=\"#c9\" property=\"group-position\">0.5</meta><meta name=\"calibre:series\" content=\"The Alloy Wars\"/>",
        );
        let m = parse_opf_metadata(&opf);
        assert_eq!(m.series.as_deref(), Some("EPUB3 Series"));
        assert_eq!(m.series_index, Some(0.5));
    }

    #[test]
    fn metadata_sparse_opf_is_all_none_not_garbage() {
        let m = parse_opf_metadata(OPF);
        assert_eq!(m.title.as_deref(), Some("The Fixture Book"));
        assert_eq!(m.authors, vec!["Jane Tester".to_string()]);
        assert!(m.series.is_none());
        assert!(m.series_index.is_none());
        assert!(m.publisher.is_none());
        assert!(m.subjects.is_empty());
        assert_eq!(m.identifier.as_deref(), Some("urn:fixture:1"));
        assert!(m.isbn.is_none());
    }

    #[test]
    fn metadata_from_bytes_counts_spine_and_survives_garbage() {
        let m = decode_epub_metadata(&book_fixture()).expect("metadata decodes");
        assert_eq!(m.title.as_deref(), Some("The Fixture Book"));
        assert_eq!(m.spine_count, 2);
        assert!(decode_epub_metadata(b"not a zip").is_none());
        assert!(decode_epub_metadata(&book_fixture()[..30]).is_none());
    }

    #[test]
    fn published_year_rejects_non_years() {
        assert_eq!(published_year("2011"), Some(2011));
        assert_eq!(published_year("  1999-05-01 "), Some(1999));
        assert_eq!(published_year("August 2011"), None);
        assert_eq!(published_year(""), None);
        assert_eq!(published_year("0999"), None);
    }
}
