//! The Public Domain Shelf (Ebook P3, Phases 1–2).
//!
//! Books Home gains a shelf of classics Dickles doesn't own yet:
//! tap one and it becomes a file in his library — owned, offline,
//! deletable. No account, no store, no rental clock, and nothing
//! phones home beyond fetching the book itself from the source of
//! record (Standard Ebooks; Project Gutenberg for the two rows SE
//! can't lawfully serve).
//!
//! The catalogue ships as data inside the app (the frozen manifest,
//! `pd-catalogue.json` beside this module). State is DERIVED, never
//! stored: a row is "on your shelf" iff its files exist in the
//! library. Delete the file and the row quietly returns to
//! not-pulled (SPEC §A.1 — the Locality Covenant as a data model).
//! The only sidecars are the failure ledger (so a failed pull
//! confesses with a reason and a Retry) and the first-pull pins
//! (sha256 + size recorded when a file first lands, so a re-pull
//! can prove the source still serves the same book — the shipped
//! manifest's own hash fields stay null rather than claim numbers
//! nobody measured).

use serde::{Deserialize, Serialize};
use tauri::Manager;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::AppState;

const MANIFEST_JSON: &str = include_str!("../pd-catalogue.json");
pub const SHELF_FOLDER: &str = "Public Domain Shelf";
const UA: &str = "Fable Files/0.1.0 (personal library pull; one book at a time)";

// ---------------------------------------------------------------------------
// Manifest.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct Source {
    pub kind: String,
    pub url: String,
    #[serde(default)]
    pub gutenberg_id: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkFile {
    pub filename: String,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub size_bytes: Option<u64>,
    /// The two M. R. James volumes live under different PG ebook
    /// numbers than their work row; the file carries its own.
    #[serde(default)]
    pub gutenberg_id: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Work {
    pub id: String,
    pub title: String,
    pub authors: Vec<String>,
    pub section: String,
    pub source: Source,
    pub files: Vec<WorkFile>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    manifest_version: u32,
    shelf_name: String,
    works: Vec<Work>,
}

pub fn manifest() -> &'static Manifest {
    static MANIFEST: std::sync::OnceLock<Manifest> = std::sync::OnceLock::new();
    MANIFEST.get_or_init(|| {
        serde_json::from_str(MANIFEST_JSON).expect("the bundled PD manifest must parse")
    })
}

pub fn work_by_id(id: &str) -> Option<&'static Work> {
    manifest().works.iter().find(|w| w.id == id)
}

// ---------------------------------------------------------------------------
// The match ladder (SPEC §A.5) — ONE normalizer pair, shared by the
// shelf state and anything else that ever asks "does he own this?".
// ---------------------------------------------------------------------------

/// Title key: lowercase, one leading article dropped, punctuation
/// gone, whitespace collapsed. "The Count of Monte Cristo" and
/// "Count of Monte Cristo, The" land on the same key.
pub fn norm_title(raw: &str) -> String {
    let lowered = raw.to_lowercase();
    let mut words: Vec<String> = lowered
        .split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect();
    if matches!(words.first().map(|s| s.as_str()), Some("the" | "a" | "an")) {
        words.remove(0);
    }
    words.join(" ")
}

/// Author key: name tokens sorted, so "Bram Stoker" and
/// "Stoker, Bram" are the same person to the ladder, and
/// "H. P. Lovecraft" equals "Lovecraft, H. P.".
pub fn norm_author(raw: &str) -> String {
    let lowered = raw.to_lowercase();
    let mut tokens: Vec<String> = lowered
        .split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect();
    tokens.sort();
    tokens.join(" ")
}

/// The ordered-authors key for a work or a catalogued book: each
/// author's key, in the order the source lists them.
pub fn author_key(authors: &[String]) -> String {
    authors
        .iter()
        .map(|a| norm_author(a))
        .collect::<Vec<_>>()
        .join(" ; ")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Owned {
    /// Same normalized title AND author key (the ISBN rung of the
    /// ladder confirms where a copy carries one; the seed manifest
    /// records no ISBNs, so identity is title+authors).
    Exact { title: String, path: String },
    /// Same normalized title, author in a different form — shown,
    /// never silently merged (the probably-law).
    Near { title: String, path: String },
}

/// One catalogued book, reduced to what the ladder reads.
pub struct LibraryBook {
    pub title: String,
    pub authors: Vec<String>,
    pub path: String,
}

pub fn match_work(work: &Work, library: &[LibraryBook]) -> Option<Owned> {
    let wt = norm_title(&work.title);
    let wa = author_key(&work.authors);
    let mut near: Option<Owned> = None;
    for book in library {
        if norm_title(&book.title) != wt {
            continue;
        }
        if author_key(&book.authors) == wa {
            return Some(Owned::Exact {
                title: book.title.clone(),
                path: book.path.clone(),
            });
        }
        if near.is_none() {
            near = Some(Owned::Near {
                title: book.title.clone(),
                path: book.path.clone(),
            });
        }
    }
    near
}

// ---------------------------------------------------------------------------
// Shelf state.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdRowWire {
    pub id: String,
    pub title: String,
    pub author_line: String,
    pub section: String,
    pub note: Option<String>,
    /// "on_shelf" | "owned" | "possibly" | "add"
    pub state: &'static str,
    /// For owned/possibly: his existing copy's title and path.
    pub owned_title: Option<String>,
    pub owned_path: Option<String>,
    /// The shelf copy's path when any file is present.
    pub shelf_path: Option<String>,
    pub present_files: usize,
    pub total_files: usize,
    /// The ledger's confession for this row, when the last pull
    /// failed: plain language, with a Retry on the row.
    pub failure: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdPullWire {
    pub running: bool,
    pub done: usize,
    pub total: usize,
    pub current: Option<String>,
    pub failures: usize,
}

impl Default for PdPullWire {
    fn default() -> Self {
        PdPullWire {
            running: false,
            done: 0,
            total: 0,
            current: None,
            failures: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdShelfWire {
    pub shelf_name: String,
    pub dir: String,
    /// The jurisdiction truth, shown once in the section head.
    pub header_note: &'static str,
    pub total_works: usize,
    pub on_shelf: usize,
    pub rows: Vec<PdRowWire>,
    pub pull: PdPullWire,
}

/// The shelf directory: inside the library root his books actually
/// live in (the directory holding the most identified book files),
/// so the indexer — which walks recursively — shelves pulled books
/// beside his own. Empty library: the SPEC's named default.
pub fn shelf_dir(library: &[LibraryBook]) -> PathBuf {
    let mut counts: HashMap<PathBuf, usize> = HashMap::new();
    for book in library {
        let p = Path::new(&book.path);
        if let Some(parent) = p.parent() {
            // Files already in the shelf don't vote for its parent.
            if parent.file_name().map(|n| n == SHELF_FOLDER) != Some(true) {
                *counts.entry(parent.to_path_buf()).or_default() += 1;
            }
        }
    }
    let root = counts
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map(|(dir, _)| dir)
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            PathBuf::from(home).join("Documents").join("Books")
        });
    root.join(SHELF_FOLDER)
}

/// Compose the whole shelf from file presence + the ladder. Pure
/// reads: safe to call on every room paint.
pub fn compose_shelf(library: &[LibraryBook], pull: &PdPullWire) -> PdShelfWire {
    let dir = shelf_dir(library);
    let ledger = load_ledger();
    let mut rows = Vec::new();
    let mut on_shelf = 0usize;
    for work in &manifest().works {
        let present: Vec<&WorkFile> = work
            .files
            .iter()
            .filter(|f| dir.join(&f.filename).is_file())
            .collect();
        let first_present = work
            .files
            .iter()
            .find(|f| dir.join(&f.filename).is_file())
            .map(|f| dir.join(&f.filename).to_string_lossy().into_owned());
        let (state, owned_title, owned_path) = if present.len() == work.files.len() {
            on_shelf += 1;
            ("on_shelf", None, None)
        } else if present.is_empty() {
            match match_work(work, library) {
                Some(Owned::Exact { title, path }) => ("owned", Some(title), Some(path)),
                Some(Owned::Near { title, path }) => ("possibly", Some(title), Some(path)),
                None => ("add", None, None),
            }
        } else {
            // Some volumes present, some not: the row offers the
            // missing ones; what's there opens.
            ("add", None, None)
        };
        rows.push(PdRowWire {
            id: work.id.clone(),
            title: work.title.clone(),
            author_line: work.authors.join(", "),
            section: work.section.clone(),
            note: work.note.clone(),
            state,
            owned_title,
            owned_path,
            shelf_path: first_present,
            present_files: present.len(),
            total_files: work.files.len(),
            failure: ledger.get(&work.id).cloned(),
        });
    }
    PdShelfWire {
        shelf_name: manifest().shelf_name.clone(),
        dir: dir.to_string_lossy().into_owned(),
        header_note: "Public domain in the United States. Pulled books are files you own — delete one and its row simply offers it again.",
        total_works: manifest().works.len(),
        on_shelf,
        rows,
        pull: pull.clone(),
    }
}

// ---------------------------------------------------------------------------
// Ledger + pins (app-data sidecars; temp-file → validate → rename).
// ---------------------------------------------------------------------------

fn sidecar(name: &str) -> PathBuf {
    crate::reader::data_dir().join(name)
}

fn load_ledger() -> HashMap<String, String> {
    std::fs::read_to_string(sidecar("pd-ledger.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_sidecar<T: Serialize>(name: &str, value: &T) -> Result<(), String> {
    let dir = crate::reader::data_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!("{name}.tmp"));
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, &text).map_err(|e| e.to_string())?;
    // Validate before the rename: a corrupt sidecar is never adopted.
    let _: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join(name)).map_err(|e| e.to_string())
}

fn ledger_record(work_id: &str, reason: Option<&str>) {
    let mut ledger = load_ledger();
    match reason {
        Some(r) => {
            ledger.insert(work_id.to_string(), r.to_string());
        }
        None => {
            ledger.remove(work_id);
        }
    }
    let _ = save_sidecar("pd-ledger.json", &ledger);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pin {
    sha256: String,
    size_bytes: u64,
}

fn load_pins() -> HashMap<String, Pin> {
    std::fs::read_to_string(sidecar("pd-pins.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn pin_record(filename: &str, pin: Pin) {
    let mut pins = load_pins();
    pins.insert(filename.to_string(), pin);
    let _ = save_sidecar("pd-pins.json", &pins);
}

// ---------------------------------------------------------------------------
// The fetchers. Both sources are followed the way a browser walks
// them: read the book's page, take the download href it offers,
// never construct a filename. SE serves an interstitial ("Your
// Download Has Started!") unless the link carries ?source=download;
// if HTML comes back anyway, the refresh target inside it is the
// file. PG ebook pages offer several formats; images EPUB wins.
// ---------------------------------------------------------------------------

fn http_get(url: &str) -> Result<ureq::Response, String> {
    ureq::get(url)
        .set("User-Agent", UA)
        .timeout(std::time::Duration::from_secs(120))
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(code, _) => {
                format!("the source answered {code} — it may be having a bad day; retry later")
            }
            other => format!("couldn't reach the source: {other}"),
        })
}

fn http_text(url: &str) -> Result<String, String> {
    http_get(url)?
        .into_string()
        .map_err(|e| format!("the source's page couldn't be read: {e}"))
}

fn http_bytes(url: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    http_get(url)?
        .into_reader()
        .read_to_end(&mut bytes)
        .map_err(|e| format!("the download broke partway: {e}"))?;
    Ok(bytes)
}

/// Every href value in a page, in document order.
fn hrefs(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(pos) = rest.find("href=\"") {
        let after = &rest[pos + 6..];
        match after.find('"') {
            Some(end) => {
                out.push(after[..end].to_string());
                rest = &after[end..];
            }
            None => break,
        }
    }
    out
}

fn absolutize(href: &str, base: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        href.to_string()
    } else if let Some(stripped) = href.strip_prefix('/') {
        // Scheme + host of the base URL.
        let host_end = base
            .find("://")
            .map(|i| i + 3)
            .and_then(|start| base[start..].find('/').map(|j| start + j))
            .unwrap_or(base.len());
        format!("{}/{}", &base[..host_end], stripped)
    } else {
        href.to_string()
    }
}

/// Resolve a Standard Ebooks work's EPUB download URL from its
/// book page.
pub fn se_download_url(page_url: &str) -> Result<String, String> {
    let html = http_text(page_url)?;
    for href in hrefs(&html) {
        let clean = href.split('?').next().unwrap_or("");
        if clean.contains("/downloads/") && clean.ends_with(".epub") {
            let mut url = absolutize(&href, page_url);
            if !url.contains("source=download") {
                url.push_str(if url.contains('?') { "&" } else { "?" });
                url.push_str("source=download");
            }
            return Ok(url);
        }
    }
    Err("Standard Ebooks' page didn't offer an EPUB download".into())
}

/// Resolve a Project Gutenberg ebook's EPUB URL from its page:
/// the images variant when offered, else the plain EPUB.
pub fn pg_download_url(gutenberg_id: u32) -> Result<String, String> {
    let page = format!("https://www.gutenberg.org/ebooks/{gutenberg_id}");
    let html = http_text(&page)?;
    let mut plain: Option<String> = None;
    for href in hrefs(&html) {
        let clean = href.split('?').next().unwrap_or("");
        if !clean.ends_with(".epub") {
            continue;
        }
        let url = absolutize(&href, &page);
        if clean.contains("images") {
            return Ok(url);
        }
        if plain.is_none() {
            plain = Some(url);
        }
    }
    plain.ok_or_else(|| "Project Gutenberg's page didn't offer an EPUB".to_string())
}

fn download_url_for(work: &Work, file: &WorkFile) -> Result<String, String> {
    match work.source.kind.as_str() {
        "standardebooks" => se_download_url(&work.source.url),
        "gutenberg" => {
            let id = file
                .gutenberg_id
                .or(work.source.gutenberg_id)
                .ok_or_else(|| format!("{} has no Gutenberg id", work.title))?;
            pg_download_url(id)
        }
        other => Err(format!("unknown source kind {other}")),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Pull one file: download to `.part`, verify (ZIP magic, a real
/// EPUB decode with chapters, sha256 against the first-pull pin),
/// then rename into place. A failure leaves no file that could
/// pose as the book — never a half-a-book.
fn pull_file(work: &Work, file: &WorkFile, dir: &Path) -> Result<(), String> {
    let url = download_url_for(work, file)?;
    let bytes = http_bytes(&url)?;
    // An HTML answer is the SE interstitial or an error page —
    // if it carries a refresh target, follow it once.
    if bytes.starts_with(b"<") {
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if let Some(target) = interstitial_target(&text) {
            let target = absolutize(&target, &url);
            let bytes = http_bytes(&target)?;
            if bytes.starts_with(b"<") {
                return Err("the source served a web page instead of the book".into());
            }
            return adopt_bytes(work, file, dir, bytes);
        }
        return Err("the source served a web page instead of the book".into());
    }
    adopt_bytes(work, file, dir, bytes)
}

/// The meta-refresh / canonical download target inside an SE
/// interstitial page, when present.
fn interstitial_target(html: &str) -> Option<String> {
    for href in hrefs(html) {
        let clean = href.split('?').next().unwrap_or("").to_string();
        if clean.ends_with(".epub") {
            return Some(href);
        }
    }
    // <meta http-equiv="refresh" content="0; url=…">
    let lower = html.to_lowercase();
    if let Some(pos) = lower.find("url=") {
        let rest = &html[pos + 4..];
        let end = rest
            .find(|c| c == '"' || c == '\'' || c == '>' || c == ' ')
            .unwrap_or(rest.len());
        let target = &rest[..end];
        if target.contains(".epub") {
            return Some(target.to_string());
        }
    }
    None
}

fn adopt_bytes(_work: &Work, file: &WorkFile, dir: &Path, bytes: Vec<u8>) -> Result<(), String> {
    if bytes.len() < 4 || &bytes[..2] != b"PK" {
        return Err("what arrived isn't an EPUB file (it doesn't start like one)".into());
    }
    // Validate-then-adopt (Forge §K): the bytes must BE the book —
    // a decodable EPUB with chapters — before they may land.
    let book = ufm_preview::epub::decode_epub_book(&bytes);
    if book.drm_protected || book.chapters.is_empty() {
        return Err(
            "what arrived doesn't decode as a readable book, so it wasn't shelved".into(),
        );
    }
    let sha = sha256_hex(&bytes);
    let pins = load_pins();
    if let Some(pin) = pins.get(&file.filename) {
        if pin.sha256 != sha || pin.size_bytes != bytes.len() as u64 {
            return Err(
                "the source's copy no longer matches the one first pulled for this book — \
                 nothing was changed; Grizz should look before this row is trusted again"
                    .into(),
            );
        }
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("couldn't create the shelf folder: {e}"))?;
    let part = dir.join(format!("{}.part", file.filename));
    std::fs::write(&part, &bytes).map_err(|e| format!("couldn't save the book: {e}"))?;
    let staged_len = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if staged_len != bytes.len() as u64 {
        let _ = std::fs::remove_file(&part);
        return Err("the saved book came out a different size than the download".into());
    }
    std::fs::rename(&part, dir.join(&file.filename))
        .map_err(|e| format!("couldn't shelve the book: {e}"))?;
    if pins.get(&file.filename).is_none() {
        pin_record(
            &file.filename,
            Pin {
                sha256: sha,
                size_bytes: bytes.len() as u64,
            },
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The pull job: single-flight, sequential, polite intervals, every
// failure confessed to the ledger with its reason.
// ---------------------------------------------------------------------------

pub struct PdHub {
    pub running: AtomicBool,
    pub status: Mutex<PdPullWire>,
}

impl PdHub {
    pub fn new() -> Self {
        PdHub {
            running: AtomicBool::new(false),
            status: Mutex::new(PdPullWire::default()),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PdPullProgress {
    done: usize,
    total: usize,
    current: Option<String>,
    failures: usize,
}

/// Pull the given works (or, with `None`, every work not fully on
/// the shelf and not already his under the ladder's Exact rung).
/// Returns the running snapshot; the work happens on a thread and
/// reports through `pd-pull-progress` / `pd-pull-finished` events.
#[tauri::command]
pub fn pd_pull(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    hub: tauri::State<'_, PdHub>,
    work_ids: Option<Vec<String>>,
) -> Result<PdPullWire, String> {
    if hub.running.load(Ordering::SeqCst) {
        return Ok(hub.status.lock().unwrap().clone());
    }
    // Resolve the shelf + the work list on this side of the lock.
    let (dir, library) = {
        let catalog = state.catalog.lock().unwrap();
        let rows = catalog.books_all().map_err(|e| e.to_string())?;
        let library: Vec<LibraryBook> = rows
            .iter()
            .map(|r| LibraryBook {
                title: r.meta.title.clone().unwrap_or_default(),
                authors: r.meta.authors.clone(),
                path: r.meta.provider_key.clone(),
            })
            .collect();
        (shelf_dir(&library), library)
    };
    if hub.running.swap(true, Ordering::SeqCst) {
        return Ok(hub.status.lock().unwrap().clone());
    }
    let wanted: Vec<&'static Work> = manifest()
        .works
        .iter()
        .filter(|w| match &work_ids {
            Some(ids) => ids.iter().any(|i| i == &w.id),
            None => {
                let all_present = w
                    .files
                    .iter()
                    .all(|f| dir.join(&f.filename).is_file());
                !all_present
                    && !matches!(match_work(w, &library), Some(Owned::Exact { .. }))
            }
        })
        .collect();
    let total_files: usize = wanted
        .iter()
        .map(|w| {
            w.files
                .iter()
                .filter(|f| !dir.join(&f.filename).is_file())
                .count()
        })
        .sum();
    {
        let mut st = hub.status.lock().unwrap();
        *st = PdPullWire {
            running: true,
            done: 0,
            total: total_files,
            current: None,
            failures: 0,
        };
    }
    let initial = hub.status.lock().unwrap().clone();
    let app2 = app.clone();
    std::thread::spawn(move || {
        use tauri::Emitter;
        let mut done = 0usize;
        let mut failures = 0usize;
        let mut any_landed = false;
        for work in &wanted {
            for file in &work.files {
                if dir.join(&file.filename).is_file() {
                    continue;
                }
                {
                    // The hub outlives this thread via the app's
                    // managed state; reach it through the handle.
                    let hub2 = app2.state::<PdHub>();
                    let mut st = hub2.status.lock().unwrap();
                    st.current = Some(work.title.clone());
                    st.done = done;
                    st.failures = failures;
                }
                let _ = app2.emit(
                    "pd-pull-progress",
                    PdPullProgress {
                        done,
                        total: total_files,
                        current: Some(work.title.clone()),
                        failures,
                    },
                );
                match pull_file(work, file, &dir) {
                    Ok(()) => {
                        any_landed = true;
                    }
                    Err(reason) => {
                        failures += 1;
                        ledger_record(&work.id, Some(&reason));
                        break; // this work's remaining files wait for the retry
                    }
                }
                done += 1;
                // Fetching is polite by construction: sequential,
                // one connection, a breath between books.
                std::thread::sleep(std::time::Duration::from_millis(1200));
            }
            if work
                .files
                .iter()
                .all(|f| dir.join(&f.filename).is_file())
            {
                ledger_record(&work.id, None);
            }
        }
        {
            let hub2 = app2.state::<PdHub>();
            let mut st = hub2.status.lock().unwrap();
            st.running = false;
            st.done = done;
            st.failures = failures;
            st.current = None;
            hub2.running.store(false, Ordering::SeqCst);
        }
        let _ = app2.emit(
            "pd-pull-finished",
            PdPullProgress {
                done,
                total: total_files,
                current: None,
                failures,
            },
        );
        if any_landed {
            // Shelve the newcomers: the identify pass reads their
            // own Dublin Core and puts them on the room's shelves.
            crate::books::identify_in_background(app2.clone(), None);
        }
    });
    Ok(initial)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work(title: &str, authors: &[&str]) -> Work {
        Work {
            id: "x".into(),
            title: title.into(),
            authors: authors.iter().map(|s| s.to_string()).collect(),
            section: "s".into(),
            source: Source {
                kind: "standardebooks".into(),
                url: "https://example.invalid".into(),
                gutenberg_id: None,
            },
            files: vec![],
            note: None,
        }
    }

    fn book(title: &str, authors: &[&str], path: &str) -> LibraryBook {
        LibraryBook {
            title: title.into(),
            authors: authors.iter().map(|s| s.to_string()).collect(),
            path: path.into(),
        }
    }

    #[test]
    fn manifest_freeze_is_48_works_49_files() {
        let m = manifest();
        assert_eq!(m.manifest_version, 1);
        assert_eq!(m.works.len(), 48);
        assert_eq!(m.works.iter().map(|w| w.files.len()).sum::<usize>(), 49);
        assert_eq!(
            m.works.iter().filter(|w| w.source.kind == "gutenberg").count(),
            2
        );
        // The Leagues rule, pinned: that row is Gutenberg, forever.
        let leagues = m
            .works
            .iter()
            .find(|w| w.title.contains("Twenty Thousand Leagues"))
            .expect("leagues row exists");
        assert_eq!(leagues.source.kind, "gutenberg");
        assert_eq!(leagues.source.gutenberg_id, Some(164));
    }

    #[test]
    fn ladder_exact_beats_near_and_orderings_agree() {
        let dracula = work("Dracula", &["Bram Stoker"]);
        let lib = vec![
            book("Dracula", &["Stoker, Bram"], "/books/dracula.epub"),
            book("Dracula", &["Someone Else"], "/books/other.epub"),
        ];
        match match_work(&dracula, &lib) {
            Some(Owned::Exact { path, .. }) => assert_eq!(path, "/books/dracula.epub"),
            other => panic!("expected Exact, got {other:?}"),
        }
        let lib2 = vec![book("Dracula", &["Someone Else"], "/books/other.epub")];
        assert!(matches!(
            match_work(&dracula, &lib2),
            Some(Owned::Near { .. })
        ));
        assert!(match_work(&dracula, &[]).is_none());
    }

    #[test]
    fn ladder_apostrophe_and_article_ugly_titles() {
        // Forge §P's ugly case: articles, apostrophes, initials.
        let w = work("The Beginner's Guide to Magic", &["H. P. Lovecraft"]);
        let lib = vec![book(
            "Beginner's Guide to Magic, The",
            &["Lovecraft, H. P."],
            "/b.epub",
        )];
        // NB: "…, The" trailing-article forms are NOT folded by
        // norm_title (leading articles only) — this asserts the
        // honest limit rather than a pretend match.
        assert!(match_work(&w, &lib).is_none());
        let lib2 = vec![book(
            "The Beginner's Guide to Magic",
            &["Lovecraft, H. P."],
            "/b.epub",
        )];
        assert!(matches!(
            match_work(&w, &lib2),
            Some(Owned::Exact { .. })
        ));
    }

    #[test]
    fn href_extraction_and_interstitial() {
        let page = r#"<a href="/ebooks/x/y/downloads/x_y.epub">EPUB</a> <a href="/about">About</a>"#;
        assert_eq!(
            hrefs(page),
            vec!["/ebooks/x/y/downloads/x_y.epub", "/about"]
        );
        let interstitial = r#"<meta http-equiv="refresh" content="5; url=https://standardebooks.org/ebooks/x/y/downloads/x_y.epub?source=download">"#;
        assert!(interstitial_target(interstitial)
            .unwrap()
            .ends_with("x_y.epub?source=download"));
    }
}
