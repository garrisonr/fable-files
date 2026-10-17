//! Phase 6 — ebook reader backend (spec §25).
//!
//! Built on the Phase 4 EPUB decoder (`ufm_preview::epub`), extended
//! with `decode_epub_book`, which returns the whole book chapter by
//! chapter in spine order. This module owns the shell side: the file
//! size rail, and reading progress persistence.
//!
//! Progress lives in `reading-progress.json` with the app's other
//! sidecars (`~/.local/share/fable-files/`), keyed by absolute path —
//! the same keying as labels, stars, and collections. It records the
//! chapter per book and the reader's type scale.
//!
//! Scope honesty: EPUB only. PDFs get the Phase 4 rendered preview;
//! a paginated PDF reading surface is a separate build (named in the
//! Phase 6 audit for Dickles' ruling). The double-click law is
//! unchanged — the reader opens from Quick Look's Read button or
//! Open With, never by hijacking the default.
//!
//! PDF READER (Dickles' ruling 2026-10-15 07:40): the paginated
//! surface arrived as the second half of this module. Same progress
//! sidecar (entries gain optional `page` / `zoom_mode` — EPUB
//! entries serialize exactly as before), same doors (Quick Look's
//! Read button, Open With ▸ Fable Reader), double-click law still
//! unchanged. Pages render through ufm-preview's poppler path at
//! reading width and are cached in the freedesktop family's style —
//! md5(file-uri) naming, mtime freshness — one app-private tier:
//! `~/.cache/fable-files/pdf-pages/<md5>-p<page>-w<bucket>.png`.
//!
//! THE READING ROOM (Ebook P2, 2026-10-17): chapters now arrive
//! RICH — `decode_epub_book_rich` returns the publisher's own
//! sanitized XHTML + scoped CSS for the chapter being read, beside
//! the same chapter text as always (the text stays the fallback
//! and the read-aloud path). Progress entries gain an optional
//! `scroll_frac` (within-chapter place, 0..=1) so reopening lands
//! on the same place, not just the same chapter; entries without
//! it serialize exactly as P1 wrote them.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use ufm_preview::epub::{decode_epub_book, decode_epub_book_rich};

/// Books larger than this get a plain-language refusal instead of a
/// long, memory-hungry decode.
const MAX_BOOK_BYTES: u64 = 200 * 1024 * 1024;

fn data_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".local/share/fable-files")
}

fn progress_path() -> PathBuf {
    data_dir().join("reading-progress.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProgressEntry {
    chapter: usize,
    #[serde(default = "default_font_scale")]
    font_scale: f32,
    #[serde(default)]
    updated: String,
    /// PDF position (0-based page). Absent on EPUB entries, which
    /// serialize exactly as they did before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    page: Option<usize>,
    /// PDF zoom mode ("fit-page", "fit-width", or a percent string
    /// like "125"). Absent on EPUB entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    zoom_mode: Option<String>,
    /// EPUB within-chapter place (Ebook P2): the fraction of the
    /// chapter's scrollable height the reader had reached, 0..=1.
    /// Absent on entries P2 hasn't touched and on PDF entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scroll_frac: Option<f64>,
}

fn default_font_scale() -> f32 {
    1.0
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ReadingProgress {
    #[serde(default)]
    books: HashMap<String, ProgressEntry>,
}

fn load_progress() -> ReadingProgress {
    std::fs::read_to_string(progress_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_progress(progress: &ReadingProgress) -> Result<(), String> {
    let dir = data_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let tmp = dir.join("reading-progress.json.tmp");
    let text = serde_json::to_string_pretty(progress).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, progress_path()).map_err(|e| e.to_string())
}

fn read_book_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let md = std::fs::metadata(path).map_err(|_| "That book no longer exists.".to_string())?;
    if !md.is_file() {
        return Err("That isn't a book file.".into());
    }
    if md.len() > MAX_BOOK_BYTES {
        return Err(
            "That book is larger than 200 MB, so the built-in reader can't open it. \
             It will still open in your usual reader app."
                .into(),
        );
    }
    std::fs::read(path).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Commands.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BookOpenView {
    pub title: Option<String>,
    pub author: Option<String>,
    pub chapter_count: usize,
    pub drm_protected: bool,
    pub note: Option<String>,
    /// Where the reader left off (0-based, clamped to the book).
    pub start_chapter: usize,
    pub font_scale: f32,
    /// Decoded chapter titles in spine order (P2) — the chapter
    /// menu is complete from the moment the book opens, instead of
    /// learning titles only for chapters already visited.
    pub chapter_titles: Vec<Option<String>>,
    /// The saved within-chapter place for `start_chapter` (P2),
    /// when the progress entry carries one.
    pub start_scroll_frac: Option<f64>,
}

#[tauri::command]
pub async fn open_book(path: String) -> Result<BookOpenView, String> {
    let progress = load_progress();
    let entry = progress.books.get(&path).cloned();
    tauri::async_runtime::spawn_blocking(move || {
        let bytes = read_book_bytes(Path::new(&path))?;
        let book = decode_epub_book(&bytes);
        let chapter_count = book.chapters.len();
        let start_chapter = entry
            .as_ref()
            .map(|e| e.chapter.min(chapter_count.saturating_sub(1)))
            .unwrap_or(0);
        Ok(BookOpenView {
            title: book.title,
            author: book.author,
            chapter_count,
            drm_protected: book.drm_protected,
            note: book.note,
            start_chapter,
            font_scale: entry
                .as_ref()
                .map(|e| e.font_scale)
                .unwrap_or(1.0)
                .clamp(0.7, 2.2),
            chapter_titles: book.chapters.iter().map(|c| c.title.clone()).collect(),
            start_scroll_frac: entry.as_ref().and_then(|e| e.scroll_frac),
        })
    })
    .await
    .map_err(|e| format!("reader task failed: {e}"))?
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChapterView {
    pub index: usize,
    pub title: Option<String>,
    pub text: String,
    /// The chapter's sanitized XHTML for the rich surface (P2),
    /// when the markup parsed. Absent → the reader renders `text`
    /// exactly as P1 did.
    pub html: Option<String>,
    /// The chapter's publisher CSS, sanitized and scoped to the
    /// reader's `.reader-pub` container (P2). Absent when the
    /// chapter uses no stylesheets or style blocks.
    pub css: Option<String>,
    pub chapter_count: usize,
}

#[tauri::command]
pub async fn read_chapter(path: String, index: usize) -> Result<ChapterView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let bytes = read_book_bytes(Path::new(&path))?;
        let book = decode_epub_book_rich(&bytes, Some(index));
        let chapter_count = book.chapters.len();
        let rich = book.rich.into_iter().nth(index).flatten();
        let (html, css) = match rich {
            Some(r) => (r.html, r.css),
            None => (None, None),
        };
        let chapter = book
            .chapters
            .into_iter()
            .nth(index)
            .ok_or_else(|| "That chapter doesn't exist in this book.".to_string())?;
        Ok(ChapterView {
            index,
            title: chapter.title,
            text: chapter.text,
            html,
            css,
            chapter_count,
        })
    })
    .await
    .map_err(|e| format!("reader task failed: {e}"))?
}

/// Persist the reading position (and optionally the type scale and
/// the within-chapter place) for a book. Called by the reader on
/// navigation, on scroll settle, and on close.
#[tauri::command]
pub fn set_reading_progress(
    state: tauri::State<'_, crate::AppState>,
    path: String,
    chapter: usize,
    font_scale: Option<f32>,
    scroll_frac: Option<f64>,
) -> Result<(), String> {
    let mut progress = load_progress();
    let entry = progress.books.entry(path.clone()).or_insert(ProgressEntry {
        chapter: 0,
        font_scale: 1.0,
        updated: String::new(),
        page: None,
        zoom_mode: None,
        scroll_frac: None,
    });
    let chapter_changed = entry.chapter != chapter;
    entry.chapter = chapter;
    match scroll_frac {
        Some(frac) => entry.scroll_frac = Some(frac.clamp(0.0, 1.0)),
        // A chapter change without a fresh place must not inherit
        // the old chapter's place.
        None if chapter_changed => entry.scroll_frac = None,
        None => {}
    }
    if let Some(scale) = font_scale {
        entry.font_scale = scale.clamp(0.7, 2.2);
    }
    entry.updated = chrono::Utc::now().to_rfc3339();
    let updated = entry.updated.clone();
    save_progress(&progress)?;
    // Ebook P1: progress existing IS the 'reading' state — the
    // catalog's touch rules decide what automation may change.
    let catalog = state.catalog.lock().unwrap();
    let _ = catalog.touch_book_reading(&path, &updated);
    Ok(())
}

/// One book's place in the reader's progress store (Books Home's
/// Continue Reading source). Not a Tauri command — the room's
/// payload composes it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressView {
    pub path: String,
    pub chapter: Option<usize>,
    pub page: Option<usize>,
    /// Within-chapter place for EPUB entries (P2), so Continue
    /// Reading's percentage counts the part of the chapter the
    /// reader has actually read.
    pub scroll_frac: Option<f64>,
    pub updated: String,
}

/// Every book with saved progress, most recently read first.
/// Entries with no timestamp (pre-dating the field) sort last.
pub fn list_reading_progress() -> Vec<ProgressView> {
    let progress = load_progress();
    let mut views: Vec<ProgressView> = progress
        .books
        .iter()
        .map(|(path, e)| ProgressView {
            path: path.clone(),
            chapter: if e.page.is_some() { None } else { Some(e.chapter) },
            page: e.page,
            scroll_frac: if e.page.is_some() { None } else { e.scroll_frac },
            updated: e.updated.clone(),
        })
        .collect();
    views.sort_by(|a, b| b.updated.cmp(&a.updated));
    views
}

// ---------------------------------------------------------------------------
// PDF reader commands.
// ---------------------------------------------------------------------------

/// Render widths the page cache quantizes to — wide enough for a
/// crisp reading page on a TV, few enough that the cache doesn't
/// grow a variant per window size.
const PAGE_WIDTH_BUCKETS: [u32; 5] = [896, 1280, 1664, 2048, 2560];

fn page_width_bucket(requested: u32) -> u32 {
    PAGE_WIDTH_BUCKETS
        .iter()
        .copied()
        .find(|b| *b >= requested)
        .unwrap_or(2560)
}

fn pdf_cache_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".cache/fable-files/pdf-pages")
}

/// Percent-encode a path into a file:// URI (freedesktop thumbnail
/// spec) — the same encoding `commands::file_uri` uses, so a PDF's
/// digest matches the rest of the thumbnail family's naming law.
fn file_uri(path: &Path) -> String {
    let mut out = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn png_dimensions(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24 || &png[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    Some((
        u32::from_be_bytes([png[16], png[17], png[18], png[19]]),
        u32::from_be_bytes([png[20], png[21], png[22], png[23]]),
    ))
}

fn stat_pdf(path: &Path) -> Result<u64, String> {
    let md = std::fs::metadata(path).map_err(|_| "That document no longer exists.".to_string())?;
    if !md.is_file() {
        return Err("That isn't a document file.".into());
    }
    Ok(md.len())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfOpenView {
    pub title: Option<String>,
    pub author: Option<String>,
    pub page_count: usize,
    pub encrypted: bool,
    /// Plain-language reason the reader can't show pages, when it
    /// can't (encrypted, oversize, unreadable).
    pub note: Option<String>,
    pub outline: Vec<ufm_preview::pdf::PdfOutlineEntry>,
    /// Where the reader left off (0-based, clamped to the document).
    pub start_page: usize,
    pub zoom_mode: String,
    pub page_width_pts: Option<f64>,
    pub page_height_pts: Option<f64>,
}

#[tauri::command]
pub async fn open_pdf(path: String) -> Result<PdfOpenView, String> {
    let progress = load_progress();
    let entry = progress.books.get(&path).cloned();
    tauri::async_runtime::spawn_blocking(move || {
        let p = Path::new(&path);
        let size = stat_pdf(p)?;
        // Gauntlet G1: the reader is a user-invoked door, so
        // Poppler is ENSURED here (session-latched — one
        // declined prompt is respected for the session) instead
        // of the book opening onto a missing-tools note. The
        // receipt outranks the decoder's note when both speak.
        let receipt = crate::deps::ensure_tool_session("pdfinfo", "poppler").err();
        let doc = ufm_preview::pdf::open_pdf_document(p, size);
        let page_count = doc.page_count.unwrap_or(0) as usize;
        let start_page = entry
            .as_ref()
            .and_then(|e| e.page)
            .map(|pg| pg.min(page_count.saturating_sub(1)))
            .unwrap_or(0);
        Ok(PdfOpenView {
            title: doc.title,
            author: doc.author,
            page_count,
            encrypted: doc.encrypted,
            note: crate::commands::prefer_setup_receipt(doc.note, receipt),
            outline: doc.outline,
            start_page,
            zoom_mode: entry
                .as_ref()
                .and_then(|e| e.zoom_mode.clone())
                .unwrap_or_else(|| "fit-width".into()),
            page_width_pts: doc.page_width_pts,
            page_height_pts: doc.page_height_pts,
        })
    })
    .await
    .map_err(|e| format!("reader task failed: {e}"))?
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfPageView {
    /// 0-based page index (the reader's own numbering).
    pub index: usize,
    /// The rendered page as a data URL.
    pub image: String,
    pub width: u32,
    pub height: u32,
}

#[tauri::command]
pub async fn read_pdf_page(
    path: String,
    page: usize,
    width_px: u32,
) -> Result<PdfPageView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        use base64::Engine;
        let p = Path::new(&path);
        let size = stat_pdf(p)?;
        // Belt and braces (Gauntlet G1): open_pdf ensures
        // Poppler first in every real flow; if a page is ever
        // requested cold, the session-latched ensure runs here
        // too — free when the tools are already in place.
        let _ = crate::deps::ensure_tool_session("pdfinfo", "poppler");
        let bucket = page_width_bucket(width_px.max(320));
        let cache_dir = pdf_cache_dir();
        let digest = format!("{:x}", md5::compute(file_uri(p).as_bytes()));
        let cache_file = cache_dir.join(format!("{digest}-p{}-w{bucket}.png", page + 1));
        let src_mtime = std::fs::metadata(p).and_then(|m| m.modified()).ok();
        let cache_fresh = cache_file
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .zip(src_mtime)
            .map(|(cm, sm)| cm >= sm)
            .unwrap_or(false);
        let png = if cache_fresh {
            std::fs::read(&cache_file).ok()
        } else {
            None
        };
        let png = match png {
            Some(bytes) => bytes,
            None => {
                let render = ufm_preview::pdf::render_pdf_page(p, size, page as u32 + 1, bucket)?;
                let _ = std::fs::create_dir_all(&cache_dir);
                let tmp = cache_dir.join(format!(
                    "{digest}-p{}-w{bucket}.tmp.png",
                    page + 1
                ));
                if std::fs::write(&tmp, &render.png).is_ok() {
                    let _ = std::fs::rename(&tmp, &cache_file);
                }
                render.png
            }
        };
        let (width, height) = png_dimensions(&png)
            .ok_or_else(|| "The page renderer produced an unreadable image.".to_string())?;
        Ok(PdfPageView {
            index: page,
            image: format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(&png)
            ),
            width,
            height,
        })
    })
    .await
    .map_err(|e| format!("reader task failed: {e}"))?
}

/// Persist the PDF reading position (0-based page, and optionally
/// the zoom mode). Called by the reader on navigation, on zoom
/// change, and on close — the EPUB twin is `set_reading_progress`.
#[tauri::command]
pub fn set_pdf_progress(
    state: tauri::State<'_, crate::AppState>,
    path: String,
    page: usize,
    zoom_mode: Option<String>,
) -> Result<(), String> {
    let mut progress = load_progress();
    let entry = progress.books.entry(path.clone()).or_insert(ProgressEntry {
        chapter: 0,
        font_scale: 1.0,
        updated: String::new(),
        page: None,
        zoom_mode: None,
        scroll_frac: None,
    });
    entry.page = Some(page);
    if let Some(mode) = zoom_mode {
        entry.zoom_mode = Some(mode);
    }
    entry.updated = chrono::Utc::now().to_rfc3339();
    let updated = entry.updated.clone();
    save_progress(&progress)?;
    // Ebook P1: the EPUB twin above — progress means 'reading'.
    let catalog = state.catalog.lock().unwrap();
    let _ = catalog.touch_book_reading(&path, &updated);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_buckets_quantize_up() {
        assert_eq!(page_width_bucket(320), 896);
        assert_eq!(page_width_bucket(896), 896);
        assert_eq!(page_width_bucket(900), 1280);
        assert_eq!(page_width_bucket(1664), 1664);
        assert_eq!(page_width_bucket(4000), 2560);
    }

    #[test]
    fn progress_roundtrip_keeps_epub_entries_unchanged() {
        // A legacy sidecar written before PDFs joined the reader:
        // chapter-only entries must load with no page/zoom and
        // re-serialize WITHOUT the new keys (the EPUB surface never
        // sees a shape change).
        let legacy = r#"{"books":{"/books/a.epub":{"chapter":3,"font_scale":1.2,"updated":"2026-10-15T00:00:00Z"}}}"#;
        let progress: ReadingProgress = serde_json::from_str(legacy).unwrap();
        let entry = &progress.books["/books/a.epub"];
        assert_eq!(entry.chapter, 3);
        assert_eq!(entry.page, None);
        assert_eq!(entry.zoom_mode, None);
        let out = serde_json::to_string(&progress).unwrap();
        assert!(!out.contains("\"page\""), "epub entry gained a page key: {out}");
        assert!(!out.contains("zoom_mode"), "epub entry gained zoom: {out}");
        assert!(
            !out.contains("scroll_frac"),
            "legacy entry gained a scroll_frac key: {out}"
        );

        // A PDF entry round-trips its page + zoom mode.
        let mut progress = ReadingProgress::default();
        progress.books.insert(
            "/docs/b.pdf".into(),
            ProgressEntry {
                chapter: 0,
                font_scale: 1.0,
                updated: "2026-10-15T00:00:00Z".into(),
                page: Some(41),
                zoom_mode: Some("fit-page".into()),
                scroll_frac: None,
            },
        );
        let text = serde_json::to_string(&progress).unwrap();
        let back: ReadingProgress = serde_json::from_str(&text).unwrap();
        let entry = &back.books["/docs/b.pdf"];
        assert_eq!(entry.page, Some(41));
        assert_eq!(entry.zoom_mode.as_deref(), Some("fit-page"));
    }

    #[test]
    fn scroll_frac_roundtrips_on_epub_entries() {
        // P2: an EPUB entry that carries a within-chapter place
        // keeps it through a save/load cycle.
        let mut progress = ReadingProgress::default();
        progress.books.insert(
            "/books/c.epub".into(),
            ProgressEntry {
                chapter: 2,
                font_scale: 1.0,
                updated: "2026-10-17T00:00:00Z".into(),
                page: None,
                zoom_mode: None,
                scroll_frac: Some(0.45),
            },
        );
        let text = serde_json::to_string(&progress).unwrap();
        assert!(text.contains("scroll_frac"), "place was not saved: {text}");
        let back: ReadingProgress = serde_json::from_str(&text).unwrap();
        let entry = &back.books["/books/c.epub"];
        assert_eq!(entry.chapter, 2);
        assert_eq!(entry.scroll_frac, Some(0.45));
    }

    #[test]
    fn file_uri_matches_thumbnail_family_encoding() {
        assert_eq!(
            file_uri(Path::new("/home/x/My Doc #1.pdf")),
            "file:///home/x/My%20Doc%20%231.pdf"
        );
    }
}
