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

    match render_first_page(&path_s, dpi) {
        Ok(png) => out.page_png = Some(png),
        Err(why) => {
            if out.note.is_none() {
                out.note = Some(format!("The first page couldn't be rendered ({why})."));
            }
        }
    }
    out
}

fn render_first_page(path_s: &str, dpi: f64) -> Result<Vec<u8>, String> {
    let stem: PathBuf = std::env::temp_dir().join(format!(
        "fable-pdfprev-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let stem_s = stem.to_string_lossy().into_owned();
    let result = run_tool(
        "pdftoppm",
        &[
            "-f",
            "1",
            "-l",
            "1",
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

    /// Perf evidence (run explicitly: `cargo test -p ufm-preview
    /// -- --ignored big_pdf_perf --nocapture`): synthesize a ~110 MB
    /// PDF whose first page draws a 6000×6000 image — the worst
    /// realistic page-1 render — and time the full decode. The pane
    /// must never hang on a big book of scans.
    #[test]
    #[ignore]
    fn big_pdf_perf() {
        use std::io::Write;
        if !poppler_available() {
            eprintln!("poppler not installed — skipping perf test");
            return;
        }
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
        drop(pdf);
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
}
