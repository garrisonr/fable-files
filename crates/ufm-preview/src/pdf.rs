//! PDF decoder: document metadata and a rendered first page.
//!
//! Rendering uses the poppler tools (`pdfinfo`, `pdftoppm`,
//! `pdftotext`) — the same `poppler-utils` package the shell already
//! shells to for PDF text extraction (see commands.rs
//! `extract_document_text`), so the AppImage gains no bundled binary
//! and no new native dependency. Pure-Rust PDF rasterization was
//! rejected: `lopdf` parses but does not render, and the available
//! pure renderers are too young to bet the preview pane on.
//!
//! Failure is a note, never an error: missing tools, encrypted files,
//! and oversize files all degrade to metadata (when readable) plus a
//! plain-language `note`, and the shell falls back to its text /
//! metadata card.

use serde::Serialize;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Files larger than this skip the page render (pdfinfo metadata is
/// still read — it only touches the cross-reference table). A 200 MB
/// book of scans previews as metadata + text instead of stalling the
/// pane on a render nobody asked to wait for.
pub const MAX_RENDER_BYTES: u64 = 200 * 1024 * 1024;

const PDFINFO_TIMEOUT: Duration = Duration::from_secs(10);
const RENDER_TIMEOUT: Duration = Duration::from_secs(60);
const TEXT_TIMEOUT: Duration = Duration::from_secs(15);
/// Excerpt: the first pages' text, capped — the pane shows ~50 lines.
const MAX_TEXT_CHARS: usize = 6000;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PdfPreview {
    pub title: Option<String>,
    pub author: Option<String>,
    pub page_count: Option<u32>,
    /// First page, rasterized to PNG (bounded width — see `decode_pdf`).
    /// Carried to the shell as bytes, never into JSON payloads.
    #[serde(skip)]
    pub page_png: Option<Vec<u8>>,
    /// Opening text (first pages), for the excerpt under the page.
    pub text: String,
    /// The file is encrypted (password-protected).
    pub encrypted: bool,
    /// Plain-language reason the preview is limited, when it is.
    pub note: Option<String>,
}

/// Run a tool with a hard timeout, capturing stdout. `Err` carries a
/// plain-language reason; a timeout kills the child.
fn run_tool(program: &str, args: &[&str], timeout: Duration) -> Result<Vec<u8>, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!("{program} isn't available on this system")
            } else {
                format!("couldn't start {program}: {e}")
            }
        })?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("{program} gave no output channel"))?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return Ok(reader.join().unwrap_or_default());
            }
            Ok(None) => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{program} took too long and was stopped"));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(format!("{program} failed: {e}")),
        }
    }
}

/// Parse a `pdfinfo` field ("Title", "Pages", …) from its output.
fn pdfinfo_field(info: &str, field: &str) -> Option<String> {
    let prefix = format!("{field}:");
    info.lines().find_map(|line| {
        line.strip_prefix(&prefix)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    })
}

/// Decode the PDF at `path`: metadata, first-page render (PNG, scaled
/// to at most `max_width_px` wide), and an opening-text excerpt.
/// `file_size` is the caller's stat of the same file (the render size
/// rail). Never fails hard — see the module docs.
pub fn decode_pdf(path: &Path, file_size: u64, max_width_px: u32) -> PdfPreview {
    decode_inner(path, file_size, max_width_px, true, true)
}

/// Page render only (no text pass) — the thumbnail path, where the
/// excerpt would be discarded anyway.
pub fn decode_pdf_page(path: &Path, file_size: u64, max_width_px: u32) -> PdfPreview {
    decode_inner(path, file_size, max_width_px, false, true)
}

/// Metadata + excerpt only (no render) — for when the shell already
/// holds a cached page image.
pub fn decode_pdf_metadata(path: &Path, file_size: u64) -> PdfPreview {
    decode_inner(path, file_size, 0, true, false)
}

fn decode_inner(
    path: &Path,
    file_size: u64,
    max_width_px: u32,
    want_text: bool,
    want_render: bool,
) -> PdfPreview {
    let mut out = PdfPreview::default();
    let path_s = path.to_string_lossy().into_owned();

    let info = match run_tool("pdfinfo", &[&path_s], PDFINFO_TIMEOUT) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(why) => {
            out.note = Some(if why.contains("isn't available") {
                "PDF page previews need the Poppler tools, which aren't installed — \
                 showing the file's details instead."
                    .into()
            } else {
                format!("This PDF couldn't be read ({why}).")
            });
            return out;
        }
    };
    if info.trim().is_empty() {
        out.note = Some("This file isn't a readable PDF.".into());
        return out;
    }
    out.title = pdfinfo_field(&info, "Title");
    out.author = pdfinfo_field(&info, "Author");
    out.page_count = pdfinfo_field(&info, "Pages").and_then(|v| v.parse().ok());
    out.encrypted = pdfinfo_field(&info, "Encrypted")
        .map(|v| v.starts_with("yes"))
        .unwrap_or(false);
    if out.encrypted {
        out.note = Some(
            "This PDF is password-protected, so Fable can't render its pages.".into(),
        );
        return out;
    }

    // Opening text (best-effort; scanned PDFs simply have none).
    if want_text {
        if let Ok(bytes) = run_tool(
            "pdftotext",
            &["-f", "1", "-l", "2", "-layout", &path_s, "-"],
            TEXT_TIMEOUT,
        ) {
            let text = String::from_utf8_lossy(&bytes);
            out.text = text.chars().take(MAX_TEXT_CHARS).collect();
        }
    }

    if !want_render {
        return out;
    }
    if file_size > MAX_RENDER_BYTES {
        out.note = Some(format!(
            "This PDF is very large ({}), so no page preview was rendered — \
             double-click opens it in your reader.",
            human_size(file_size)
        ));
        return out;
    }

    // Page size (points) → DPI that lands the render at max_width_px.
    let dpi = pdfinfo_field(&info, "Page size")
        .and_then(|s| {
            s.split_whitespace()
                .next()
                .and_then(|w| w.parse::<f64>().ok())
        })
        .map(|w_pts| ((max_width_px as f64) * 72.0 / w_pts).clamp(36.0, 220.0))
        .unwrap_or(110.0);

    match render_page_png(&path_s, 1, dpi) {
        Ok(png) => out.page_png = Some(png),
        Err(why) => {
            if out.note.is_none() {
                out.note = Some(format!("The first page couldn't be rendered ({why})."));
            }
        }
    }
    out
}

fn render_page_png(path_s: &str, page: u32, dpi: f64) -> Result<Vec<u8>, String> {
    let stem: PathBuf = std::env::temp_dir().join(format!(
        "fable-pdfprev-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let stem_s = stem.to_string_lossy().into_owned();
    let page_s = page.to_string();
    let result = run_tool(
        "pdftoppm",
        &[
            "-f",
            &page_s,
            "-l",
            &page_s,
            "-singlefile",
            "-png",
            "-r",
            &format!("{dpi:.0}"),
            path_s,
            &stem_s,
        ],
        RENDER_TIMEOUT,
    );
    let png_path = stem.with_extension("png");
    let bytes = std::fs::read(&png_path).map_err(|e| format!("no page image produced ({e})"));
    let _ = std::fs::remove_file(&png_path);
    result.and_then(|_| bytes)
}

// ---------------------------------------------------------------------------
// Reader support (PDF Reader build): whole-document open (metadata +
// outline) and single-page renders at reading resolution. The preview
// path above renders page 1 at pane width; the reader renders the
// page it's on, crisply, and nothing else — huge documents stay lazy.
// ---------------------------------------------------------------------------

const OUTLINE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PdfOutlineEntry {
    pub title: String,
    /// 1-based page the bookmark points at.
    pub page: u32,
    /// Nesting depth in the outline tree (0 = top level).
    pub depth: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PdfDocument {
    pub title: Option<String>,
    pub author: Option<String>,
    pub page_count: Option<u32>,
    pub encrypted: bool,
    /// Plain-language reason the reader is limited, when it is.
    pub note: Option<String>,
    pub page_width_pts: Option<f64>,
    pub page_height_pts: Option<f64>,
    /// Bookmarks in document order (empty when the PDF has none).
    pub outline: Vec<PdfOutlineEntry>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PdfPageRender {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Parse "612 x 792 pts" (pdfinfo "Page size") into (width, height).
fn parse_page_size(info: &str) -> (Option<f64>, Option<f64>) {
    let raw = pdfinfo_field(info, "Page size");
    let mut parts = raw
        .as_deref()
        .unwrap_or("")
        .split(|c: char| c == 'x' || c.is_whitespace())
        .filter(|s| !s.is_empty());
    let w = parts.next().and_then(|s| s.parse::<f64>().ok());
    let h = parts.next().and_then(|s| s.parse::<f64>().ok());
    (w, h)
}

/// One pdfinfo pass. `Err` is the plain-language reason the file
/// couldn't be read at all (tool missing / unreadable).
fn pdfinfo_text(path_s: &str) -> Result<String, String> {
    match run_tool("pdfinfo", &[path_s], PDFINFO_TIMEOUT) {
        Ok(bytes) => {
            let info = String::from_utf8_lossy(&bytes).into_owned();
            if info.trim().is_empty() {
                Err("This file isn't a readable PDF.".into())
            } else {
                Ok(info)
            }
        }
        Err(why) => Err(if why.contains("isn't available") {
            "Reading PDFs needs the Poppler tools, which aren't installed — \
             this file will still open in your usual reader app."
                .into()
        } else {
            format!("This PDF couldn't be read ({why}).")
        }),
    }
}

/// Open a PDF for the reader: metadata + outline, no page renders.
/// Encrypted and over-rail files come back with a `note` and no
/// outline — the reader shows the note, never a broken pane.
pub fn open_pdf_document(path: &Path, file_size: u64) -> PdfDocument {
    let mut out = PdfDocument::default();
    let path_s = path.to_string_lossy().into_owned();
    let info = match pdfinfo_text(&path_s) {
        Ok(info) => info,
        Err(note) => {
            out.note = Some(note);
            return out;
        }
    };
    out.title = pdfinfo_field(&info, "Title");
    out.author = pdfinfo_field(&info, "Author");
    out.page_count = pdfinfo_field(&info, "Pages").and_then(|v| v.parse().ok());
    out.encrypted = pdfinfo_field(&info, "Encrypted")
        .map(|v| v.starts_with("yes"))
        .unwrap_or(false);
    let (w, h) = parse_page_size(&info);
    out.page_width_pts = w;
    out.page_height_pts = h;
    if out.encrypted {
        out.note = Some(
            "This PDF is password-protected, so Fable can't render its pages.".into(),
        );
        return out;
    }
    if file_size > MAX_RENDER_BYTES {
        out.note = Some(format!(
            "This PDF is very large ({}), so the built-in reader can't open it — \
             it will still open in your usual reader app.",
            human_size(file_size)
        ));
        return out;
    }
    out.outline = extract_outline(&path_s).unwrap_or_default();
    out
}

/// Render one page (1-based) at `target_width_px` wide. Hard errors
/// are plain-language strings for the reader to show; the size rail
/// and the encryption gate run before any tool does.
pub fn render_pdf_page(
    path: &Path,
    file_size: u64,
    page: u32,
    target_width_px: u32,
) -> Result<PdfPageRender, String> {
    if page == 0 {
        return Err("That page doesn't exist in this PDF.".into());
    }
    if file_size > MAX_RENDER_BYTES {
        return Err(format!(
            "This PDF is very large ({}), so the built-in reader can't render its pages.",
            human_size(file_size)
        ));
    }
    let path_s = path.to_string_lossy().into_owned();
    let info = pdfinfo_text(&path_s)?;
    if pdfinfo_field(&info, "Encrypted")
        .map(|v| v.starts_with("yes"))
        .unwrap_or(false)
    {
        return Err("This PDF is password-protected, so Fable can't render its pages.".into());
    }
    if let Some(count) = pdfinfo_field(&info, "Pages").and_then(|v| v.parse::<u32>().ok()) {
        if page > count {
            return Err("That page doesn't exist in this PDF.".into());
        }
    }
    let (w_pts, _) = parse_page_size(&info);
    let dpi = w_pts
        .map(|w| ((target_width_px as f64) * 72.0 / w).clamp(36.0, 300.0))
        .unwrap_or(150.0);
    let png = render_page_png(&path_s, page, dpi)?;
    let (width, height) = png_dimensions(&png)
        .ok_or_else(|| "The page renderer produced an unreadable image.".to_string())?;
    Ok(PdfPageRender { png, width, height })
}

/// Width/height from a PNG's IHDR chunk.
fn png_dimensions(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24 || &png[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    Some((
        u32::from_be_bytes([png[16], png[17], png[18], png[19]]),
        u32::from_be_bytes([png[20], png[21], png[22], png[23]]),
    ))
}

/// Bookmarks via `pdftohtml -xml` — the only poppler tool that
/// exposes the outline. Its XML wraps the bookmarks in an <outline>
/// element; a nested group is a nested <outline> after its parent
/// item, so depth = open-outline count − 1 when the item appears.
/// Best-effort: any failure means "no bookmarks", never an error.
fn extract_outline(path_s: &str) -> Option<Vec<PdfOutlineEntry>> {
    let stem: PathBuf = std::env::temp_dir().join(format!(
        "fable-pdfoutline-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let xml_path = stem.with_extension("xml");
    let xml_s = xml_path.to_string_lossy().into_owned();
    let result = run_tool(
        "pdftohtml",
        &["-xml", "-i", "-hidden", path_s, &xml_s],
        OUTLINE_TIMEOUT,
    );
    let text = std::fs::read_to_string(&xml_path).ok();
    let _ = std::fs::remove_file(&xml_path);
    result.ok()?;
    let text = text?;
    Some(parse_outline_xml(&text))
}

fn parse_outline_xml(xml: &str) -> Vec<PdfOutlineEntry> {
    let mut entries = Vec::new();
    let mut depth_open = 0usize;
    let mut rest = xml;
    while let Some(lt) = rest.find('<') {
        let after = &rest[lt..];
        if after.starts_with("<outline>") {
            depth_open += 1;
            rest = &after["<outline>".len()..];
        } else if after.starts_with("</outline>") {
            depth_open = depth_open.saturating_sub(1);
            rest = &after["</outline>".len()..];
            if depth_open == 0 {
                break; // the document's single outline is done
            }
        } else if after.starts_with("<item") {
            let Some(gt) = after.find('>') else { break };
            let tag = &after[..=gt];
            let page = tag
                .split("page=\"")
                .nth(1)
                .and_then(|s| s.split('"').next())
                .and_then(|s| s.parse::<u32>().ok());
            let inner = &after[gt + 1..];
            // Item text runs to the next '<' (a nested <outline> or
            // the item's own close tag).
            let text_end = inner.find('<').unwrap_or(inner.len());
            let title = xml_unescape(inner[..text_end].trim());
            if let (Some(page), false) = (page, title.is_empty()) {
                entries.push(PdfOutlineEntry {
                    title,
                    page,
                    depth: depth_open.saturating_sub(1),
                });
            }
            rest = &inner[text_end..];
        } else {
            rest = &after[1..];
        }
    }
    entries
}

/// The five predefined XML entities + numeric character references —
/// all a bookmark title can contain.
fn xml_unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
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
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#')?.parse::<u32>().ok())
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => out.push(c),
            None => out.push_str(&tail[..=semi]),
        }
        rest = &tail[semi + 1..];
    }
    out.push_str(rest);
    out
}

fn human_size(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1} GB", bytes as f64 / (MB * 1024.0))
    } else {
        format!("{:.0} MB", bytes as f64 / MB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal one-page PDF (uncompressed content stream) with a
    /// document title — small enough to write by hand, real enough
    /// for poppler to read and render.
    fn fixture_pdf() -> Vec<u8> {
        let content = "BT /F1 24 Tf 72 720 Td (Hello Fixture) Tj ET";
        let objects: Vec<String> = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>"
                .into(),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
            "<< /Title (Fixture Title) /Author (Jane Tester) >>".into(),
        ];
        let mut pdf = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.push_str(&format!("{} 0 obj\n{body}\nendobj\n", i + 1));
        }
        let xref = pdf.len();
        pdf.push_str(&format!(
            "xref\n0 {}\n0000000000 65535 f \n",
            objects.len() + 1
        ));
        for off in &offsets {
            pdf.push_str(&format!("{off:010} 00000 n \n"));
        }
        pdf.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R /Info 6 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        ));
        pdf.into_bytes()
    }

    fn poppler_available() -> bool {
        Command::new("pdfinfo")
            .arg("-v")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
    }

    fn write_temp(name: &str, bytes: &[u8]) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "fable-phase4-test-{}-{name}",
            std::process::id()
        ));
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn decodes_metadata_and_renders_first_page() {
        if !poppler_available() {
            eprintln!("poppler not installed — skipping live render test");
            return;
        }
        let bytes = fixture_pdf();
        let path = write_temp("fixture.pdf", &bytes);
        let p = decode_pdf(&path, bytes.len() as u64, 800);
        let _ = std::fs::remove_file(&path);
        assert_eq!(p.page_count, Some(1));
        assert_eq!(p.title.as_deref(), Some("Fixture Title"));
        assert_eq!(p.author.as_deref(), Some("Jane Tester"));
        assert!(!p.encrypted);
        assert!(p.text.contains("Hello Fixture"), "text: {:?}", p.text);
        let png = p.page_png.expect("page rendered");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "not a PNG");
        // IHDR width/height: the render is bounded near the request.
        let w = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
        let h = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
        assert!((700..=900).contains(&w), "width {w}");
        assert!(h > w, "portrait page expected, got {w}x{h}");
    }

    #[test]
    fn oversize_pdf_skips_render_with_note() {
        // A sparse stand-in: the size rail must trip before any tool
        // runs, so the "file" doesn't even need to be a real PDF.
        let path = write_temp("huge.pdf", b"%PDF-1.4 sparse stand-in");
        let p = decode_pdf(&path, MAX_RENDER_BYTES + 1, 800);
        let _ = std::fs::remove_file(&path);
        assert!(p.page_png.is_none());
        // Either the size note (if pdfinfo somehow ran first — it
        // can't: pdfinfo fails on the stand-in) or a read note; what
        // matters is no render and no panic. With poppler present the
        // empty-info path reports unreadable; without it, unavailable.
        // Both are honest notes.
        assert!(p.note.is_some());
    }

    #[test]
    fn garbage_pdf_degrades_to_note() {
        let path = write_temp("garbage.pdf", b"definitely not a pdf");
        let p = decode_pdf(&path, 20, 800);
        let _ = std::fs::remove_file(&path);
        assert!(p.page_png.is_none());
        assert!(p.page_count.is_none());
        if poppler_available() {
            assert!(p.note.is_some());
        }
    }

    /// ~103 MB fixture whose first page draws a 6000×6000 image —
    /// the worst realistic page-1 render. Returns (path, size).
    fn write_big_fixture() -> (PathBuf, u64) {
        use std::io::Write;
        // Deterministic pseudo-random pixels (incompressible, so the
        // file is honestly ~108 MB of image data).
        let (w, h) = (6000usize, 6000usize);
        let mut raw = Vec::with_capacity(w * h * 3);
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        while raw.len() < w * h * 3 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            raw.extend_from_slice(&x.to_le_bytes());
        }
        raw.truncate(w * h * 3);
        let mut comp = Vec::new();
        {
            let mut enc = flate2::write::ZlibEncoder::new(
                &mut comp,
                flate2::Compression::new(1),
            );
            enc.write_all(&raw).unwrap();
            enc.finish().unwrap();
        }
        drop(raw);

        let mut pdf: Vec<u8> = Vec::new();
        let mut offsets: Vec<usize> = Vec::new();
        let mut push_obj = |pdf: &mut Vec<u8>, offsets: &mut Vec<usize>, body: &[u8]| {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n", offsets.len()).as_bytes());
            pdf.extend_from_slice(body);
            pdf.extend_from_slice(b"\nendobj\n");
        };
        push_obj(&mut pdf, &mut offsets, b"<< /Type /Catalog /Pages 2 0 R >>");
        push_obj(&mut pdf, &mut offsets, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push_obj(
            &mut pdf,
            &mut offsets,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
              /Resources << /XObject << /Im1 5 0 R >> >> >>",
        );
        let content = b"q 612 0 0 792 0 0 cm /Im1 Do Q";
        let mut cobj = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
        cobj.extend_from_slice(content);
        cobj.extend_from_slice(b"\nendstream");
        push_obj(&mut pdf, &mut offsets, &cobj);
        let mut iobj = format!(
            "<< /Type /XObject /Subtype /Image /Width {w} /Height {h} \
             /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode \
             /Length {} >>\nstream\n",
            comp.len()
        )
        .into_bytes();
        iobj.extend_from_slice(&comp);
        iobj.extend_from_slice(b"\nendstream");
        push_obj(&mut pdf, &mut offsets, &iobj);
        push_obj(&mut pdf, &mut offsets, b"<< /Title (Big Fixture) >>");
        let xref = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes());
        for off in &offsets {
            pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R /Info 6 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                offsets.len() + 1
            )
            .as_bytes(),
        );
        let size = pdf.len() as u64;
        assert!(size > 100 * 1024 * 1024, "fixture only {size} bytes");
        let path = write_temp("big.pdf", &pdf);
        (path, size)
    }

    /// Perf evidence (run explicitly: `cargo test -p ufm-preview
    /// -- --ignored big_pdf_perf --nocapture`): time the full preview
    /// decode of the ~103 MB fixture. The pane must never hang on a
    /// big book of scans.
    #[test]
    #[ignore]
    fn big_pdf_perf() {
        if !poppler_available() {
            eprintln!("poppler not installed — skipping perf test");
            return;
        }
        let (path, size) = write_big_fixture();
        let t0 = std::time::Instant::now();
        let p = decode_pdf(&path, size, 1000);
        let elapsed = t0.elapsed();
        let _ = std::fs::remove_file(&path);
        println!(
            "big_pdf_perf: file {:.0} MB, decode {:?}, pages {:?}, png {} bytes",
            size as f64 / 1_048_576.0,
            elapsed,
            p.page_count,
            p.page_png.as_ref().map(|v| v.len()).unwrap_or(0)
        );
        assert_eq!(p.page_count, Some(1));
        assert!(p.page_png.is_some(), "no render: {:?}", p.note);
        assert!(
            elapsed < Duration::from_secs(60),
            "decode took {elapsed:?} — the pane would feel hung"
        );
    }

    /// Perf evidence for the reader path (run explicitly:
    /// `-- --ignored reader_page_perf --nocapture`): render one page
    /// of the ~103 MB fixture at reading width (1664 px) and time it.
    #[test]
    #[ignore]
    fn reader_page_perf() {
        if !poppler_available() {
            eprintln!("poppler not installed — skipping perf test");
            return;
        }
        let (path, size) = write_big_fixture();
        let doc = open_pdf_document(&path, size);
        assert_eq!(doc.page_count, Some(1));
        let t0 = std::time::Instant::now();
        let r = render_pdf_page(&path, size, 1, 1664).expect("page renders");
        let elapsed = t0.elapsed();
        let _ = std::fs::remove_file(&path);
        println!(
            "reader_page_perf: file {:.0} MB, render {:?}, page {}x{}, png {} bytes",
            size as f64 / 1_048_576.0,
            elapsed,
            r.width,
            r.height,
            r.png.len()
        );
        assert!((1600..=1730).contains(&r.width), "width {}", r.width);
        assert!(
            elapsed < Duration::from_secs(60),
            "render took {elapsed:?} — the reader would feel hung"
        );
    }

    /// Three-page fixture with a real /Outlines tree (one nested
    /// item) — the reader's bookmark evidence. Page objects: 3,4,5;
    /// outline root 7; items 8,9,10; nested item 15 under 9.
    fn fixture_pdf_outlined() -> Vec<u8> {
        let content = "BT /F1 24 Tf 72 720 Td (Outlined fixture page) Tj ET";
        let stream = format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len());
        let page = "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Contents 6 0 R /Resources << /Font << /F1 12 0 R >> >> >>";
        let objects: Vec<String> = vec![
            "<< /Type /Catalog /Pages 2 0 R /Outlines 7 0 R /PageMode /UseOutlines >>".into(),
            "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>".into(),
            page.into(),
            page.into(),
            page.into(),
            stream,
            "<< /Type /Outlines /First 8 0 R /Last 10 0 R /Count 4 >>".into(),
            "<< /Title (Chapter One) /Parent 7 0 R /Next 9 0 R /Dest [3 0 R /XYZ 0 792 null] >>"
                .into(),
            "<< /Title (Chapter Two & More) /Parent 7 0 R /Prev 8 0 R /Next 10 0 R \
             /First 15 0 R /Last 15 0 R /Count 1 /Dest [4 0 R /XYZ 0 792 null] >>"
                .into(),
            "<< /Title (Finale) /Parent 7 0 R /Prev 9 0 R /Dest [5 0 R /XYZ 0 792 null] >>"
                .into(),
            "<< /Title (Outlined Fixture) /Author (Pat Outline) >>".into(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
            "<< /Length 5 >>\nstream\nBT ET\nendstream".into(),
            "<< /Length 5 >>\nstream\nBT ET\nendstream".into(),
            "<< /Title (Nested & Deep) /Parent 9 0 R /Dest [5 0 R /XYZ 0 792 null] >>".into(),
        ];
        assemble_pdf(&objects, 11, None)
    }

    /// Assemble objects (1-based, in order) + xref + trailer. `info`
    /// is the /Info object number; `extra_trailer` is inserted raw
    /// into the trailer dict (the encrypted fixture's /Encrypt + /ID).
    fn assemble_pdf(
        objects: &[String],
        info: usize,
        extra_trailer: Option<&str>,
    ) -> Vec<u8> {
        let mut pdf = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.push_str(&format!("{} 0 obj\n{body}\nendobj\n", i + 1));
        }
        let xref = pdf.len();
        pdf.push_str(&format!(
            "xref\n0 {}\n0000000000 65535 f \n",
            objects.len() + 1
        ));
        for off in &offsets {
            pdf.push_str(&format!("{off:010} 00000 n \n"));
        }
        let extra = extra_trailer.unwrap_or("");
        pdf.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R /Info {info} 0 R {extra}>>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        ));
        pdf.into_bytes()
    }

    #[test]
    fn outline_xml_parsing_nests_and_unescapes() {
        // The exact shape pdftohtml 24.02 emits (verified on the box
        // against a hand-built outlined fixture): a nested group is
        // a nested <outline> after its parent item.
        let xml = "<pdf2xml>\n<page number=\"1\"><text>Page-1</text></page>\n\
                   <outline>\n\
                   <item page=\"1\">Chapter One</item>\n\
                   <item page=\"2\">Chapter Two &amp; More</item>\n\
                   <outline>\n\
                   <item page=\"3\">Nested &amp; Deep</item>\n\
                   </outline>\n\
                   <item page=\"3\">Finale</item>\n\
                   </outline>\n</pdf2xml>";
        let entries = parse_outline_xml(xml);
        assert_eq!(
            entries,
            vec![
                PdfOutlineEntry { title: "Chapter One".into(), page: 1, depth: 0 },
                PdfOutlineEntry { title: "Chapter Two & More".into(), page: 2, depth: 0 },
                PdfOutlineEntry { title: "Nested & Deep".into(), page: 3, depth: 1 },
                PdfOutlineEntry { title: "Finale".into(), page: 3, depth: 0 },
            ]
        );
        assert!(parse_outline_xml("<pdf2xml><page number=\"1\"/></pdf2xml>").is_empty());
        assert_eq!(xml_unescape("a &lt;b&gt; &#65;&#x42;"), "a <b> AB");
    }

    #[test]
    fn opens_outlined_document_and_renders_middle_page() {
        if !poppler_available() {
            eprintln!("poppler not installed — skipping live document test");
            return;
        }
        let bytes = fixture_pdf_outlined();
        let path = write_temp("outlined.pdf", &bytes);
        let doc = open_pdf_document(&path, bytes.len() as u64);
        assert_eq!(doc.page_count, Some(3));
        assert_eq!(doc.title.as_deref(), Some("Outlined Fixture"));
        assert_eq!(doc.author.as_deref(), Some("Pat Outline"));
        assert!(!doc.encrypted);
        assert_eq!(doc.note, None);
        assert_eq!(doc.page_width_pts, Some(612.0));
        assert_eq!(doc.page_height_pts, Some(792.0));
        assert_eq!(
            doc.outline,
            vec![
                PdfOutlineEntry { title: "Chapter One".into(), page: 1, depth: 0 },
                PdfOutlineEntry { title: "Chapter Two & More".into(), page: 2, depth: 0 },
                PdfOutlineEntry { title: "Nested & Deep".into(), page: 3, depth: 1 },
                PdfOutlineEntry { title: "Finale".into(), page: 3, depth: 0 },
            ],
            "outline from pdftohtml: {:?}",
            doc.outline
        );
        let page2 = render_pdf_page(&path, bytes.len() as u64, 2, 800).expect("page 2 renders");
        assert_eq!(&page2.png[..8], b"\x89PNG\r\n\x1a\n");
        assert!((760..=840).contains(&page2.width), "width {}", page2.width);
        assert!(page2.height > page2.width);
        let err = render_pdf_page(&path, bytes.len() as u64, 4, 800).unwrap_err();
        assert!(err.contains("doesn't exist"), "err: {err}");
        let err0 = render_pdf_page(&path, bytes.len() as u64, 0, 800).unwrap_err();
        assert!(err0.contains("doesn't exist"), "err: {err0}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn oversize_render_is_refused_before_any_tool() {
        let path = write_temp("huge2.pdf", b"%PDF-1.4 sparse stand-in");
        let err = render_pdf_page(&path, MAX_RENDER_BYTES + 1, 1, 800).unwrap_err();
        let _ = std::fs::remove_file(&path);
        assert!(err.contains("very large"), "err: {err}");
    }

    // --- Encrypted fixture: Standard Security Handler R2 (40-bit
    // RC4, empty passwords), implemented from PDF 32000 §7.6 so the
    // suite can prove the encryption gate against a file poppler
    // itself reports as Encrypted: yes. RC4 is a stream cipher, so
    // encryption preserves lengths and the xref stays valid.

    const PADDING: [u8; 32] = [
        0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA,
        0x01, 0x08, 0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE,
        0x64, 0x53, 0x69, 0x7A,
    ];

    fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
        let mut s: Vec<u8> = (0..=255).collect();
        let mut j = 0usize;
        for i in 0..256 {
            j = (j + s[i] as usize + key[i % key.len()] as usize) % 256;
            s.swap(i, j);
        }
        let (mut i, mut jj) = (0usize, 0usize);
        data.iter()
            .map(|b| {
                i = (i + 1) % 256;
                jj = (jj + s[i] as usize) % 256;
                s.swap(i, jj);
                b ^ s[(s[i] as usize + s[jj] as usize) % 256]
            })
            .collect()
    }

    fn pad_password(pwd: &[u8]) -> [u8; 32] {
        let mut out = [0u8; 32];
        let n = pwd.len().min(32);
        out[..n].copy_from_slice(&pwd[..n]);
        out[n..].copy_from_slice(&PADDING[..32 - n]);
        out
    }

    fn fixture_pdf_encrypted() -> Vec<u8> {
        let file_id: [u8; 16] = *b"fablefixture0001";
        let perms: i32 = -3904;
        // Owner key + /O (Algorithm 3, R2: single RC4 pass).
        let owner_digest = md5::compute(pad_password(b""));
        let o_value = rc4(&owner_digest.0[..5], &pad_password(b""));
        // File key (Algorithm 2).
        let mut key_input = Vec::new();
        key_input.extend_from_slice(&pad_password(b""));
        key_input.extend_from_slice(&o_value);
        key_input.extend_from_slice(&perms.to_le_bytes());
        key_input.extend_from_slice(&file_id);
        let file_key = md5::compute(&key_input).0[..5].to_vec();
        let u_value = rc4(&file_key, &PADDING);
        let obj_key = |obj: u32| -> Vec<u8> {
            let mut input = file_key.clone();
            input.extend_from_slice(&obj.to_le_bytes()[..3]);
            input.extend_from_slice(&0u16.to_le_bytes());
            md5::compute(&input).0[..10].to_vec()
        };
        let hex = |bytes: &[u8]| -> String {
            bytes.iter().map(|b| format!("{b:02x}")).collect()
        };
        // Object 4: the content stream, encrypted as a raw blob —
        // the fixture is assembled at byte level because encrypted
        // bytes aren't valid UTF-8 and must not pass through a
        // String.
        let content = "BT /F1 24 Tf 72 720 Td (Hello Fixture) Tj ET";
        let enc_stream = rc4(&obj_key(4), content.as_bytes());
        let mut stream_obj = format!("<< /Length {} >>\nstream\n", enc_stream.len()).into_bytes();
        stream_obj.extend_from_slice(&enc_stream);
        stream_obj.extend_from_slice(b"\nendstream");
        // Object 6: info strings as hex (encrypted bytes aren't safe
        // inside a literal string).
        let enc_title = rc4(&obj_key(6), b"Fixture Title");
        let enc_author = rc4(&obj_key(6), b"Jane Tester");
        let info_obj = format!(
            "<< /Title <{}> /Author <{}> >>",
            hex(&enc_title),
            hex(&enc_author)
        );
        let encrypt_obj = format!(
            "<< /Filter /Standard /V 1 /R 2 /Length 40 /O <{}> /U <{}> /P {perms} >>",
            hex(&o_value),
            hex(&u_value)
        );
        let text_obj = |s: &str| s.as_bytes().to_vec();
        let objects: Vec<Vec<u8>> = vec![
            text_obj("<< /Type /Catalog /Pages 2 0 R >>"),
            text_obj("<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            text_obj(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                 /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
            ),
            stream_obj,
            text_obj("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
            text_obj(&info_obj),
            text_obj(&encrypt_obj),
        ];
        let id_hex = hex(&file_id);
        let mut pdf: Vec<u8> = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            pdf.extend_from_slice(body);
            pdf.extend_from_slice(b"\nendobj\n");
        }
        let xref = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for off in &offsets {
            pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R /Info 6 0 R /Encrypt 7 0 R \
                 /ID [<{id_hex}><{id_hex}>] >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }

    #[test]
    fn encrypted_pdf_is_gated_with_a_plain_note() {
        if !poppler_available() {
            eprintln!("poppler not installed — skipping encrypted fixture test");
            return;
        }
        let bytes = fixture_pdf_encrypted();
        let path = write_temp("encrypted.pdf", &bytes);
        let size = bytes.len() as u64;
        let doc = open_pdf_document(&path, size);
        assert!(doc.encrypted, "pdfinfo must see the fixture as encrypted: {doc:?}");
        assert_eq!(doc.page_count, Some(1));
        let note = doc.note.clone().expect("encrypted note");
        assert!(note.contains("password-protected"), "note: {note}");
        assert!(doc.outline.is_empty());
        // The preview path agrees (same gate, Phase 4 behavior).
        let p = decode_pdf(&path, size, 800);
        assert!(p.encrypted);
        assert!(p.page_png.is_none());
        // And the reader render refuses with the same plain words.
        let err = render_pdf_page(&path, size, 1, 800).unwrap_err();
        assert!(err.contains("password-protected"), "err: {err}");
        let _ = std::fs::remove_file(&path);
    }
}
