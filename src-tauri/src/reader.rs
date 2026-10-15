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

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use ufm_preview::epub::decode_epub_book;

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
    pub chapter_count: usize,
}

#[tauri::command]
pub async fn read_chapter(path: String, index: usize) -> Result<ChapterView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let bytes = read_book_bytes(Path::new(&path))?;
        let book = decode_epub_book(&bytes);
        let chapter_count = book.chapters.len();
        let chapter = book
            .chapters
            .into_iter()
            .nth(index)
            .ok_or_else(|| "That chapter doesn't exist in this book.".to_string())?;
        Ok(ChapterView {
            index,
            title: chapter.title,
            text: chapter.text,
            chapter_count,
        })
    })
    .await
    .map_err(|e| format!("reader task failed: {e}"))?
}

/// Persist the reading position (and optionally the type scale) for
/// a book. Called by the reader on navigation and on close.
#[tauri::command]
pub fn set_reading_progress(
    path: String,
    chapter: usize,
    font_scale: Option<f32>,
) -> Result<(), String> {
    let mut progress = load_progress();
    let entry = progress.books.entry(path).or_insert(ProgressEntry {
        chapter: 0,
        font_scale: 1.0,
        updated: String::new(),
    });
    entry.chapter = chapter;
    if let Some(scale) = font_scale {
        entry.font_scale = scale.clamp(0.7, 2.2);
    }
    entry.updated = chrono::Utc::now().to_rfc3339();
    save_progress(&progress)
}
