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

// ---------------------------------------------------------------------------
// The Reading Room (Ebook P2 — 2026-10-17): rich chapter decode.
//
// `decode_epub_book` above flattens a chapter to text — right for a
// preview, wrong for a reading surface: publisher CSS, images,
// footnotes, and internal links all die in the flattening. This
// additive API richly decodes ONE chapter (the one being read): its
// XHTML is rewritten by a streaming sanitizer (quick-xml — already
// the workspace's XML reader), its publisher CSS is collected,
// sanitized, and scoped under the reader's `.reader-pub` container,
// images are inlined as data URLs, and internal links are resolved
// against the spine into `data-fable-link` positions the UI
// navigates (the webview never sees a live book href). The chapter
// TEXT this decode reports comes from the same `xhtml_to_text` walk
// as the plain decode, so the text surface — and any future
// read-aloud consumer — sees exactly the P1 chapters, in spine
// order. Everything above this section is untouched.
// ---------------------------------------------------------------------------

/// Cap on one inlined media member (decoded bytes).
const MAX_RICH_MEDIA_MEMBER: usize = 8 * 1024 * 1024;
/// Cap on all inlined media in one chapter (images + CSS art/fonts).
const MAX_RICH_MEDIA_TOTAL: usize = 24 * 1024 * 1024;
/// Cap on one chapter's sanitized HTML fragment.
const MAX_RICH_HTML_CHARS: usize = 1_500_000;
/// Cap on one chapter's scoped CSS.
const MAX_RICH_CSS_CHARS: usize = 256_000;
/// Cap on @import chains followed inside the archive.
const MAX_RICH_IMPORT_DEPTH: usize = 3;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct EpubChapterRich {
    /// Sanitized XHTML fragment for the chapter, or None when the
    /// chapter couldn't be faithfully rewritten (malformed markup,
    /// over a cap) — the caller falls back to the chapter's text.
    pub html: Option<String>,
    /// Publisher CSS for the chapter: sanitized and scoped under
    /// `.reader-pub`. None when the chapter carries no usable CSS.
    pub css: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct EpubBookRich {
    pub title: Option<String>,
    pub author: Option<String>,
    /// Chapters in reading (spine) order — identical to
    /// `decode_epub_book`'s chapters (same walk, same caps).
    pub chapters: Vec<EpubChapter>,
    /// Parallel to `chapters`; an entry is Some only for the
    /// chapter the caller asked to richly decode (media inlining
    /// is priced per chapter, not per book).
    pub rich: Vec<Option<EpubChapterRich>>,
    pub drm_protected: bool,
    /// Plain-language reason the book is limited, when it is.
    pub note: Option<String>,
}

/// Archive paths named by `META-INF/encryption.xml` (percent-decoded,
/// leading slash stripped) — the font-face gate reads this: a font
/// under encryption is obfuscated or locked, never inlined.
fn encrypted_paths(zip: &ZipArchive) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    if let Some(enc) = zip.read_text("META-INF/encryption.xml") {
        for t in iter_tags(&enc, "CipherReference") {
            if let Some(uri) = attr(t, "URI") {
                out.insert(percent_decode(uri).trim_start_matches('/').to_string());
            }
        }
    }
    out
}

/// Decode a whole EPUB like `decode_epub_book`, additionally
/// richly decoding the chapter at `rich_index` when given.
/// Never fails hard: worst case mirrors the plain decode's notes.
pub fn decode_epub_book_rich(data: &[u8], rich_index: Option<usize>) -> EpubBookRich {
    let mut out = EpubBookRich::default();
    let Some(zip) = ZipArchive::open(data) else {
        out.note = Some(
            "This file isn't a readable EPUB (the book archive couldn't be opened).".into(),
        );
        return out;
    };

    // DRM: same rule as the decoders above.
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
    // Archive path → declared media type (media inlining + gates).
    let manifest_media: HashMap<String, String> = manifest
        .values()
        .map(|i| (resolve_href(&opf_dir, &i.href), i.media_type.clone()))
        .collect();
    let spine_ids: Vec<String> = iter_tags(&opf, "itemref")
        .into_iter()
        .filter(|t| attr(t, "linear") != Some("no"))
        .filter_map(|t| attr(t, "idref").map(str::to_owned))
        .take(MAX_SPINE_DOCS)
        .collect();

    // The chapter walk — deliberately the plain decode's walk, so
    // `chapters` here and there can never disagree.
    let mut chapter_paths: Vec<String> = Vec::new();
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
        chapter_paths.push(path);
        out.chapters.push(EpubChapter {
            title,
            text: capped,
        });
    }
    out.rich = vec![None; out.chapters.len()];
    if out.chapters.is_empty() && out.note.is_none() {
        out.note = Some("No readable chapters were found in this book.".into());
    } else if truncated && out.note.is_none() {
        out.note = Some(
            "This book is very large, so the reader loaded only its first portion.".into(),
        );
    }

    // The rich pass over the one chapter being read.
    if let Some(i) = rich_index {
        if let Some(path) = chapter_paths.get(i) {
            let spine_index: HashMap<String, usize> = chapter_paths
                .iter()
                .enumerate()
                .map(|(n, p)| (p.clone(), n))
                .collect();
            let chapter_dir = path
                .rsplit_once('/')
                .map(|(d, _)| d.to_string())
                .unwrap_or_default();
            let mut ctx = RichCtx {
                zip: &zip,
                manifest_media: &manifest_media,
                spine_index: &spine_index,
                encrypted: encrypted_paths(&zip),
                chapter_dir,
                chapter_index: i,
                media_bytes: 0,
                css_parts: Vec::new(),
                imported_css: std::collections::HashSet::new(),
            };
            out.rich[i] = Some(rich_chapter(&mut ctx, path));
        }
    }
    out
}

/// State shared by the sanitizer and the CSS pipeline for one
/// chapter: the archive, the spine map, the media budget, and the
/// CSS collected along the way ((base dir, raw text) in order).
struct RichCtx<'a> {
    zip: &'a ZipArchive<'a>,
    manifest_media: &'a HashMap<String, String>,
    spine_index: &'a HashMap<String, usize>,
    encrypted: std::collections::HashSet<String>,
    chapter_dir: String,
    chapter_index: usize,
    media_bytes: usize,
    css_parts: Vec<(String, String)>,
    imported_css: std::collections::HashSet<String>,
}

impl<'a> RichCtx<'a> {
    /// Inline one archive member as a data URL, under the media
    /// caps. Images and fonts only, by manifest type or extension;
    /// an encrypted member is never inlined.
    fn inline_media(&mut self, archive_path: &str) -> Option<String> {
        if self.encrypted.contains(archive_path) {
            return None;
        }
        let entry = self.zip.get(archive_path)?;
        if entry.uncompressed_size as usize > MAX_RICH_MEDIA_MEMBER {
            return None;
        }
        let media = media_type_for(self.manifest_media.get(archive_path), archive_path)?;
        let bytes = self.zip.read(entry)?;
        if self.media_bytes + bytes.len() > MAX_RICH_MEDIA_TOTAL {
            return None;
        }
        self.media_bytes += bytes.len();
        Some(format!("data:{media};base64,{}", b64_encode(&bytes)))
    }
}

/// The media type a member may inline as: the manifest's declared
/// type when it is an image/font type, else the extension's.
fn media_type_for(manifest: Option<&String>, path: &str) -> Option<String> {
    if let Some(m) = manifest {
        let m = m.trim().to_ascii_lowercase();
        if m.starts_with("image/")
            || m.starts_with("font/")
            || m == "application/font-woff"
            || m == "application/vnd.ms-opentype"
            || m == "application/x-font-ttf"
        {
            return Some(m);
        }
    }
    let ext = path.rsplit('.').next()?.to_ascii_lowercase();
    Some(
        match ext.as_str() {
            "jpg" | "jpeg" => "image/jpeg",
            "png" => "image/png",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "svg" => "image/svg+xml",
            "bmp" => "image/bmp",
            "ttf" => "font/ttf",
            "otf" => "font/otf",
            "woff" => "font/woff",
            "woff2" => "font/woff2",
            _ => return None,
        }
        .to_string(),
    )
}

/// Standard base64 (the module rolls its own, like its ZIP reader —
/// no new dependency enters the tree for one encoder).
fn b64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0] as u32,
            *chunk.get(1).unwrap_or(&0) as u32,
            *chunk.get(2).unwrap_or(&0) as u32,
        ];
        let n = (b[0] << 16) | (b[1] << 8) | b[2];
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Richly rewrite one chapter document: sanitize its XHTML and
/// collect + scope its CSS. A chapter whose markup won't parse
/// yields `html: None` (the caller falls back to its text).
fn rich_chapter(ctx: &mut RichCtx, path: &str) -> EpubChapterRich {
    let Some(xhtml) = ctx.zip.read_text(path) else {
        return EpubChapterRich::default();
    };
    let html = sanitize_chapter(&xhtml, ctx)
        .filter(|h| !h.trim().is_empty() && h.chars().count() <= MAX_RICH_HTML_CHARS);
    let parts = std::mem::take(&mut ctx.css_parts);
    let mut css = String::new();
    for (base, raw) in &parts {
        if css.chars().count() > MAX_RICH_CSS_CHARS {
            break;
        }
        css.push_str(&scope_css(raw, base, ctx, 0));
    }
    let css = if css.trim().is_empty() {
        None
    } else {
        Some(css.chars().take(MAX_RICH_CSS_CHARS).collect())
    };
    EpubChapterRich { html, css }
}

/// Resolve one entity reference body (the text between the
/// ampersand and the semicolon) to what it names: numeric refs,
/// the XML five, and
/// the named HTML refs books actually use. Unknown names return
/// None — the caller re-emits the reference for the webview's HTML
/// parser, which knows the full HTML table.
fn resolve_entity(name: &str) -> Option<String> {
    if let Some(hex) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
        return u32::from_str_radix(hex, 16)
            .ok()
            .and_then(char::from_u32)
            .map(|c| c.to_string());
    }
    if let Some(dec) = name.strip_prefix('#') {
        return dec
            .parse::<u32>()
            .ok()
            .and_then(char::from_u32)
            .map(|c| c.to_string());
    }
    let c = match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "ensp" => '\u{2002}',
        "emsp" => '\u{2003}',
        "thinsp" => '\u{2009}',
        "zwnj" => '\u{200c}',
        "zwj" => '\u{200d}',
        "lrm" => '\u{200e}',
        "rlm" => '\u{200f}',
        "ndash" => '\u{2013}',
        "mdash" => '\u{2014}',
        "lsquo" => '\u{2018}',
        "rsquo" => '\u{2019}',
        "sbquo" => '\u{201a}',
        "ldquo" => '\u{201c}',
        "rdquo" => '\u{201d}',
        "bdquo" => '\u{201e}',
        "laquo" => '\u{ab}',
        "raquo" => '\u{bb}',
        "lsaquo" => '\u{2039}',
        "rsaquo" => '\u{203a}',
        "hellip" => '\u{2026}',
        "dagger" => '\u{2020}',
        "Dagger" => '\u{2021}',
        "bull" => '\u{2022}',
        "prime" => '\u{2032}',
        "Prime" => '\u{2033}',
        "permil" => '\u{2030}',
        "oline" => '\u{203e}',
        "frasl" => '\u{2044}',
        "copy" => '\u{a9}',
        "reg" => '\u{ae}',
        "trade" => '\u{2122}',
        "middot" => '\u{b7}',
        "sect" => '\u{a7}',
        "para" => '\u{b6}',
        "deg" => '\u{b0}',
        "plusmn" => '\u{b1}',
        "times" => '\u{d7}',
        "divide" => '\u{f7}',
        "minus" => '\u{2212}',
        "infin" => '\u{221e}',
        "ne" => '\u{2260}',
        "le" => '\u{2264}',
        "ge" => '\u{2265}',
        "asymp" => '\u{2248}',
        "euro" => '\u{20ac}',
        "pound" => '\u{a3}',
        "yen" => '\u{a5}',
        "cent" => '\u{a2}',
        "curren" => '\u{a4}',
        "brvbar" => '\u{a6}',
        "uml" => '\u{a8}',
        "ordf" => '\u{aa}',
        "ordm" => '\u{ba}',
        "iexcl" => '\u{a1}',
        "iquest" => '\u{bf}',
        "szlig" => '\u{df}',
        "agrave" => '\u{e0}',
        "eacute" => '\u{e9}',
        "egrave" => '\u{e8}',
        "ccedil" => '\u{e7}',
        "ntilde" => '\u{f1}',
        "auml" => '\u{e4}',
        "ouml" => '\u{f6}',
        "uuml" => '\u{fc}',
        "alpha" => '\u{3b1}',
        "beta" => '\u{3b2}',
        "gamma" => '\u{3b3}',
        "delta" => '\u{3b4}',
        "epsilon" => '\u{3b5}',
        "lambda" => '\u{3bb}',
        "mu" => '\u{3bc}',
        "pi" => '\u{3c0}',
        "sigma" => '\u{3c3}',
        "phi" => '\u{3c6}',
        "omega" => '\u{3c9}',
        "Alpha" => '\u{391}',
        "Delta" => '\u{394}',
        "Omega" => '\u{3a9}',
        "larr" => '\u{2190}',
        "uarr" => '\u{2191}',
        "rarr" => '\u{2192}',
        "darr" => '\u{2193}',
        "harr" => '\u{2194}',
        "spades" => '\u{2660}',
        "clubs" => '\u{2663}',
        "hearts" => '\u{2665}',
        "diams" => '\u{2666}',
        "loz" => '\u{25ca}',
        _ => return None,
    };
    Some(c.to_string())
}

/// Escape text content for the HTML fragment.
fn esc_text_into(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

/// Escape an attribute value for the HTML fragment.
fn esc_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Elements dropped WITH their contents: active content, embedding,
/// forms, and inert templates. (`link` has its own arm — harvested
/// for CSS first. Everything not allowed and not dropped here is
/// UNWRAPPED: the tag dies, its children survive.)
const RICH_DROP_CONTENTS: &[&str] = &[
    "script", "iframe", "object", "embed", "form", "select", "textarea",
    "button", "frame", "frameset", "applet", "template", "title",
];

/// The XHTML book vocabulary the reader renders. Anything else is
/// unwrapped, so an unknown element costs its tag, never its text.
const RICH_ALLOWED: &[&str] = &[
    "p", "div", "span", "h1", "h2", "h3", "h4", "h5", "h6", "blockquote",
    "pre", "code", "em", "i", "strong", "b", "small", "sub", "sup", "a",
    "img", "br", "hr", "ul", "ol", "li", "dl", "dt", "dd", "table",
    "thead", "tbody", "tfoot", "tr", "td", "th", "caption", "colgroup",
    "col", "section", "article", "aside", "figure", "figcaption", "header",
    "footer", "nav", "main", "q", "cite", "abbr", "time", "mark", "ruby",
    "rt", "rp", "bdi", "bdo", "wbr", "ins", "del", "s", "u", "var", "kbd",
    "samp", "data", "dfn",
];

/// One emitted element on the open stack, so its End event can be
/// matched and closed (or silently consumed for voids/unwraps).
enum Open {
    Elem(String),
    Bare(String),
    ImgSpan,
    Unwrap(String),
}

fn open_name(o: &Open) -> &str {
    match o {
        Open::Elem(n) | Open::Bare(n) | Open::Unwrap(n) => n,
        Open::ImgSpan => "img",
    }
}

/// An `<svg>` subtree in flight: only its image survives (the common
/// full-page illustration pattern); a script inside poisons it.
struct SvgState {
    depth: usize,
    image: Option<String>,
    poison: bool,
}

/// An element's local name, lowercased (`epub:section` → `section`).
fn elem_name(full: &str) -> String {
    full.rsplit(':').next().unwrap_or(full).to_ascii_lowercase()
}

/// Directory part of an archive path ("" at the archive root).
fn dir_of(path: &str) -> String {
    path.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default()
}

/// Attribute (key, value) pairs of a start tag, values unescaped.
/// quick-xml surfaces values raw; §U's rule applies (unescape here,
/// with the module's own unescape as the error fallback).
fn raw_attrs(e: &quick_xml::events::BytesStart) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for a in e.attributes() {
        let Ok(a) = a else { continue };
        let key = a.key.as_ref().to_ascii_lowercase();
        let value = a
            .unescape_value()
            .map(|c| c.into_owned())
            .unwrap_or_else(|_| unescape(&a.value));
        out.push((key, value));
    }
    out
}

fn digits_only(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// The attribute filter: what may ride an emitted element.
/// Everything else — every `on*` handler, every URL but the ones
/// rewritten below, every presentational hook the CSS pass owns —
/// is dropped. Publisher classes survive (the CSS needs them);
/// `epub:type` note semantics become `fable-*` classes.
fn filtered_attrs(
    ctx: &mut RichCtx,
    elem: &str,
    e: &quick_xml::events::BytesStart,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut classes: Vec<String> = Vec::new();
    let mut style: Option<String> = None;
    let mut link_attrs: Vec<(String, String)> = Vec::new();
    let base_dir = ctx.chapter_dir.clone();
    for (key, value) in raw_attrs(e) {
        match key.as_str() {
            "class" => classes.extend(value.split_whitespace().map(str::to_string)),
            "id" | "title" | "lang" | "dir" | "xml:lang" => out.push((key, value)),
            "style" => {
                let f = filter_declarations(&value, &base_dir, ctx);
                if !f.is_empty() {
                    style = Some(f);
                }
            }
            "epub:type" => {
                for tok in value.split_whitespace() {
                    match tok {
                        "noteref" => classes.push("fable-noteref".to_string()),
                        "footnote" | "endnote" | "rearnote" | "note" | "footnotes"
                        | "endnotes" => classes.push("fable-note".to_string()),
                        _ => {}
                    }
                }
            }
            "role" => match value.trim() {
                "doc-noteref" => classes.push("fable-noteref".to_string()),
                "doc-footnote" | "doc-endnote" => classes.push("fable-note".to_string()),
                _ => {}
            },
            "href" if elem == "a" => link_attrs = rewrite_href(ctx, &value),
            "colspan" | "rowspan" if elem == "td" || elem == "th" => {
                if digits_only(&value) {
                    out.push((key, value));
                }
            }
            "start" if elem == "ol" => {
                if digits_only(&value) {
                    out.push((key, value));
                }
            }
            "value" if elem == "li" => {
                if digits_only(&value) {
                    out.push((key, value));
                }
            }
            "type" if elem == "ol" => out.push((key, value)),
            "datetime" if elem == "time" => out.push((key, value)),
            _ => {}
        }
    }
    if !classes.is_empty() {
        out.push(("class".to_string(), classes.join(" ")));
    }
    if let Some(s) = style {
        out.push(("style".to_string(), s));
    }
    out.extend(link_attrs);
    out
}

/// Rewrite one `<a href>` into navigation data attributes — the
/// webview never receives a live book href. Internal targets become
/// `data-fable-link` (spine chapter) + optional `data-fable-frag`;
/// web/mail targets become `data-fable-ext`; anything else (unknown
/// schemes, targets outside the spine) loses its link and keeps
/// its text.
fn rewrite_href(ctx: &RichCtx, href: &str) -> Vec<(String, String)> {
    let href = href.trim();
    if href.is_empty() {
        return Vec::new();
    }
    let lower = href.to_ascii_lowercase();
    if lower.starts_with("http:") || lower.starts_with("https:") || lower.starts_with("mailto:") {
        return vec![("data-fable-ext".to_string(), href.to_string())];
    }
    if let Some(colon) = href.find(':') {
        if !href[..colon].contains('/') {
            return Vec::new(); // javascript:, data:, file:… — dead by design
        }
    }
    let (path, frag) = match href.split_once('#') {
        Some((p, f)) => (p, Some(f)),
        None => (href, None),
    };
    let target = if path.is_empty() {
        Some(ctx.chapter_index)
    } else {
        let resolved = resolve_href(&ctx.chapter_dir, path);
        ctx.spine_index.get(&resolved).copied()
    };
    match target {
        Some(idx) => {
            let mut v = vec![("data-fable-link".to_string(), idx.to_string())];
            if let Some(f) = frag {
                if !f.is_empty() {
                    v.push(("data-fable-frag".to_string(), f.to_string()));
                }
            }
            v
        }
        None => Vec::new(),
    }
}

/// Emit an allowed element's start tag and record it on the stack.
fn emit_open(
    ctx: &mut RichCtx,
    name: &str,
    e: &quick_xml::events::BytesStart,
    out: &mut String,
    stack: &mut Vec<Open>,
) {
    let attrs = filtered_attrs(ctx, name, e);
    out.push('<');
    out.push_str(name);
    for (k, v) in &attrs {
        out.push(' ');
        out.push_str(k);
        out.push_str("=\"");
        out.push_str(&esc_attr(v));
        out.push('"');
    }
    out.push('>');
    if matches!(name, "br" | "hr" | "col" | "wbr") {
        stack.push(Open::Bare(name.to_string()));
    } else {
        stack.push(Open::Elem(name.to_string()));
    }
}

/// Emit an `<img>`: its source inlined when the archive can honestly
/// provide it, else an honest placeholder span carrying the alt text
/// (the End event closes whichever was emitted).
fn emit_img(ctx: &mut RichCtx, e: &quick_xml::events::BytesStart, out: &mut String) -> Open {
    let mut src: Option<String> = None;
    let mut alt = String::new();
    let mut classes: Vec<String> = Vec::new();
    let mut dims: Vec<(String, String)> = Vec::new();
    for (key, value) in raw_attrs(e) {
        match key.as_str() {
            "src" => src = Some(value),
            "alt" => alt = value,
            "class" => classes.extend(value.split_whitespace().map(str::to_string)),
            "width" | "height" => {
                if value.bytes().all(|b| b.is_ascii_digit() || b == b'%') && !value.is_empty() {
                    dims.push((key, value));
                }
            }
            _ => {}
        }
    }
    let data = src.as_deref().and_then(|s| {
        let path = resolve_href(&ctx.chapter_dir, s);
        ctx.inline_media(&path)
    });
    match data {
        Some(url) => {
            out.push_str("<img src=\"");
            out.push_str(&url);
            out.push('"');
            if !alt.is_empty() {
                out.push_str(" alt=\"");
                out.push_str(&esc_attr(&alt));
                out.push('"');
            }
            if !classes.is_empty() {
                out.push_str(" class=\"");
                out.push_str(&esc_attr(&classes.join(" ")));
                out.push('"');
            }
            for (k, v) in &dims {
                out.push(' ');
                out.push_str(k);
                out.push_str("=\"");
                out.push_str(&esc_attr(v));
                out.push('"');
            }
            out.push('>');
            Open::Bare("img".to_string())
        }
        None => {
            out.push_str("<span class=\"fable-img-missing\">");
            let label = if alt.is_empty() { "Image" } else { alt.as_str() };
            esc_text_into(label, out);
            Open::ImgSpan
        }
    }
}

/// A stylesheet `<link>`: harvest its CSS into the chapter's parts
/// (document order matters, so this runs where the link appears).
fn harvest_link(ctx: &mut RichCtx, e: &quick_xml::events::BytesStart) {
    let mut rel = String::new();
    let mut href: Option<String> = None;
    for (key, value) in raw_attrs(e) {
        match key.as_str() {
            "rel" => rel = value,
            "href" => href = Some(value),
            _ => {}
        }
    }
    if !rel.split_whitespace().any(|t| t.eq_ignore_ascii_case("stylesheet")) {
        return;
    }
    let Some(href) = href else { return };
    let path = resolve_href(&ctx.chapter_dir, &href);
    if let Some(text) = ctx.zip.read_text(&path) {
        let base = dir_of(&path);
        ctx.css_parts
            .push((base, text.chars().take(MAX_RICH_CSS_CHARS).collect()));
    }
}

/// One start event inside an `<svg>` subtree: track nesting, watch
/// for a script (poison), and capture the first resolvable image.
fn svg_start(
    svg: &mut SvgState,
    name: &str,
    e: &quick_xml::events::BytesStart,
    ctx: &mut RichCtx,
) {
    match name {
        "svg" => svg.depth += 1,
        "script" => svg.poison = true,
        "image" => {
            if svg.image.is_none() {
                for (key, value) in raw_attrs(e) {
                    if key == "href" || key == "xlink:href" {
                        let path = resolve_href(&ctx.chapter_dir, &value);
                        if let Some(url) = ctx.inline_media(&path) {
                            svg.image = Some(url);
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

/// The streaming sanitizer: one chapter document in, a safe HTML
/// fragment out. `None` when the markup doesn't parse — the caller
/// falls back to the chapter's text rather than showing a
/// half-rewritten page.
fn sanitize_chapter(xhtml: &str, ctx: &mut RichCtx) -> Option<String> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::reader::Reader::from_str(xhtml);
    let cfg = reader.config_mut();
    cfg.trim_text_start = false;
    cfg.trim_text_end = false;
    cfg.expand_empty_elements = true;

    let mut out = String::with_capacity(xhtml.len().min(MAX_RICH_HTML_CHARS));
    let mut stack: Vec<Open> = Vec::new();
    let mut skip: Option<(String, usize)> = None;
    let mut style_text: Option<String> = None;
    let mut svg: Option<SvgState> = None;

    loop {
        match reader.read_event() {
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => {
                let name = elem_name(e.name().as_ref());
                if let Some(s) = &mut svg {
                    svg_start(s, &name, &e, ctx);
                    continue;
                }
                if let Some((root, depth)) = &mut skip {
                    if name == *root {
                        *depth += 1;
                    }
                    continue;
                }
                if style_text.is_some() {
                    continue; // tags inside <style> aren't real
                }
                match name.as_str() {
                    "style" => style_text = Some(String::new()),
                    "link" => {
                        harvest_link(ctx, &e);
                        skip = Some(("link".to_string(), 1));
                    }
                    "svg" => {
                        svg = Some(SvgState {
                            depth: 1,
                            image: None,
                            poison: false,
                        })
                    }
                    "img" => {
                        let open = emit_img(ctx, &e, &mut out);
                        stack.push(open);
                    }
                    _ if RICH_DROP_CONTENTS.contains(&name.as_str()) => {
                        skip = Some((name, 1));
                    }
                    _ if RICH_ALLOWED.contains(&name.as_str()) => {
                        emit_open(ctx, &name, &e, &mut out, &mut stack);
                    }
                    _ => stack.push(Open::Unwrap(name)),
                }
            }
            Ok(Event::End(e)) => {
                let name = elem_name(e.name().as_ref());
                if let Some(mut s) = svg.take() {
                    if name == "svg" {
                        s.depth = s.depth.saturating_sub(1);
                        if s.depth == 0 {
                            if !s.poison {
                                if let Some(url) = &s.image {
                                    out.push_str("<img src=\"");
                                    out.push_str(url);
                                    out.push_str("\" alt=\"\">");
                                }
                            }
                            continue;
                        }
                    }
                    svg = Some(s);
                    continue;
                }
                if style_text.is_some() {
                    if name == "style" {
                        let text = style_text.take().unwrap_or_default();
                        ctx.css_parts.push((ctx.chapter_dir.clone(), text));
                    }
                    continue;
                }
                if let Some((root, depth)) = &mut skip {
                    if name == *root {
                        *depth -= 1;
                        if *depth == 0 {
                            skip = None;
                        }
                    }
                    continue;
                }
                let matched = matches!(stack.last(), Some(o) if open_name(o) == name);
                if matched {
                    match stack.pop() {
                        Some(Open::Elem(n)) => {
                            out.push_str("</");
                            out.push_str(&n);
                            out.push('>');
                        }
                        Some(Open::ImgSpan) => out.push_str("</span>"),
                        _ => {}
                    }
                }
            }
            Ok(Event::Text(e)) => {
                let text: &str = e.as_ref();
                if let Some(cap) = &mut style_text {
                    cap.push_str(text);
                } else if skip.is_none() && svg.is_none() {
                    esc_text_into(text, &mut out);
                }
            }
            Ok(Event::GeneralRef(e)) => {
                let name: &str = e.as_ref();
                if let Some(cap) = &mut style_text {
                    cap.push('&');
                    cap.push_str(name);
                    cap.push(';');
                } else if skip.is_none() && svg.is_none() {
                    match resolve_entity(name) {
                        Some(s) => esc_text_into(&s, &mut out),
                        None => {
                            if !name.is_empty()
                                && name
                                    .bytes()
                                    .all(|b| b.is_ascii_alphanumeric() || b == b'#')
                            {
                                // Unknown but well-formed: hand it to
                                // the webview's HTML parser, which
                                // knows the full HTML entity table.
                                out.push('&');
                                out.push_str(name);
                                out.push(';');
                            } else {
                                esc_text_into(name, &mut out);
                            }
                        }
                    }
                }
            }
            Ok(Event::CData(e)) => {
                let text: &str = e.as_ref();
                if let Some(cap) = &mut style_text {
                    cap.push_str(text);
                } else if skip.is_none() && svg.is_none() {
                    esc_text_into(text, &mut out);
                }
            }
            Ok(_) => {} // comments, PIs, doctype, declarations
            Err(_) => return None,
        }
        if out.len() > MAX_RICH_HTML_CHARS * 2 {
            return None;
        }
    }
    Some(out)
}

/// Remove `/* … */` comments (quote-aware; an unterminated comment
/// eats the tail, as CSS itself would treat it).
fn strip_css_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut q: Option<char> = None;
    let mut rest = css;
    while !rest.is_empty() {
        let c = rest.chars().next().unwrap_or('\u{fffd}');
        if let Some(qc) = q {
            out.push(c);
            if c == qc {
                q = None;
            }
            rest = &rest[c.len_utf8()..];
            continue;
        }
        if c == '"' || c == '\'' {
            q = Some(c);
            out.push(c);
            rest = &rest[c.len_utf8()..];
            continue;
        }
        if rest.starts_with("/*") {
            match rest[2..].find("*/") {
                Some(end) => rest = &rest[2 + end + 2..],
                None => break,
            }
            continue;
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// Split a stylesheet (or an at-rule's block) into (prelude, block)
/// items: a prelude ending at `;` is a statement (block None); one
/// ending at `{` carries its balanced block. Quote- and paren-aware
/// — CSS structure characters are ASCII, so the byte walk is safe.
fn css_items(css: &str) -> Vec<(String, Option<String>)> {
    let b = css.as_bytes();
    let n = b.len();
    let mut items = Vec::new();
    let mut i = 0usize;
    while i < n {
        let start = i;
        let mut parens = 0i32;
        let mut q: Option<u8> = None;
        let mut term: Option<u8> = None;
        while i < n {
            let c = b[i];
            if let Some(qc) = q {
                if c == qc {
                    q = None;
                }
                i += 1;
                continue;
            }
            match c {
                b'"' | b'\'' => q = Some(c),
                b'(' => parens += 1,
                b')' => parens -= 1,
                b'{' if parens == 0 => {
                    term = Some(b'{');
                    break;
                }
                b';' if parens == 0 => {
                    term = Some(b';');
                    break;
                }
                _ => {}
            }
            i += 1;
        }
        let prelude = css[start..i].trim().to_string();
        match term {
            Some(b';') => {
                i += 1;
                if !prelude.is_empty() {
                    items.push((prelude, None));
                }
            }
            Some(b'{') => {
                i += 1;
                let bstart = i;
                let mut depth = 1i32;
                let mut q: Option<u8> = None;
                let mut closed = false;
                while i < n {
                    let c = b[i];
                    if let Some(qc) = q {
                        if c == qc {
                            q = None;
                        }
                        i += 1;
                        continue;
                    }
                    match c {
                        b'"' | b'\'' => q = Some(c),
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                closed = true;
                                i += 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
                let block = if closed {
                    &css[bstart..i.saturating_sub(1)]
                } else {
                    &css[bstart..i]
                };
                if !prelude.is_empty() {
                    items.push((prelude, Some(block.to_string())));
                }
            }
            _ => break,
        }
    }
    items
}

/// Split a selector list on top-level commas (paren/bracket/quote
/// aware — `:not(a, b)` and `[href="x,y"]` stay whole).
fn split_top_commas(s: &str) -> Vec<String> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut q: Option<u8> = None;
    let mut start = 0usize;
    for i in 0..b.len() {
        let c = b[i];
        if let Some(qc) = q {
            if c == qc {
                q = None;
            }
            continue;
        }
        match c {
            b'"' | b'\'' => q = Some(c),
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b',' if depth == 0 => {
                out.push(s[start..i].to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(s[start..].to_string());
    out
}

/// Split a declaration block on top-level semicolons (paren/quote
/// aware — `url(data:image/png;base64,…)` stays one declaration).
fn split_declarations(block: &str) -> Vec<String> {
    let b = block.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut q: Option<u8> = None;
    let mut start = 0usize;
    for i in 0..b.len() {
        let c = b[i];
        if let Some(qc) = q {
            if c == qc {
                q = None;
            }
            continue;
        }
        match c {
            b'"' | b'\'' => q = Some(c),
            b'(' => depth += 1,
            b')' => depth -= 1,
            b';' if depth == 0 => {
                out.push(block[start..i].to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    let tail = &block[start..];
    if !tail.trim().is_empty() {
        out.push(tail.to_string());
    }
    out
}

/// Scope one selector under the reader's `.reader-pub` container.
/// Document-level roots (html/body/:root) map onto the container
/// itself; everything else descends from it.
fn scope_selector(sel: &str) -> String {
    let s = sel.trim();
    if s.is_empty() {
        return String::new();
    }
    let lower = s.to_ascii_lowercase();
    for root in ["body", "html", ":root"] {
        if lower == root {
            return ".reader-pub".to_string();
        }
        if lower.starts_with(root) {
            let rest = &s[root.len()..];
            if matches!(
                rest.chars().next(),
                Some('.' | '#' | '[' | ':' | ' ' | '>' | '+' | '~')
            ) {
                return format!(".reader-pub{rest}");
            }
        }
    }
    format!(".reader-pub {s}")
}

/// Rewrite every `url(…)` in a declaration value: data: and
/// fragment refs pass through; archive members inline (encrypted
/// members refuse inside `inline_media`); anything external kills
/// the declaration (None).
fn rewrite_css_urls(value: &str, base_dir: &str, ctx: &mut RichCtx) -> Option<String> {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    loop {
        let lower = rest.to_ascii_lowercase();
        let Some(pos) = lower.find("url(") else {
            out.push_str(rest);
            return Some(out);
        };
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 4..];
        let close = after.find(')')?;
        let inner = after[..close].trim().trim_matches(|c| c == '"' || c == '\'');
        if inner.starts_with("data:") || inner.starts_with('#') {
            out.push_str("url(");
            out.push_str(&after[..close]);
            out.push(')');
        } else if inner.contains("://") || inner.starts_with("//") {
            return None;
        } else {
            let path = resolve_href(base_dir, inner);
            let url = ctx.inline_media(&path)?;
            out.push_str("url(\"");
            out.push_str(&url);
            out.push_str("\")");
        }
        rest = &after[close + 1..];
    }
}

/// Filter one declaration block: drop active/exfiltration-shaped
/// declarations and flow-breaking positioning; rewrite urls
/// through the archive. Returns "" when nothing survives.
fn filter_declarations(block: &str, base_dir: &str, ctx: &mut RichCtx) -> String {
    let mut out = String::new();
    for decl in split_declarations(block) {
        let Some(colon) = decl.find(':') else {
            continue;
        };
        let prop = decl[..colon].trim();
        let value = decl[colon + 1..].trim();
        if prop.is_empty() || value.is_empty() {
            continue;
        }
        let prop_l = prop.to_ascii_lowercase();
        let val_l = value.to_ascii_lowercase();
        if val_l.contains("expression(")
            || val_l.contains("javascript:")
            || val_l.contains("vbscript:")
        {
            continue;
        }
        if prop_l == "behavior" || prop_l == "-moz-binding" {
            continue;
        }
        if prop_l == "position" && (val_l == "fixed" || val_l == "absolute") {
            continue; // a reading column is a flow, not a canvas
        }
        let value = if val_l.contains("url(") {
            match rewrite_css_urls(value, base_dir, ctx) {
                Some(v) => v,
                None => continue,
            }
        } else {
            value.to_string()
        };
        out.push_str(prop);
        out.push(':');
        out.push_str(&value);
        out.push(';');
    }
    out
}

/// The target of an `@import` prelude: a quoted string or a url().
fn import_target(prelude: &str) -> Option<String> {
    let p = prelude.trim();
    if let Some(u) = p.to_ascii_lowercase().find("url(") {
        let rest = &p[u + 4..];
        let end = rest.find(')')?;
        return Some(
            rest[..end]
                .trim()
                .trim_matches(|c| c == '"' || c == '\'')
                .to_string(),
        );
    }
    let q = p.find(['"', '\''])?;
    let quote = p.as_bytes()[q];
    let rest = &p[q + 1..];
    let end = rest.find(quote as char)?;
    Some(rest[..end].to_string())
}

/// Sanitize + scope one stylesheet (or at-rule block) against the
/// archive. `base_dir` is the directory the sheet's own relative
/// references resolve against; `depth` bounds @import chains.
fn scope_css(css: &str, base_dir: &str, ctx: &mut RichCtx, depth: usize) -> String {
    let cleaned = strip_css_comments(css);
    let mut out = String::new();
    for (prelude, block) in css_items(&cleaned) {
        if out.len() > MAX_RICH_CSS_CHARS {
            break;
        }
        let p = prelude.trim();
        if let Some(at) = p.strip_prefix('@') {
            let name = format!(
                "@{}",
                at.split(|c: char| c.is_whitespace() || c == '(')
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase()
            );
            match name.as_str() {
                "@import" => {
                    if depth < MAX_RICH_IMPORT_DEPTH {
                        if let Some(target) = import_target(p) {
                            let resolved = resolve_href(base_dir, &target);
                            let is_css = ctx
                                .manifest_media
                                .get(&resolved)
                                .map(|m| m.contains("css"))
                                .unwrap_or(false)
                                || resolved.ends_with(".css");
                            if is_css && !ctx.imported_css.contains(&resolved) {
                                if let Some(text) = ctx.zip.read_text(&resolved) {
                                    ctx.imported_css.insert(resolved.clone());
                                    let base2 = dir_of(&resolved);
                                    let inner = scope_css(&text, &base2, ctx, depth + 1);
                                    out.push_str(&inner);
                                }
                            }
                        }
                    }
                }
                "@media" | "@supports" => {
                    if let Some(b) = &block {
                        let inner = scope_css(b, base_dir, ctx, depth);
                        if !inner.trim().is_empty() {
                            out.push_str(p);
                            out.push('{');
                            out.push_str(&inner);
                            out.push('}');
                        }
                    }
                }
                "@font-face" => {
                    if let Some(b) = &block {
                        let had_url = b.to_ascii_lowercase().contains("url(");
                        let decls = filter_declarations(b, base_dir, ctx);
                        // inline_media refuses encrypted members, so a
                        // surviving data: src is a clean font; a face
                        // whose every src died is dropped whole.
                        if !decls.is_empty() && (!had_url || decls.contains("data:")) {
                            out.push_str("@font-face{");
                            out.push_str(&decls);
                            out.push('}');
                        }
                    }
                }
                "@keyframes" | "@-webkit-keyframes" => {
                    if let Some(b) = &block {
                        let mut inner = String::new();
                        for (sel, blk) in css_items(b) {
                            if let Some(bb) = &blk {
                                let d = filter_declarations(bb, base_dir, ctx);
                                if !d.is_empty() {
                                    inner.push_str(sel.trim());
                                    inner.push('{');
                                    inner.push_str(&d);
                                    inner.push('}');
                                }
                            }
                        }
                        if !inner.is_empty() {
                            out.push_str(p);
                            out.push('{');
                            out.push_str(&inner);
                            out.push('}');
                        }
                    }
                }
                _ => {} // @charset/@page/@viewport/… — dropped
            }
            continue;
        }
        let Some(b) = &block else { continue };
        let decls = filter_declarations(b, base_dir, ctx);
        if decls.is_empty() {
            continue;
        }
        let selectors: Vec<String> = split_top_commas(p)
            .iter()
            .map(|s| scope_selector(s))
            .filter(|s| !s.is_empty())
            .collect();
        if selectors.is_empty() {
            continue;
        }
        out.push_str(&selectors.join(","));
        out.push('{');
        out.push_str(&decls);
        out.push('}');
    }
    out
}

#[cfg(test)]
mod rich_tests {
    use super::*;

    // -- stored-only ZIP writer (the reader verifies no CRCs) --
    fn stored_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        let mut central: Vec<u8> = Vec::new();
        for (name, data) in entries {
            let offset = out.len() as u32;
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // stored
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes()); // crc (unchecked)
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);
            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
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

    const RCONTAINER: &str = r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;

    const ROPF: &str = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="bid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="bid">urn:fixture:rich</dc:identifier>
    <dc:title>The Rich Fixture</dc:title>
    <dc:creator>Jane Tester</dc:creator>
  </metadata>
  <manifest>
    <item id="ch1" href="text/ch1.xhtml" media-type="application/xhtml+xml"/>
    <item id="ch2" href="text/ch2.xhtml" media-type="application/xhtml+xml"/>
    <item id="notes" href="text/notes.xhtml" media-type="application/xhtml+xml"/>
    <item id="css" href="css/book.css" media-type="text/css"/>
    <item id="xcss" href="css/extra.css" media-type="text/css"/>
    <item id="fig" href="images/fig.png" media-type="image/png"/>
    <item id="font" href="fonts/book.ttf" media-type="application/x-font-ttf"/>
  </manifest>
  <spine><itemref idref="ch1"/><itemref idref="ch2"/><itemref idref="notes"/></spine>
</package>"#;

    const RCSS: &str = r#"/* fixture sheet */
@import url("extra.css");
body { font-family: "BookFont", serif; color: #222; }
.dropcap { font-size: 3em; float: left; line-height: 0.8; }
.evil { background: url(http://evil.example/x.png); position: fixed; top: 0; }
@font-face { font-family: "BookFont"; src: url(../fonts/book.ttf); }
@media print { .dropcap { float: none; } }
"#;

    const RXCSS: &str = ".imported { letter-spacing: 0.1em; }";

    const RCH1: &str = r##"<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
<head><title>Ch 1 head</title><link rel="stylesheet" type="text/css" href="../css/book.css"/></head>
<body>
<h1 class="dropcap">Chapter One</h1>
<p>Text with &amp; entity &mdash; dash &#8217; quote &unknownref; too.</p>
<p>See the figure <img src="../images/fig.png" alt="A figure"/> and note.<a epub:type="noteref" href="notes.xhtml#n1"><sup>1</sup></a></p>
<p><a href="ch2.xhtml">Next chapter</a> <a href="#local">local</a> <a href="https://example.com/x">external</a> <a href="javascript:evil()">bad</a></p>
<p id="local">Local target.</p>
<script>alert(1)</script>
<p onclick="evil()">Clean para.</p>
</body></html>"##;

    const RCH2: &str = r#"<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:xlink="http://www.w3.org/1999/xlink">
<head><title>Ch 2</title><style>p { margin: 0 0 1em; }</style></head>
<body>
<h1>Chapter Two</h1>
<p style="color: red; position: absolute; top: 0">Styled para.</p>
<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><image xlink:href="../images/fig.png" width="100" height="100"/></svg>
</body></html>"#;

    const RNOTES: &str = r#"<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
<head><title>Notes</title></head>
<body>
<aside epub:type="footnote" id="n1"><p>1. The note text. <a href="ch1.xhtml">back</a></p></aside>
</body></html>"#;

    const PNG_BYTES: &[u8] = b"\x89PNG\r\n\x1a\nFAKEFIG";
    const TTF_BYTES: &[u8] = b"FAKETTF0";

    fn rich_book() -> Vec<u8> {
        stored_zip(&[
            ("mimetype", b"application/epub+zip"),
            ("META-INF/container.xml", RCONTAINER.as_bytes()),
            ("OEBPS/content.opf", ROPF.as_bytes()),
            ("OEBPS/css/book.css", RCSS.as_bytes()),
            ("OEBPS/css/extra.css", RXCSS.as_bytes()),
            ("OEBPS/images/fig.png", PNG_BYTES),
            ("OEBPS/fonts/book.ttf", TTF_BYTES),
            ("OEBPS/text/ch1.xhtml", RCH1.as_bytes()),
            ("OEBPS/text/ch2.xhtml", RCH2.as_bytes()),
            ("OEBPS/text/notes.xhtml", RNOTES.as_bytes()),
        ])
    }

    #[test]
    fn rich_chapters_match_plain_decode() {
        let data = rich_book();
        let plain = decode_epub_book(&data);
        let rich = decode_epub_book_rich(&data, Some(0));
        assert_eq!(rich.chapters, plain.chapters);
        assert_eq!(rich.chapters.len(), 3);
        assert_eq!(rich.chapters[0].title.as_deref(), Some("Chapter One"));
        // No rich index requested → every rich slot stays empty.
        let none = decode_epub_book_rich(&data, None);
        assert!(none.rich.iter().all(|r| r.is_none()));
    }

    #[test]
    fn rich_html_keeps_the_book_and_drops_the_danger() {
        let rich = decode_epub_book_rich(&rich_book(), Some(0));
        let ch = rich.rich[0].as_ref().expect("rich chapter");
        let html = ch.html.as_deref().expect("html present");
        // Publisher structure + classes survive.
        assert!(html.contains("<h1 class=\"dropcap\">Chapter One</h1>"), "{html}");
        // Entities: known refs resolve, unknown refs pass through.
        assert!(html.contains("Text with &amp; entity — dash ’ quote &unknownref; too."), "{html}");
        // Images inline as data URLs with their alt.
        assert!(html.contains("<img src=\"data:image/png;base64,iVBORw0KG"), "{html}");
        assert!(html.contains("alt=\"A figure\""), "{html}");
        // The footnote reference is a navigation datum, tagged.
        assert!(html.contains("data-fable-link=\"2\""), "{html}");
        assert!(html.contains("data-fable-frag=\"n1\""), "{html}");
        assert!(html.contains("class=\"fable-noteref\""), "{html}");
        // Internal, fragment, external links — all rewritten.
        assert!(html.contains("<a data-fable-link=\"1\">Next chapter</a>"), "{html}");
        assert!(html.contains("data-fable-link=\"0\""), "{html}");
        assert!(html.contains("data-fable-frag=\"local\""), "{html}");
        assert!(html.contains("data-fable-ext=\"https://example.com/x\""), "{html}");
        assert!(html.contains("<a>bad</a>"), "{html}");
        // No live href, no script, no handler survives anywhere.
        assert!(!html.contains("href="), "{html}");
        assert!(!html.contains("<script"), "{html}");
        assert!(!html.contains("alert"), "{html}");
        assert!(!html.contains("onclick"), "{html}");
    }

    #[test]
    fn rich_css_is_scoped_sanitized_and_complete() {
        let rich = decode_epub_book_rich(&rich_book(), Some(0));
        let css = rich.rich[0]
            .as_ref()
            .and_then(|c| c.css.as_deref())
            .expect("css present")
            .to_string();
        // body maps onto the container; classes descend from it.
        assert!(css.contains(".reader-pub{font-family:\"BookFont\", serif;color:#222;}"), "{css}");
        assert!(css.contains(".reader-pub .dropcap{"), "{css}");
        assert!(css.contains("float:left"), "{css}");
        // The @import chain is followed inside the archive.
        assert!(css.contains(".reader-pub .imported{letter-spacing:0.1em;}"), "{css}");
        // External art and fixed positioning are gone; the rest stays.
        assert!(!css.contains("evil.example"), "{css}");
        assert!(!css.contains("position:fixed"), "{css}");
        assert!(css.contains(".reader-pub .evil{top:0;}"), "{css}");
        // The embedded font is inlined as a data URL.
        assert!(css.contains("@font-face{"), "{css}");
        assert!(css.contains("data:application/x-font-ttf;base64,"), "{css}");
        // @media survives with scoped inner rules.
        assert!(css.contains("@media print"), "{css}");
        assert!(css.contains(".reader-pub .dropcap{float:none;}"), "{css}");
    }

    #[test]
    fn rich_second_chapter_styles_and_svg() {
        let rich = decode_epub_book_rich(&rich_book(), Some(1));
        let ch = rich.rich[1].as_ref().expect("rich chapter");
        let html = ch.html.as_deref().expect("html present");
        // Inline style filtered: color + top survive, absolute dies.
        assert!(html.contains("style=\"color:red;top:0;\""), "{html}");
        // The SVG illustration becomes its image.
        assert!(html.contains("<img src=\"data:image/png;base64,iVBORw0KG"), "{html}");
        assert!(!html.contains("<svg"), "{html}");
        // The chapter's own <style> block is scoped too.
        let css = ch.css.as_deref().expect("css present");
        assert!(css.contains(".reader-pub p{margin:0 0 1em;}"), "{css}");
    }

    #[test]
    fn rich_notes_carry_the_note_class_and_back_link() {
        let rich = decode_epub_book_rich(&rich_book(), Some(2));
        let html = rich.rich[2]
            .as_ref()
            .and_then(|c| c.html.as_deref())
            .expect("html present")
            .to_string();
        assert!(html.contains("class=\"fable-note\""), "{html}");
        assert!(html.contains("id=\"n1\""), "{html}");
        assert!(html.contains("data-fable-link=\"0\""), "{html}");
    }

    #[test]
    fn rich_font_face_dies_when_the_font_is_encrypted() {
        let enc = r#"<?xml version="1.0"?>
<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container"
  xmlns:enc="http://www.w3.org/2001/04/xmlenc#">
  <enc:EncryptedData><enc:CipherData><enc:CipherReference URI="OEBPS/fonts/book.ttf"/></enc:CipherData></enc:EncryptedData>
</encryption>"#;
        let entries: Vec<(&str, &[u8])> = vec![
            ("mimetype", b"application/epub+zip"),
            ("META-INF/container.xml", RCONTAINER.as_bytes()),
            ("META-INF/encryption.xml", enc.as_bytes()),
            ("OEBPS/content.opf", ROPF.as_bytes()),
            ("OEBPS/css/book.css", RCSS.as_bytes()),
            ("OEBPS/css/extra.css", RXCSS.as_bytes()),
            ("OEBPS/images/fig.png", PNG_BYTES),
            ("OEBPS/fonts/book.ttf", TTF_BYTES),
            ("OEBPS/text/ch1.xhtml", RCH1.as_bytes()),
            ("OEBPS/text/ch2.xhtml", RCH2.as_bytes()),
            ("OEBPS/text/notes.xhtml", RNOTES.as_bytes()),
        ];
        let rich = decode_epub_book_rich(&stored_zip(&entries), Some(0));
        // Font-only encryption is not DRM; the book still reads.
        assert!(!rich.drm_protected);
        let css = rich.rich[0]
            .as_ref()
            .and_then(|c| c.css.as_deref())
            .expect("css present")
            .to_string();
        assert!(!css.contains("@font-face"), "{css}");
        // …while the rest of the sheet survives.
        assert!(css.contains(".reader-pub .dropcap{"), "{css}");
    }

    #[test]
    fn rich_malformed_chapter_falls_back_to_text() {
        let broken = RCH1.replace(
            "<p id=\"local\">Local target.</p>",
            "<div><p id=\"local\">Local target.</div>",
        );
        let data = stored_zip(&[
            ("mimetype", b"application/epub+zip"),
            ("META-INF/container.xml", RCONTAINER.as_bytes()),
            ("OEBPS/content.opf", ROPF.as_bytes()),
            ("OEBPS/css/book.css", RCSS.as_bytes()),
            ("OEBPS/css/extra.css", RXCSS.as_bytes()),
            ("OEBPS/images/fig.png", PNG_BYTES),
            ("OEBPS/fonts/book.ttf", TTF_BYTES),
            ("OEBPS/text/ch1.xhtml", broken.as_bytes()),
            ("OEBPS/text/ch2.xhtml", RCH2.as_bytes()),
            ("OEBPS/text/notes.xhtml", RNOTES.as_bytes()),
        ]);
        let rich = decode_epub_book_rich(&data, Some(0));
        // The chapter still exists as text (the P1 surface)…
        assert!(rich.chapters[0].text.contains("Local target."));
        // …but its rich form honestly declines.
        let ch = rich.rich[0].as_ref().expect("rich slot");
        assert!(ch.html.is_none(), "html: {:?}", ch.html);
    }

    #[test]
    fn rich_missing_image_is_an_honest_placeholder() {
        let ch1 = RCH1.replace("../images/fig.png", "../images/gone.png");
        let data = stored_zip(&[
            ("mimetype", b"application/epub+zip"),
            ("META-INF/container.xml", RCONTAINER.as_bytes()),
            ("OEBPS/content.opf", ROPF.as_bytes()),
            ("OEBPS/css/book.css", RCSS.as_bytes()),
            ("OEBPS/css/extra.css", RXCSS.as_bytes()),
            ("OEBPS/images/fig.png", PNG_BYTES),
            ("OEBPS/fonts/book.ttf", TTF_BYTES),
            ("OEBPS/text/ch1.xhtml", ch1.as_bytes()),
            ("OEBPS/text/ch2.xhtml", RCH2.as_bytes()),
            ("OEBPS/text/notes.xhtml", RNOTES.as_bytes()),
        ]);
        let rich = decode_epub_book_rich(&data, Some(0));
        let html = rich.rich[0]
            .as_ref()
            .and_then(|c| c.html.as_deref())
            .expect("html present")
            .to_string();
        assert!(
            html.contains("<span class=\"fable-img-missing\">A figure</span>"),
            "{html}"
        );
    }
}
