//! Ebook P1 — "The Library" (2026-10-17): book identification and
//! the Books Home data surface.
//!
//! Identification is the ES-import pattern grown up: a single-flight
//! pass (static guard, Rust-side progress events — never a webview
//! timer) that walks the catalog's stale book candidates, decodes
//! each file's own metadata (EPUB OPF Dublin Core + series via
//! `ufm_preview`; PDF info dictionary via the poppler path), caches
//! the cover, and writes the identity into the catalog
//! (`media_identities` media_kind 'ebook' + `book_metadata` +
//! `book_authors`, one transaction per book). The pass runs three
//! ways: automatically after an indexing run completes (scoped to
//! the indexed location), on Books Home's tidy-up door, and on first
//! Books Home open when unsorted books wait. Freshness is the
//! catalog's size+mtime snapshot contract, so re-runs skip settled
//! books and the pass is resumable by construction.
//!
//! Honesty rules, pinned in the catalog tests and here: a book that
//! can't be decoded lands in the report's failed bucket WITH its
//! reason and gets NO identity row (it stays visibly unsorted); a
//! book whose file parses but says almost nothing gets a row whose
//! title is its file stem — shelved, but never dressed in invented
//! metadata.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tauri::{Emitter, Manager};

use crate::commands::{Cmd, CommandError};
use crate::AppState;
use ufm_catalog::{BookMetadataRow, BookShelfRow};
use ufm_core::UfmError;

static BOOKS_IDENTIFY_RUNNING: AtomicBool = AtomicBool::new(false);

/// Metadata reads cap: an EPUB's OPF is a few KB and the decoder
/// works on the whole archive in memory; past this a "book" is an
/// archive in a trench coat and the pass says so instead of eating
/// the potato's RAM.
const MAX_BOOK_META_BYTES: u64 = 64 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Wire types.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BookCard {
    pub item_id: String,
    pub path: String,
    pub name: String,
    pub title: String,
    pub authors: Vec<String>,
    pub author_line: String,
    pub series: Option<String>,
    pub series_index: Option<f64>,
    pub year: Option<i64>,
    /// epub | pdf
    pub format: String,
    pub cover_path: Option<String>,
    /// want | reading | read
    pub state: Option<String>,
    pub publisher: Option<String>,
    pub language: Option<String>,
    pub subjects: Vec<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContinueRow {
    pub card: BookCard,
    pub chapter: Option<usize>,
    pub page: Option<usize>,
    /// Spine chapters (EPUB) or pages (PDF), when known.
    pub total: Option<i64>,
    /// 0..=1 along the book, when both position and total are known.
    pub fraction: Option<f64>,
    /// RFC 3339 of the last progress save — rendered, never faked.
    pub updated: String,
    /// The reader is at the final chapter/page: Books Home offers
    /// the one-tap "Mark as Read" (finishing is offered, never
    /// auto-declared — DESIGN P1).
    pub finish_candidate: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeriesShelfWire {
    pub series: String,
    pub books: Vec<BookCard>,
    pub present: Vec<i64>,
    pub missing: Vec<i64>,
    /// "Books 1, 3 of 4 · missing 2" — composed here so every
    /// surface reads the same honest sentence.
    pub gap_label: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorShelfWire {
    pub author: String,
    pub books: Vec<BookCard>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsortedCard {
    pub path: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BooksCountsWire {
    pub identified: u64,
    pub unsorted: u64,
    pub series: u64,
    pub authors: u64,
    pub want: u64,
    pub reading: u64,
    pub read: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BooksHomePayload {
    pub counts: BooksCountsWire,
    pub continue_reading: Vec<ContinueRow>,
    pub recently_added: Vec<BookCard>,
    pub series: Vec<SeriesShelfWire>,
    pub authors: Vec<AuthorShelfWire>,
    pub all: Vec<BookCard>,
    pub unsorted: Vec<UnsortedCard>,
    pub unsorted_total: u64,
    /// The Public Domain Shelf (Ebook P3): the frozen 48-work
    /// manifest composed against the live library — every row
    /// knows whether it's on the shelf, already his under
    /// another edition, or waiting to be added.
    pub pd_shelf: crate::pd::PdShelfWire,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentifyReport {
    pub already_running: bool,
    pub total: u64,
    pub identified: u64,
    pub with_series: u64,
    pub with_cover: u64,
    pub failed: u64,
    /// (file name, plain-language reason) — capped at 20 in the
    /// report; the count above is always the true total.
    pub failures: Vec<(String, String)>,
}

/// Progress for the self-organizing shelves: as each book resolves,
/// the room re-reads and the book settles onto its shelf. Emitted
/// from the blocking pass (Rust events, per Forge law).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BooksIdentifyProgress {
    done: u32,
    total: u32,
    identified: u32,
    failed: u32,
}

// ---------------------------------------------------------------------------
// Identification.
// ---------------------------------------------------------------------------

fn file_stem(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_string(),
        _ => name.to_string(),
    }
}

fn cover_dir(data_base: &Path) -> PathBuf {
    data_base.join("book-covers")
}

fn cover_ext(media_type: &str) -> &'static str {
    match media_type {
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "jpg",
    }
}

/// Cache one book's cover under the cover store, atomically
/// (tmp + rename). Returns the stored path; a cover that already
/// exists is kept — re-identification never re-writes art.
fn store_cover(data_base: &Path, provider_key: &str, cover: &ufm_preview::epub::EpubCover) -> Option<PathBuf> {
    let dir = cover_dir(data_base);
    std::fs::create_dir_all(&dir).ok()?;
    let digest = format!("{:x}", md5::compute(provider_key.as_bytes()));
    let path = dir.join(format!("{digest}.{}", cover_ext(&cover.media_type)));
    if path.exists() {
        return Some(path);
    }
    let tmp = dir.join(format!("{digest}.tmp"));
    std::fs::write(&tmp, &cover.data).ok()?;
    std::fs::rename(&tmp, &path).ok()?;
    Some(path)
}

enum Identified {
    Done { series: bool, cover: bool },
    Failed(String),
}

/// Identify one candidate item. All catalog writes go through the
/// managed catalog (one short lock per book — the app breathes
/// between books on a long pass).
fn identify_one(state: &AppState, item: &ufm_vfs::Item) -> Identified {
    let Some(pk) = item.provider_key.clone() else {
        return Identified::Failed("the catalog has no path for this file".into());
    };
    let path = Path::new(&pk);
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let size = item.size_bytes.unwrap_or(0);
    let now = chrono::Utc::now().to_rfc3339();
    let snapshot_modified = item.modified.map(|m| m.to_rfc3339());

    let row = match ext.as_str() {
        "epub" => {
            if size > MAX_BOOK_META_BYTES {
                return Identified::Failed(format!(
                    "this book is over {} MB, so the library pass doesn't open it",
                    MAX_BOOK_META_BYTES / (1024 * 1024)
                ));
            }
            let bytes = match std::fs::read(path) {
                Ok(b) => b,
                Err(e) => return Identified::Failed(format!("couldn't read the file: {e}")),
            };
            let Some(meta) = ufm_preview::epub::decode_epub_metadata(&bytes) else {
                return Identified::Failed(
                    "couldn't be read as an EPUB (its book index is missing or damaged)".into(),
                );
            };
            // Cover: the preview decoder's cover, cached once.
            let mut cover_path = None;
            let preview = ufm_preview::epub::decode_epub(&bytes);
            if let Some(cover) = &preview.cover {
                cover_path = store_cover(&state.data_base, &pk, cover)
                    .map(|p| p.to_string_lossy().into_owned());
            }
            let has_cover = cover_path.is_some();
            let row = BookMetadataRow {
                item_id: item.id.to_string(),
                provider_key: pk.clone(),
                format: "epub".into(),
                title: meta
                    .title
                    .clone()
                    .or_else(|| Some(file_stem(&item.name))),
                authors: meta.authors.clone(),
                series: meta.series.clone(),
                series_index: meta.series_index,
                publisher: meta.publisher.clone(),
                published_date: meta.published_date.clone(),
                pub_year: meta
                    .published_date
                    .as_deref()
                    .and_then(ufm_preview::epub::published_year),
                language: meta.language.clone(),
                description: meta.description.clone(),
                subjects: meta.subjects.clone(),
                isbn: meta.isbn.clone(),
                identifier: meta.identifier.clone(),
                spine_count: Some(meta.spine_count as i64),
                page_count: None,
                cover_path,
                source: "opf".into(),
                size_bytes: item.size_bytes.map(|s| s as i64),
                modified: snapshot_modified,
                identified_at: now,
            };
            let series = row.series.is_some();
            return finish_write(state, row, series, has_cover);
        }
        "pdf" => {
            let preview = ufm_preview::pdf::decode_pdf_metadata(path, size);
            if preview.encrypted {
                return Identified::Failed(
                    "this PDF is password-protected, so the library pass can't read its details"
                        .into(),
                );
            }
            BookMetadataRow {
                item_id: item.id.to_string(),
                provider_key: pk.clone(),
                format: "pdf".into(),
                title: preview.title.clone().or_else(|| Some(file_stem(&item.name))),
                authors: preview.author.iter().cloned().collect(),
                series: None,
                series_index: None,
                publisher: None,
                published_date: None,
                pub_year: None,
                language: None,
                description: None,
                subjects: Vec::new(),
                isbn: None,
                identifier: None,
                spine_count: None,
                page_count: preview.page_count.map(|p| p as i64),
                cover_path: None,
                source: "pdfinfo".into(),
                size_bytes: item.size_bytes.map(|s| s as i64),
                modified: snapshot_modified,
                identified_at: now,
            }
        }
        _ => return Identified::Failed("not a book file".into()),
    };
    finish_write(state, row, false, false)
}

fn finish_write(state: &AppState, row: BookMetadataRow, series: bool, cover: bool) -> Identified {
    let catalog = state.catalog.lock().unwrap();
    match catalog.upsert_book_metadata(&row) {
        Ok(()) => Identified::Done { series, cover },
        Err(e) => Identified::Failed(format!("the catalog refused this book: {e}")),
    }
}

fn run_identify(
    app: &tauri::AppHandle,
    state: &AppState,
    location_id: Option<String>,
) -> Cmd<IdentifyReport> {
    let candidates = {
        let catalog = state.catalog.lock().unwrap();
        catalog
            .book_candidates(location_id.as_deref())
            .map_err(CommandError::from)?
    };
    let total = candidates.len() as u32;
    let mut report = IdentifyReport {
        total: total as u64,
        ..Default::default()
    };
    let mut last_emit = std::time::Instant::now() - std::time::Duration::from_secs(1);
    for (i, item) in candidates.iter().enumerate() {
        match identify_one(state, item) {
            Identified::Done { series, cover } => {
                report.identified += 1;
                if series {
                    report.with_series += 1;
                }
                if cover {
                    report.with_cover += 1;
                }
            }
            Identified::Failed(reason) => {
                report.failed += 1;
                if report.failures.len() < 20 {
                    report.failures.push((item.name.clone(), reason));
                }
            }
        }
        let done = (i + 1) as u32;
        if done == total || last_emit.elapsed() >= std::time::Duration::from_millis(250) {
            last_emit = std::time::Instant::now();
            let _ = app.emit(
                "books-identify-progress",
                BooksIdentifyProgress {
                    done,
                    total,
                    identified: report.identified as u32,
                    failed: report.failed as u32,
                },
            );
        }
    }
    Ok(report)
}

/// Fire-and-forget identification for the post-index hook and the
/// Books Home first-open tidy. Never duplicates a running pass.
pub fn identify_in_background(app: tauri::AppHandle, location_id: Option<String>) {
    if BOOKS_IDENTIFY_RUNNING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    tauri::async_runtime::spawn_blocking(move || {
        let guard = app.state::<AppState>();
        let state: &AppState = &guard;
        let _ = run_identify(&app, state, location_id);
        BOOKS_IDENTIFY_RUNNING.store(false, Ordering::SeqCst);
    });
}

/// Identify every stale book in the library (or one location).
/// Single-flight: a call during a running pass reports
/// `alreadyRunning` and changes nothing.
#[tauri::command]
pub async fn books_identify(
    app: tauri::AppHandle,
    location_id: Option<String>,
) -> Cmd<IdentifyReport> {
    if BOOKS_IDENTIFY_RUNNING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Ok(IdentifyReport {
            already_running: true,
            ..Default::default()
        });
    }
    let app2 = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let guard = app2.state::<AppState>();
        let state: &AppState = &guard;
        run_identify(&app2, state, location_id)
    })
    .await;
    BOOKS_IDENTIFY_RUNNING.store(false, Ordering::SeqCst);
    match result {
        Ok(r) => r,
        Err(e) => Err(CommandError::from(UfmError::Internal(format!(
            "book identify task failed: {e}"
        )))),
    }
}

// ---------------------------------------------------------------------------
// Books Home payload.
// ---------------------------------------------------------------------------

fn card(row: &BookShelfRow) -> BookCard {
    let meta = &row.meta;
    BookCard {
        item_id: meta.item_id.clone(),
        path: meta.provider_key.clone(),
        name: row.item.name.clone(),
        title: meta
            .title
            .clone()
            .unwrap_or_else(|| file_stem(&row.item.name)),
        authors: meta.authors.clone(),
        author_line: meta.authors.join(", "),
        series: meta.series.clone(),
        series_index: meta.series_index,
        year: meta.pub_year,
        format: meta.format.clone(),
        cover_path: meta.cover_path.clone(),
        state: row.state.clone(),
        publisher: meta.publisher.clone(),
        language: meta.language.clone(),
        subjects: meta.subjects.clone(),
        description: meta.description.clone(),
    }
}

/// Progress fraction along a book (0..=1) and whether the reader is
/// at its end. EPUB positions are 0-based chapter indices over the
/// spine; PDF positions are 0-based pages over the page count.
/// Ebook P2: when the EPUB entry carries a within-chapter place,
/// the fraction counts the part of the current chapter actually
/// read — (chapter + scroll_frac) / total — instead of crediting
/// the whole chapter the moment it opens. Entries without a place
/// keep P1's math exactly. The finish rule never changes: being on
/// the final chapter is what offers Mark as Read.
fn progress_math(
    chapter: Option<usize>,
    page: Option<usize>,
    scroll_frac: Option<f64>,
    spine_count: Option<i64>,
    page_count: Option<i64>,
) -> (Option<f64>, bool) {
    if let (Some(ch), Some(total)) = (chapter, spine_count) {
        if total > 0 {
            let frac = match scroll_frac {
                Some(f) => ((ch as f64) + f.clamp(0.0, 1.0)) / (total as f64),
                None => ((ch as f64) + 1.0) / (total as f64),
            };
            return (Some(frac.min(1.0)), ch as i64 >= total - 1);
        }
    }
    if let (Some(pg), Some(total)) = (page, page_count) {
        if total > 0 {
            let frac = ((pg as f64) + 1.0) / (total as f64);
            return (Some(frac.min(1.0)), pg as i64 >= total - 1);
        }
    }
    (None, false)
}

/// Compose the whole room in one call — the counts, the shelves,
/// and the unsorted pile — so the UI never assembles a half-true
/// picture from racing queries.
#[tauri::command]
pub fn books_home(
    state: tauri::State<'_, AppState>,
    pd_hub: tauri::State<'_, crate::pd::PdHub>,
) -> Cmd<BooksHomePayload> {
    let catalog = state.catalog.lock().unwrap();
    let counts = catalog.books_counts().map_err(CommandError::from)?;
    let all_rows = catalog.books_all().map_err(CommandError::from)?;
    let series_shelves = catalog.books_series_shelves().map_err(CommandError::from)?;
    let author_shelves = catalog.books_author_shelves().map_err(CommandError::from)?;
    let unsorted_items = catalog.book_unsorted_items().map_err(CommandError::from)?;

    let by_path: std::collections::HashMap<String, &BookShelfRow> = all_rows
        .iter()
        .map(|r| (r.meta.provider_key.clone(), r))
        .collect();

    // Continue Reading: the reader's own progress store is the
    // source of truth; identity decorates it when it exists.
    let mut continue_reading: Vec<ContinueRow> = Vec::new();
    for entry in crate::reader::list_reading_progress() {
        let (card, spine, pages) = match by_path.get(&entry.path) {
            Some(row) => (
                card(row),
                row.meta.spine_count,
                row.meta.page_count,
            ),
            None => {
                let name = Path::new(&entry.path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| entry.path.clone());
                let format = Path::new(&entry.path)
                    .extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                (
                    BookCard {
                        item_id: String::new(),
                        path: entry.path.clone(),
                        title: file_stem(&name),
                        name,
                        authors: Vec::new(),
                        author_line: String::new(),
                        series: None,
                        series_index: None,
                        year: None,
                        format,
                        cover_path: None,
                        state: None,
                        publisher: None,
                        language: None,
                        subjects: Vec::new(),
                        description: None,
                    },
                    None,
                    None,
                )
            }
        };
        let (fraction, finish) = progress_math(
            entry.chapter,
            entry.page,
            entry.scroll_frac,
            spine,
            pages,
        );
        continue_reading.push(ContinueRow {
            total: spine.or(pages),
            chapter: entry.chapter,
            page: entry.page,
            fraction,
            updated: entry.updated,
            finish_candidate: finish,
            card,
        });
    }

    // Recently Added: the newest book files in the library
    // (file recency — the honest proxy the catalog carries).
    let mut by_recency = all_rows.clone();
    by_recency.sort_by(|a, b| {
        b.item
            .modified
            .cmp(&a.item.modified)
            .then(a.meta.title.clone().unwrap_or_default().cmp(&b.meta.title.clone().unwrap_or_default()))
    });
    let recently_added: Vec<BookCard> = by_recency.iter().take(24).map(card).collect();

    // The Public Domain Shelf, composed from the same rows the
    // room shows — one library truth, two surfaces.
    let pd_library: Vec<crate::pd::LibraryBook> = all_rows
        .iter()
        .map(|r| crate::pd::LibraryBook {
            title: r.meta.title.clone().unwrap_or_default(),
            authors: r.meta.authors.clone(),
            path: r.meta.provider_key.clone(),
        })
        .collect();
    let pd_pull_status = pd_hub.status.lock().unwrap().clone();
    let pd_shelf = crate::pd::compose_shelf(&pd_library, &pd_pull_status);

    Ok(BooksHomePayload {
        counts: BooksCountsWire {
            identified: counts.identified,
            unsorted: counts.unsorted,
            series: counts.series,
            authors: counts.authors,
            want: counts.want,
            reading: counts.reading,
            read: counts.read,
        },
        continue_reading,
        recently_added,
        series: series_shelves
            .iter()
            .map(|s| SeriesShelfWire {
                series: s.series.clone(),
                books: s.books.iter().map(card).collect(),
                present: s.present.clone(),
                missing: s.missing.clone(),
                gap_label: gap_label(s),
            })
            .collect(),
        authors: author_shelves
            .iter()
            .map(|(author, books)| AuthorShelfWire {
                author: author.clone(),
                books: books.iter().map(card).collect(),
            })
            .collect(),
        all: all_rows.iter().map(card).collect(),
        unsorted: unsorted_items
            .iter()
            .take(60)
            .map(|i| UnsortedCard {
                path: i.provider_key.clone().unwrap_or_default(),
                name: i.name.clone(),
            })
            .collect(),
        unsorted_total: counts.unsorted,
        pd_shelf,
    })
}

/// "Books 1, 3 of 4 · missing 2" — the gap sentence for a series
/// shelf header. None when the shelf is complete or holds no
/// whole-numbered books at all.
fn gap_label(shelf: &ufm_catalog::SeriesShelf) -> Option<String> {
    if shelf.present.is_empty() {
        return None;
    }
    let max = shelf.present[shelf.present.len() - 1];
    let present = shelf
        .present
        .iter()
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    if shelf.missing.is_empty() {
        Some(format!("Books {present} of {max} — the full run"))
    } else {
        let missing = shelf
            .missing
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        Some(format!("Books {present} of {max} · missing {missing}"))
    }
}

/// Set (or clear, with `state: None`) a book's reading state. The
/// user's marks are origin 'user' — progress automation never
/// argues with them beyond the catalog's touch rules.
#[tauri::command]
pub fn books_set_state(
    state: tauri::State<'_, AppState>,
    path: String,
    book_state: Option<String>,
) -> Cmd<()> {
    let catalog = state.catalog.lock().unwrap();
    match book_state.as_deref() {
        None => catalog
            .delete_book_state(&path)
            .map_err(CommandError::from)?,
        Some(s @ ("want" | "reading" | "read")) => catalog
            .set_book_state(&path, s, "user", &chrono::Utc::now().to_rfc3339())
            .map_err(CommandError::from)?,
        Some(other) => {
            return Err(CommandError::from(UfmError::InvalidInput(format!(
                "unknown reading state {other:?} (want, reading, or read)"
            ))));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_math_epub_and_pdf() {
        // Chapter 0 of 4 → a quarter along, not finished.
        let (f, fin) = progress_math(Some(0), None, None, Some(4), None);
        assert_eq!(f, Some(0.25));
        assert!(!fin);
        // Final chapter → fraction 1, finish candidate.
        let (f, fin) = progress_math(Some(3), None, None, Some(4), None);
        assert_eq!(f, Some(1.0));
        assert!(fin);
        // PDF pages behave the same.
        let (f, fin) = progress_math(None, Some(9), None, None, Some(10));
        assert_eq!(f, Some(1.0));
        assert!(fin);
        // No total → no fraction, no fake finish.
        let (f, fin) = progress_math(Some(2), None, None, None, None);
        assert_eq!(f, None);
        assert!(!fin);
        // Zero-total guards.
        let (f, fin) = progress_math(Some(0), None, None, Some(0), None);
        assert_eq!(f, None);
        assert!(!fin);
    }

    #[test]
    fn progress_math_counts_the_part_of_the_chapter_read() {
        // P2: halfway through chapter 0 of 4 → an eighth along,
        // not the quarter P1 credited on open.
        let (f, fin) = progress_math(Some(0), None, Some(0.5), Some(4), None);
        assert_eq!(f, Some(0.125));
        assert!(!fin);
        // Top of chapter 2 of 4 → exactly halfway.
        let (f, fin) = progress_math(Some(2), None, Some(0.0), Some(4), None);
        assert_eq!(f, Some(0.5));
        assert!(!fin);
        // A part-read final chapter still offers the finish (the
        // rule is the chapter, never the fraction).
        let (f, fin) = progress_math(Some(3), None, Some(0.25), Some(4), None);
        assert_eq!(f, Some(0.8125));
        assert!(fin);
        // A wild stored place is clamped, never trusted.
        let (f, _) = progress_math(Some(0), None, Some(7.5), Some(4), None);
        assert_eq!(f, Some(0.25));
    }

    #[test]
    fn cover_ext_and_stem_rules() {
        assert_eq!(cover_ext("image/png"), "png");
        assert_eq!(cover_ext("image/jpeg"), "jpg");
        assert_eq!(cover_ext("image/webp"), "webp");
        assert_eq!(file_stem("Dune.epub"), "Dune");
        assert_eq!(file_stem("no-extension"), "no-extension");
        assert_eq!(file_stem(".hidden"), ".hidden");
    }
}
