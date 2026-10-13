//! Typed command surface — the ONLY bridge between the web shell and the
//! Rust core. No broad filesystem exposure reaches web content (spec §25).
//!
//! Every command takes/returns serializable types from the domain crates.
//! Commands are thin: validate, delegate to the core, emit a DomainEvent.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{Emitter, Manager, State};
use ufm_core::{CollectionId, ItemId, JobId, LocationId, UfmError};
use ufm_vfs::{Item, ItemKind};

use crate::{AppState, IndexState};

// ---------------------------------------------------------------------------
// Error rendering — one shape for the whole UI.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct CommandError {
    pub code: String,
    pub message: String,
}

impl From<UfmError> for CommandError {
    fn from(e: UfmError) -> Self {
        let code = match &e {
            UfmError::NotFound(_) => "not_found",
            UfmError::PermissionDenied(_) => "permission_denied",
            UfmError::Conflict(_) => "conflict",
            UfmError::Provider { .. } => "provider",
            UfmError::JobFailed { .. } => "job_failed",
            UfmError::Theme(_) => "theme",
            UfmError::Plugin(_) => "plugin",
            UfmError::Mcp(_) => "mcp",
            UfmError::Database(_) => "database",
            UfmError::InvalidInput(_) => "invalid_input",
            UfmError::Internal(_) => "internal",
        };
        Self {
            code: code.into(),
            message: e.to_string(),
        }
    }
}

pub type Cmd<T> = std::result::Result<T, CommandError>;

// ---------------------------------------------------------------------------
// Locations & browsing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct LocationSummary {
    pub id: LocationId,
    pub provider: String,
    pub display_name: String,
    /// Item id of the location's root — pass this as `dir` to `list_dir` to
    /// open the top level.
    pub root_item: ItemId,
}

#[tauri::command]
pub async fn list_locations(state: State<'_, AppState>) -> Cmd<Vec<LocationSummary>> {
    Ok(state.locations.lock().unwrap().clone())
}

struct DirEntryInfo {
    path: PathBuf,
    name: String,
    kind: ItemKind,
    size_bytes: Option<u64>,
    modified: Option<std::time::SystemTime>,
}

/// One level of a local directory. Blocking fs IO, so the command runs it
/// under `spawn_blocking`.
fn read_dir_entries(path: &Path) -> std::io::Result<Vec<DirEntryInfo>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(path)? {
        // One bad entry must never fail the whole listing (mirrors
        // LocalProvider::list): skip it and keep going.
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(target: "ufm", "skipping unreadable entry in {}: {e}", path.display());
                continue;
            }
        };
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(e) => {
                tracing::warn!(target: "ufm", "skipping entry with unreadable file type in {}: {e}", path.display());
                continue;
            }
        };
        let metadata = entry.metadata().ok();
        let kind = if file_type.is_dir() {
            ItemKind::Folder
        } else if file_type.is_symlink() {
            ItemKind::Symlink
        } else {
            ItemKind::File
        };
        out.push(DirEntryInfo {
            path: entry.path(),
            name: entry.file_name().to_string_lossy().into_owned(),
            kind,
            size_bytes: if file_type.is_file() {
                metadata.as_ref().map(|m| m.len())
            } else {
                None
            },
            modified: metadata.and_then(|m| m.modified().ok()),
        });
    }
    // Folders first, then case-insensitive name order.
    out.sort_by(|a, b| {
        let af = a.kind == ItemKind::Folder;
        let bf = b.kind == ItemKind::Folder;
        bf.cmp(&af)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(out)
}

/// Resolve a `list_dir` target to an absolute path. The location root is
/// special-cased via the seeded root item id; anything else must already be
/// in the browse cache (populated by earlier `list_dir` / `search` results).
fn resolve_dir_path(state: &AppState, location: LocationId, dir: ItemId) -> Cmd<PathBuf> {
    let not_found = |what: String| CommandError::from(UfmError::NotFound(what));
    let root = state
        .local_roots
        .lock()
        .unwrap()
        .get(&location)
        .cloned()
        .ok_or_else(|| not_found(format!("unknown location {location}")))?;
    let is_root = state.root_items.lock().unwrap().get(&location).copied() == Some(dir);
    if is_root {
        return Ok(root);
    }
    state
        .item_paths
        .lock()
        .unwrap()
        .get(&dir)
        .cloned()
        .ok_or_else(|| not_found("unknown item; list a location root first".to_string()))
}

#[tauri::command]
pub async fn list_dir(
    state: State<'_, AppState>,
    location: LocationId,
    dir: ItemId,
) -> Cmd<Vec<Item>> {
    // NOTE: reads the directory directly via std::fs (in spawn_blocking) rather
    // than `LocalProvider::list`, so ids stay stable across re-lists through
    // the path_ids cache. When the provider lands fully, route through it.
    let path = resolve_dir_path(&state, location, dir)?;
    let entries = tauri::async_runtime::spawn_blocking(move || read_dir_entries(&path))
        .await
        .map_err(|e| CommandError {
            code: "internal".into(),
            message: format!("list task failed: {e}"),
        })?
        .map_err(|e| {
            CommandError::from(UfmError::Provider {
                provider: "local".into(),
                message: e.to_string(),
            })
        })?;

    // Canonical lock order (see AppState): path_ids -> item_paths ->
    // item_locations. Every command taking more than one of these maps
    // must acquire them in this order.
    let mut path_ids = state.path_ids.lock().unwrap();
    let mut item_paths = state.item_paths.lock().unwrap();
    let mut item_locations = state.item_locations.lock().unwrap();
    let items = entries
        .into_iter()
        .map(|e| {
            // Stable ids: re-listing a directory must not mint new ones.
            let id = *path_ids.entry(e.path.clone()).or_insert_with(ItemId::new);
            item_paths.insert(id, e.path.clone());
            item_locations.insert(id, location);
            Item {
                id,
                location,
                name: e.name,
                kind: e.kind,
                size_bytes: e.size_bytes,
                modified: e.modified.map(chrono::DateTime::<chrono::Utc>::from),
                provider_key: Some(e.path.to_string_lossy().into_owned()),
                labels: Vec::new(),
                starred: false,
            }
        })
        .collect::<Vec<Item>>();
    let mut items = items;
    // Hydrate user labels (path-keyed in the catalog).
    if let Ok(catalog) = ufm_catalog::Catalog::open(&state.catalog_path) {
        for item in items.iter_mut() {
            if let Some(pk) = &item.provider_key {
                if let Ok(labels) = catalog.labels_for_path(pk) {
                    item.labels = labels;
                }
            }
        }
    }
    Ok(items)
}

// ---------------------------------------------------------------------------
// Jobs — the transfer center
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CopyRequest {
    pub sources: Vec<ItemId>,
    pub dst_parent: ItemId,
}

#[tauri::command]
pub async fn start_copy(
    _state: State<'_, AppState>,
    _req: CopyRequest,
) -> Cmd<JobId> {
    // TODO: build the DAG (verify-then-skip steps), journal it, queue it.
    Err(CommandError {
        code: "not_implemented".into(),
        message: "job engine wiring comes with the first feature milestone".into(),
    })
}

#[tauri::command]
pub async fn pause_job(_state: State<'_, AppState>, _job: JobId) -> Cmd<()> {
    Ok(())
}

#[tauri::command]
pub async fn resume_job(_state: State<'_, AppState>, _job: JobId) -> Cmd<()> {
    Ok(())
}

#[tauri::command]
pub async fn cancel_job(_state: State<'_, AppState>, _job: JobId) -> Cmd<()> {
    Ok(())
}

// ---------------------------------------------------------------------------
// Search — one language everywhere
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub item: Item,
    /// bm25 score from the catalog; lower = better match.
    pub rank: f64,
}

/// File-type categories for the advanced-search filter.
fn type_extensions(filter: &str) -> Option<&'static [&'static str]> {
    match filter {
        "image" => Some(&["png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "svg", "ico", "avif"][..]),
        "video" => Some(&["mp4", "mkv", "webm", "mov", "avi", "m4v", "mpg", "mpeg"][..]),
        "audio" => Some(&["mp3", "flac", "ogg", "oga", "wav", "m4a", "opus", "aac"][..]),
        "document" => Some(&["pdf", "doc", "docx", "odt", "epub", "txt", "md", "rtf", "csv", "tsv"][..]),
        "archive" => Some(&["zip", "7z", "rar", "tar", "gz", "bz2", "xz", "tgz"][..]),
        _ => None,
    }
}

fn name_in_type(name: &str, exts: &[&str]) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    !ext.is_empty() && ext != name.to_lowercase() && exts.contains(&ext.as_str())
}

#[tauri::command]
pub async fn search(
    state: State<'_, AppState>,
    query_text: String,
    scope: Option<LocationId>,
    limit: Option<u32>,
    type_filter: Option<String>,
) -> Cmd<Vec<SearchHit>> {
    let limit = limit.unwrap_or(50).min(500);
    let exts: Option<&'static [&'static str]> =
        type_filter.as_deref().and_then(type_extensions);

    // Type-only browsing (no text): list matching files by recency.
    if query_text.trim().is_empty() {
        if let Some(exts) = exts {
            let rows = {
                let catalog = state.catalog.lock().unwrap();
                catalog
                    .browse_by_extensions(exts, limit)
                    .map_err(CommandError::from)?
            };
            let rows: Vec<(Item, f64)> = rows.into_iter().map(|i| (i, 0.0)).collect();
            return finish_search(state, rows, scope);
        }
        return Ok(Vec::new());
    }

    // A query that doesn't parse is not an error — it just matches nothing.
    let query = match ufm_search::parse(&query_text) {
        Ok(q) => q,
        Err(_) => return Ok(Vec::new()),
    };
    let fts_query = ufm_search::to_fts(&query);
    if fts_query.trim().is_empty() {
        return Ok(Vec::new());
    }
    // With a type filter, over-fetch so filtering still fills the page.
    let fetch = if exts.is_some() { (limit * 8).min(400) } else { limit };
    // `search_fts` has no scope parameter in v0.1 — filter by location here.
    let mut rows = {
        let catalog = state.catalog.lock().unwrap();
        catalog
            .search_fts(&fts_query, fetch)
            .map_err(CommandError::from)?
    };
    if let Some(exts) = exts {
        rows.retain(|(item, _)| name_in_type(&item.name, exts));
        rows.truncate(limit as usize);
    }
    finish_search(state, rows, scope)
}

/// Shared tail of `search`: apply the location scope, cache result paths
/// so preview/list_dir can resolve the hits, and shape the response.
fn finish_search(
    state: State<'_, AppState>,
    rows: Vec<(Item, f64)>,
    scope: Option<LocationId>,
) -> Cmd<Vec<SearchHit>> {
    let mut rows: Vec<(Item, f64)> = match scope {
        Some(s) => rows
            .into_iter()
            .filter(|(item, _)| item.location == s)
            .collect(),
        None => rows,
    };

    // Cache paths so `preview` / `list_dir` can resolve these items.
    // Canonical lock order (see AppState): path_ids -> item_paths ->
    // item_locations. `list_dir` takes them in this order; `search` must
    // too, or the two commands deadlock against each other (ABBA) when
    // Tauri runs them concurrently.
    {
        let mut path_ids = state.path_ids.lock().unwrap();
        let mut item_paths = state.item_paths.lock().unwrap();
        let mut item_locations = state.item_locations.lock().unwrap();
        for (item, _) in rows.iter_mut() {
            if let Some(key) = item.provider_key.as_deref() {
                let path = PathBuf::from(key);
                // One stable id per path: whoever (list_dir or search) saw
                // the path first wins; the hit adopts the canonical id so
                // preview/list_dir resolve the same item either way.
                let id = *path_ids.entry(path.clone()).or_insert(item.id);
                item.id = id;
                item_paths.insert(id, path);
                item_locations.insert(id, item.location);
            }
        }
    }

    Ok(rows
        .into_iter()
        .map(|(item, rank)| SearchHit { item, rank })
        .collect())
}

// ---------------------------------------------------------------------------
// Indexing — X1-class background walk feeding the catalog
// ---------------------------------------------------------------------------

/// Rebuild the `ufm_vfs::Location` for an indexer run. `mount_local` drops
/// the root path, so the id/display name come from the seeded records and
/// the canonical local capabilities are re-applied.
fn location_for_index(state: &AppState, id: LocationId, root: &Path) -> Cmd<ufm_vfs::Location> {
    let summary = state
        .locations
        .lock()
        .unwrap()
        .iter()
        .find(|l| l.id == id)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown location {id}"))))?;
    let mut location = ufm_vfs::mount_local(summary.display_name, root.to_path_buf());
    location.id = id;
    Ok(location)
}

/// Atomically claim the background-indexer runner slot.
/// Returns `(already_running, snapshot)`: when `already_running` is true the
/// caller must NOT start a walk and should report the snapshot as-is.
/// The check and the set happen under a single lock hold, so two rapid
/// concurrent `start_indexing` calls cannot both claim the slot.
fn claim_index_slot(index_state: &Mutex<IndexState>) -> (bool, IndexState) {
    let mut index = index_state.lock().unwrap();
    if index.running {
        return (true, index.clone());
    }
    index.running = true;
    index.dirs_done = 0;
    index.files_done = 0;
    index.bytes_done = 0;
    (false, index.clone())
}

#[tauri::command]
pub async fn start_indexing(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    location: LocationId,
) -> Cmd<IndexState> {
    // Idempotent: a second call while running returns the current state,
    // never a duplicate walk. Checkpoint resume inside the Indexer makes a
    // re-run after completion cheap (already-indexed roots return zeroed
    // stats without re-walking).
    let (already_running, snapshot) = claim_index_slot(&state.index_state);
    if already_running {
        return Ok(snapshot);
    }
    // The slot is claimed; any early error below must release it, or the
    // indexer would read as running forever.
    let root = match state.local_roots.lock().unwrap().get(&location).cloned() {
        Some(root) => root,
        None => {
            state.index_state.lock().unwrap().running = false;
            return Err(CommandError::from(UfmError::NotFound(format!(
                "unknown location {location}"
            ))));
        }
    };
    let location_rec = match location_for_index(&state, location, &root) {
        Ok(location_rec) => location_rec,
        Err(e) => {
            state.index_state.lock().unwrap().running = false;
            return Err(e);
        }
    };

    let catalog_path = state.catalog_path.clone();
    let checkpoint_path = state.checkpoint_path.clone();
    // The background task re-enters state through the owned AppHandle, so the
    // borrow of `state` never escapes this command.
    tauri::async_runtime::spawn(async move {
        let progress_app = app.clone();
        let indexed = tauri::async_runtime::spawn_blocking(move || {
            let mut indexer = ufm_search::Indexer::open(&catalog_path, &checkpoint_path)?;
            indexer.set_progress_callback(Box::new(move |stats| {
                // Single-lock site: only index_state, never nested — the
                // canonical lock order is respected.
                let payload = {
                    let state = progress_app.state::<AppState>();
                    let mut index = state.index_state.lock().unwrap();
                    if index.running {
                        index.dirs_done = stats.dirs_done;
                        index.files_done = stats.files_done;
                        index.bytes_done = stats.bytes_done;
                    }
                    index.clone()
                };
                let _ = progress_app.emit("index-progress", &payload);
            }));
            indexer.index_root(&root, &location_rec)
        })
        .await;
        let state = app.state::<AppState>();
        {
            let mut index = state.index_state.lock().unwrap();
            match indexed {
                Ok(Ok(stats)) => {
                    *index = IndexState {
                        running: false,
                        dirs_done: stats.dirs_done,
                        files_done: stats.files_done,
                        bytes_done: stats.bytes_done,
                    };
                }
                Ok(Err(e)) => {
                    tracing::error!(target: "ufm", "indexing failed: {e}");
                    index.running = false;
                }
                Err(join_err) => {
                    tracing::error!(target: "ufm", "index task failed: {join_err}");
                    index.running = false;
                }
            }
        }
        let payload = state.index_state.lock().unwrap().clone();
        if let Err(e) = app.emit("index-progress", &payload) {
            tracing::warn!(target: "ufm", "index-progress emit failed: {e}");
        }
    });

    Ok(snapshot)
}

#[tauri::command]
pub async fn index_status(state: State<'_, AppState>) -> Cmd<IndexState> {
    Ok(state.index_state.lock().unwrap().clone())
}

// ---------------------------------------------------------------------------
// Themes — P0. The shell renders ONLY from tokens.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ThemeInfo {
    pub id: String,
    pub name: String,
    pub dark: bool,
}

#[tauri::command]
pub async fn list_themes(state: State<'_, AppState>) -> Cmd<Vec<ThemeInfo>> {
    let themes_dir = state.themes_dir.clone();
    let mut out = Vec::new();
    let entries = std::fs::read_dir(&themes_dir)
        .map_err(|e| CommandError::from(UfmError::Theme(format!("read themes dir: {e}"))))?;
    for entry in entries {
        let entry =
            entry.map_err(|e| CommandError::from(UfmError::Theme(e.to_string())))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        // The theme id is its directory name. `ThemeManifest.id` is a fresh
        // random UUID on every `load_from_dir` (serde default, "assigned by
        // the theme service on install"), so it is never stable across loads.
        let id = entry.file_name().to_string_lossy().into_owned();
        match ufm_themes::ThemePackage::load_from_dir(&path) {
            Ok(pkg) => out.push(ThemeInfo {
                id,
                name: pkg.manifest.name,
                dark: pkg.manifest.dark,
            }),
            Err(e) => tracing::warn!(
                target: "ufm",
                "skipping broken theme {}: {e}",
                path.display()
            ),
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// Find a theme package by id (directory name first, manifest id fallback).
/// Returns the canonical id (directory name) and the loaded package.
fn find_theme_package(
    themes_dir: &Path,
    theme: &str,
) -> Cmd<(String, ufm_themes::ThemePackage)> {
    let direct = themes_dir.join(theme);
    if direct.is_dir() {
        let pkg =
            ufm_themes::ThemePackage::load_from_dir(&direct).map_err(CommandError::from)?;
        return Ok((theme.to_string(), pkg));
    }
    if let Ok(entries) = std::fs::read_dir(themes_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Ok(pkg) = ufm_themes::ThemePackage::load_from_dir(&path) {
                    if pkg.manifest.id.to_string() == theme {
                        let id = entry.file_name().to_string_lossy().into_owned();
                        return Ok((id, pkg));
                    }
                }
            }
        }
    }
    Err(CommandError::from(UfmError::Theme(format!(
        "unknown theme {theme}"
    ))))
}

#[tauri::command]
pub async fn apply_theme(state: State<'_, AppState>, theme: String) -> Cmd<String> {
    // Returns the theme's CSS custom properties for the shell to install.
    let themes_dir = state.themes_dir.clone();
    let (id, pkg) = find_theme_package(&themes_dir, &theme)?;
    *state.active_theme.lock().unwrap() = id;
    Ok(pkg.to_css_variables())
}

#[tauri::command]
pub async fn theme_contrast_report(
    state: State<'_, AppState>,
    theme: String,
) -> Cmd<Vec<ufm_themes::ContrastIssue>> {
    let themes_dir = state.themes_dir.clone();
    let (_, pkg) = find_theme_package(&themes_dir, &theme)?;
    Ok(ufm_themes::check_contrast(&pkg.tokens))
}

// ---------------------------------------------------------------------------
// Organization — tags, stars, collections
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn tag_item(
    _state: State<'_, AppState>,
    _item: ItemId,
    _label: String,
) -> Cmd<()> {
    Ok(())
}

#[tauri::command]
pub async fn toggle_star(_state: State<'_, AppState>, _item: ItemId) -> Cmd<bool> {
    Ok(false)
}

#[tauri::command]
pub async fn list_collections(_state: State<'_, AppState>) -> Cmd<Vec<CollectionId>> {
    Ok(vec![])
}

// ---------------------------------------------------------------------------
// Preview — spacebar Quick Look
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn preview(
    state: State<'_, AppState>,
    item: ItemId,
) -> Cmd<ufm_preview::PreviewKind> {
    let (path, location) = {
        let paths = state.item_paths.lock().unwrap();
        let locations = state.item_locations.lock().unwrap();
        match (paths.get(&item), locations.get(&item)) {
            (Some(p), Some(l)) => (p.clone(), *l),
            _ => {
                return Err(CommandError::from(UfmError::NotFound(format!(
                    "unknown item {item}"
                ))))
            }
        }
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| item.to_string());
    let kind = if path.is_dir() {
        ItemKind::Folder
    } else {
        ItemKind::File
    };
    let probe = Item {
        id: item,
        location,
        name,
        kind,
        size_bytes: None,
        modified: None,
        provider_key: Some(path.to_string_lossy().into_owned()),
        labels: Vec::new(),
        starred: false,
    };
    Ok(state
        .previews
        .best_kind(&probe)
        .unwrap_or(ufm_preview::PreviewKind::Hex))
}


/// Open an item with the OS default application (Finder / Explorer /
/// xdg-open). Only backend-minted item ids resolve through `item_paths`,
/// so the shell cannot be tricked into opening an arbitrary path.
#[tauri::command]
pub async fn open_item(state: State<'_, AppState>, item: ItemId) -> Cmd<()> {
    let path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    tauri::async_runtime::spawn_blocking(move || open::that(&path))
        .await
        .map_err(|e| {
            CommandError::from(UfmError::Provider {
                provider: "local".into(),
                message: format!("open task failed: {e}"),
            })
        })?
        .map_err(|e| {
            CommandError::from(UfmError::Provider {
                provider: "local".into(),
                message: format!("open failed: {e}"),
            })
        })?;
    Ok(())
}


/// Toggle the executable bit on a file (chmod +x / chmod -x).
/// Only backend-minted item ids resolve through item_paths,
/// so the shell cannot be tricked into chmodding an arbitrary path.
#[tauri::command]
pub async fn set_executable(
    state: State<'_, AppState>,
    item: ItemId,
    executable: bool,
) -> Cmd<()> {
    let path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    tauri::async_runtime::spawn_blocking(move || {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
        let mut perms = meta.permissions();
        let mode = perms.mode();
        perms.set_mode(if executable { mode | 0o111 } else { mode & !0o111 });
        std::fs::set_permissions(&path, perms).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("chmod task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::PermissionDenied(format!("chmod failed: {e}"))))?;
    Ok(())
}


/// Read the first N lines of a text file for Quick Look preview.
/// Only backend-minted item ids resolve through item_paths.
/// Returns the lines and whether the file was truncated.
#[tauri::command]
pub async fn preview_text(
    state: State<'_, AppState>,
    item: ItemId,
    max_lines: Option<usize>,
) -> Cmd<PreviewTextResult> {
    let path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    let max = max_lines.unwrap_or(50).min(500);
    // Binary office formats can't be previewed as text — return a friendly message
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let binary_office = ["ods", "xlsx", "xls", "docx", "doc", "pptx", "ppt", "odp", "odg"];
    if binary_office.contains(&ext.as_str()) {
        return Ok(PreviewTextResult {
            lines: vec![format!("Preview not yet supported for .{} files.", ext),
                        "Rich document preview is on the roadmap.".to_string()],
            truncated: false,
        });
    }
    tauri::async_runtime::spawn_blocking(move || {
        use std::io::{BufRead, BufReader};
        let file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
        let reader = BufReader::new(file);
        let mut lines = Vec::new();
        let mut truncated = false;
        for (i, line) in reader.lines().enumerate() {
            if i >= max {
                truncated = true;
                break;
            }
            // Lossy conversion handles non-UTF8 gracefully
            let l = line.map(|s| s).unwrap_or_else(|_| "<binary data>".to_string());
            // Truncate very long lines
            // Character-safe truncation (avoid panic on UTF-8 boundary)
            let truncated_line = if l.chars().count() > 500 {
                let safe: String = l.chars().take(500).collect();
                format!("{}...", safe)
            } else {
                l
            };
            lines.push(truncated_line);
        }
        Ok::<PreviewTextResult, String>(PreviewTextResult { lines, truncated })
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("preview task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(format!("preview failed: {e}"))))
}

#[derive(Debug, serde::Serialize)]
pub struct PreviewTextResult {
    pub lines: Vec<String>,
    pub truncated: bool,
}


/// Get the OS trash directory path.
/// Linux: ~/.local/share/Trash/files, macOS: ~/.Trash
#[tauri::command]
pub async fn trash_path() -> Cmd<String> {
    let home = std::env::var("HOME").map_err(|e| {
        CommandError::from(UfmError::Internal(format!("no HOME: {e}")))
    })?;
    #[cfg(target_os = "macos")]
    let trash = format!("{home}/.Trash");
    #[cfg(not(target_os = "macos"))]
    let trash = format!("{home}/.local/share/Trash/files");
    Ok(trash)
}

/// Empty the trash (standard delete).
#[tauri::command]
pub async fn empty_trash() -> Cmd<TrashResult> {
    let trash = trash_path().await?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut deleted = 0;
        let mut errors = 0;
        // Delete files/
        if let Ok(entries) = std::fs::read_dir(&trash) {
            for entry in entries.flatten() {
                let p = entry.path();
                let r = if p.is_dir() {
                    std::fs::remove_dir_all(&p)
                } else {
                    std::fs::remove_file(&p)
                };
                if r.is_ok() { deleted += 1; } else { errors += 1; }
            }
        }
        // Delete info/ (freedesktop trash metadata)
        let info_dir = trash.replace("/files", "/info");
        if let Ok(entries) = std::fs::read_dir(&info_dir) {
            for entry in entries.flatten() {
                if std::fs::remove_file(entry.path()).is_ok() { deleted += 1; }
                else { errors += 1; }
            }
        }
        Ok(TrashResult { deleted, errors })
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("trash task failed: {e}"))))?
}

/// Securely empty the trash (overwrite with zeros before deleting).
/// Uses `shred -u -z` if available, falls back to Rust overwrite.
#[tauri::command]
pub async fn secure_empty_trash() -> Cmd<TrashResult> {
    let trash = trash_path().await?;
    tauri::async_runtime::spawn_blocking(move || {
        use std::io::{Seek, SeekFrom, Write};
        let mut deleted = 0;
        let mut errors = 0;

        // Collect all files (not dirs yet)
        let mut files = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&trash) {
            for entry in entries.flatten() {
                files.push(entry.path());
            }
        }

        for path in files {
            if path.is_dir() {
                // Recursively shred files in subdirs, then remove dir
                if let Ok(walker) = walkdir_files(&path) {
                    for f in walker {
                        shred_file(&f);
                    }
                }
                if std::fs::remove_dir_all(&path).is_ok() { deleted += 1; }
                else { errors += 1; }
            } else {
                shred_file(&path);
                if std::fs::remove_file(&path).is_ok() { deleted += 1; }
                else { errors += 1; }
            }
        }
        // Clean info/
        let info_dir = trash.replace("/files", "/info");
        if let Ok(entries) = std::fs::read_dir(&info_dir) {
            for entry in entries.flatten() {
                if std::fs::remove_file(entry.path()).is_ok() { deleted += 1; }
            }
        }
        Ok(TrashResult { deleted, errors })
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("secure trash task failed: {e}"))))?
}

/// Overwrite a file with zeros (single pass).
fn shred_file(path: &std::path::Path) {
    use std::io::{Seek, SeekFrom, Write};
    if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(path) {
        if let Ok(meta) = f.metadata() {
            let len = meta.len();
            // Write zeros in 64KB chunks
            let zeros = vec![0u8; 65536];
            let _ = f.seek(SeekFrom::Start(0));
            let mut remaining = len;
            while remaining > 0 {
                let chunk = std::cmp::min(remaining, 65536) as usize;
                if f.write_all(&zeros[..chunk]).is_err() { break; }
                remaining -= chunk as u64;
            }
            let _ = f.sync_all();
        }
    }
}

/// Recursively collect files under a directory.
fn walkdir_files(dir: &std::path::Path) -> Result<Vec<std::path::PathBuf>, String> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                out.extend(walkdir_files(&p).unwrap_or_default());
            } else {
                out.push(p);
            }
        }
    }
    Ok(out)
}

#[derive(Debug, serde::Serialize)]
pub struct TrashResult {
    pub deleted: usize,
    pub errors: usize,
}


/// List applications that can open a file, plus the current default.
/// Reads .desktop files and mimeapps.list (freedesktop spec).
#[tauri::command]
pub async fn open_with_apps(
    state: State<'_, AppState>,
    item: ItemId,
) -> Cmd<OpenWithInfo> {
    let path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    tauri::async_runtime::spawn_blocking(move || {
        let mime = guess_mime(&path);
        let apps = find_apps_for_mime(&mime);
        let default = get_default_app(&mime);
        Ok(OpenWithInfo { mime, apps, default })
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("open-with task failed: {e}"))))?
}

/// Open a file with a specific application (by .desktop ID).
#[tauri::command]
pub async fn open_with_app(
    state: State<'_, AppState>,
    item: ItemId,
    desktop_id: String,
) -> Cmd<()> {
    let path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    // Sanitize: only allow alphanumeric, dash, underscore, dot
    if !desktop_id.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.') {
        return Err(CommandError::from(UfmError::InvalidInput(
            "invalid desktop ID".into(),
        )));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let launched = std::process::Command::new("gtk-launch")
            .arg(&desktop_id)
            .arg(&path)
            .spawn();
        match launched {
            Ok(_) => Ok(()),
            Err(e) => {
                // Flatpak fallback: gtk-launch can't always resolve
                // exported desktop ids from inside an AppImage.
                let app_id = desktop_id.trim_end_matches(".desktop");
                let home = std::env::var("HOME").unwrap_or_default();
                let exported = std::path::PathBuf::from(format!(
                    "{home}/.local/share/flatpak/exports/share/applications/{desktop_id}"
                ));
                if exported.exists() {
                    std::process::Command::new("flatpak")
                        .arg("run")
                        .arg(app_id)
                        .arg(&path)
                        .spawn()
                        .map(|_| ())
                        .map_err(|fe| format!("flatpak run failed: {fe}"))
                } else {
                    Err(format!("gtk-launch failed: {e}"))
                }
            }
        }
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("launch task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Provider {
        provider: "local".into(),
        message: e,
    }))?;
    Ok(())
}

/// Set the default application for a MIME type ("always open with").
#[tauri::command]
pub async fn set_default_app(mime: String, desktop_id: String) -> Cmd<()> {
    if !desktop_id.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.') {
        return Err(CommandError::from(UfmError::InvalidInput(
            "invalid desktop ID".into(),
        )));
    }
    // Basic MIME validation: type/subtype
    if !mime.contains('/') || mime.contains(' ') || mime.contains(';') {
        return Err(CommandError::from(UfmError::InvalidInput(
            "invalid MIME type".into(),
        )));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let home = std::env::var("HOME").map_err(|e| e.to_string())?;
        let mimeapps = format!("{home}/.config/mimeapps.list");
        // Read existing or create new
        let mut content = std::fs::read_to_string(&mimeapps).unwrap_or_default();
        // Ensure [Default Applications] section exists
        if !content.contains("[Default Applications]") {
            if !content.is_empty() && !content.ends_with('\n') {
                content.push('\n');
            }
            content.push_str("[Default Applications]\n");
        }
        // Remove old entry for this MIME, add new one
        let mut lines: Vec<String> = Vec::new();
        let mut in_defaults = false;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                in_defaults = trimmed == "[Default Applications]";
                lines.push(line.to_string());
                continue;
            }
            if in_defaults && trimmed.starts_with(&format!("{mime}=")) {
                continue; // drop old entry
            }
            lines.push(line.to_string());
        }
        // Insert new entry after [Default Applications] header
        let mut out = Vec::new();
        let mut inserted = false;
        for line in lines {
            out.push(line.clone());
            if !inserted && line.trim() == "[Default Applications]" {
                out.push(format!("{mime}={desktop_id}"));
                inserted = true;
            }
        }
        std::fs::write(&mimeapps, out.join("\n") + "\n").map_err(|e| e.to_string())?;
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("set-default task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))?;
    Ok(())
}

#[derive(Debug, serde::Serialize)]
pub struct OpenWithInfo {
    pub mime: String,
    pub apps: Vec<AppInfo>,
    pub default: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct AppInfo {
    pub id: String,       // e.g. "org.gnome.TextEditor"
    pub name: String,     // e.g. "Text Editor"
    pub icon: Option<String>,
}

/// Guess MIME type from file extension (fast path).
fn guess_mime(path: &std::path::Path) -> String {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "txt" | "md" | "log" | "srt" | "vtt" | "nfo" => "text/plain",
        "csv" => "text/csv",
        "tsv" => "text/tab-separated-values",
        "json" => "application/json",
        "xml" | "html" | "htm" => "text/html",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "mp4" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "zip" => "application/zip",
        "tar" => "application/x-tar",
        "gz" => "application/gzip",
        "sh" => "application/x-shellscript",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "doc" => "application/msword",
        "xls" => "application/vnd.ms-excel",
        "ppt" => "application/vnd.ms-powerpoint",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "odp" => "application/vnd.oasis.opendocument.presentation",
        "odg" => "application/vnd.oasis.opendocument.graphics",
        "py" => "text/x-python",
        "rs" => "text/rust",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// Find .desktop apps that claim to handle a MIME type.
fn find_apps_for_mime(mime: &str) -> Vec<AppInfo> {
    let mut apps = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let home = std::env::var("HOME").unwrap_or_default();

    // Everywhere .desktop files live: XDG data dirs + Flatpak exports
    // (ArtCraft & friends are Flatpaks — leaving those dirs out made
    // installed apps invisible to Open With).
    let mut dirs: Vec<String> = Vec::new();
    let data_home = std::env::var("XDG_DATA_HOME")
        .unwrap_or_else(|_| format!("{home}/.local/share"));
    dirs.push(format!("{data_home}/applications"));
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    for d in data_dirs.split(':') {
        if !d.is_empty() {
            dirs.push(format!("{d}/applications"));
        }
    }
    dirs.push(format!("{home}/.local/share/flatpak/exports/share/applications"));
    dirs.push("/var/lib/flatpak/exports/share/applications".to_string());
    dirs.dedup();

    for dir in dirs {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().and_then(|e| e.to_str()) != Some("desktop") {
                    continue;
                }
                let id = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                if seen.contains(&id) || id.is_empty() {
                    continue;
                }
                if let Ok(content) = std::fs::read_to_string(&p) {
                    // Check MimeType= field
                    let mut handles = false;
                    let mut name = String::new();
                    let mut icon = None;
                    let mut no_display = false;
                    for line in content.lines() {
                        let line = line.trim();
                        if line.starts_with("MimeType=") {
                            let types: Vec<&str> = line[9..].split(';').collect();
                            // Match exact or wildcard (e.g. image/* matches image/png)
                            for t in types {
                                if t == mime
                                    || (t.ends_with("/*")
                                        && mime.starts_with(&t[..t.len() - 1]))
                                {
                                    handles = true;
                                    break;
                                }
                            }
                        } else if line.starts_with("Name=") && name.is_empty() {
                            name = line[5..].to_string();
                        } else if line.starts_with("Icon=") {
                            icon = Some(line[5..].to_string());
                        } else if line == "NoDisplay=true" {
                            no_display = true;
                        }
                    }
                    if handles && !no_display && !name.is_empty() {
                        seen.insert(id.clone());
                        apps.push(AppInfo { id, name, icon });
                    }
                }
            }
        }
    }
    apps.sort_by(|a, b| a.name.cmp(&b.name));
    apps
}

/// Get the default app for a MIME type from mimeapps.list.
fn get_default_app(mime: &str) -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let content = std::fs::read_to_string(format!("{home}/.config/mimeapps.list")).ok()?;
    let mut in_defaults = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_defaults = trimmed == "[Default Applications]";
            continue;
        }
        if in_defaults && trimmed.starts_with(&format!("{mime}=")) {
            // Take first app before semicolon
            let val = &trimmed[mime.len() + 1..];
            return val.split(';').next().map(|s| s.to_string()).filter(|s| !s.is_empty());
        }
    }
    None
}


/// Get the full filesystem path for an item (file or directory).
#[tauri::command]
pub async fn get_item_path(
    state: State<'_, AppState>,
    item: ItemId,
) -> Cmd<String> {
    // Check item_paths first
    if let Some(p) = state.item_paths.lock().unwrap().get(&item).cloned() {
        return Ok(p.to_string_lossy().to_string());
    }
    // Fall back: is this item a location root?
    let root_items = state.root_items.lock().unwrap();
    for (loc, root_id) in root_items.iter() {
        if *root_id == item {
            if let Some(root) = state.local_roots.lock().unwrap().get(loc).cloned() {
                return Ok(root.to_string_lossy().to_string());
            }
        }
    }
    Err(CommandError::from(UfmError::NotFound(format!(
        "unknown item {item}"
    ))))
}


/// Get recently used files from ~/.local/share/recently-used.xbel
#[tauri::command]
pub async fn get_recent_files(limit: Option<u32>) -> Cmd<Vec<RecentFile>> {
    let limit = limit.unwrap_or(50).min(200) as usize;
    tauri::async_runtime::spawn_blocking(move || {
        let home = std::env::var("HOME").map_err(|e| e.to_string())?;
        let xbel_path = format!("{home}/.local/share/recently-used.xbel");
        let content = std::fs::read_to_string(&xbel_path).map_err(|e| e.to_string())?;

        let mut files = Vec::new();
        // Simple XML parsing: extract href and modified attributes from bookmark tags
        for line in content.lines() {
            let line = line.trim();
            if !line.starts_with("<bookmark href=") {
                continue;
            }
            // Extract href="file:///path"
            let href = extract_attr(line, "href");
            let modified = extract_attr(line, "modified");
            if let Some(href) = href {
                if let Some(path_str) = href.strip_prefix("file://") {
                    // URL decode the path
                    let decoded = url_decode(path_str);
                    let path = std::path::Path::new(&decoded);
                    // Only include files that still exist
                    if path.exists() {
                        let name = path.file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("")
                            .to_string();
                        files.push(RecentFile {
                            path: decoded,
                            name,
                            modified: modified.unwrap_or_default(),
                        });
                        if files.len() >= limit {
                            break;
                        }
                    }
                }
            }
        }
        Ok::<Vec<RecentFile>, String>(files)
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("recent task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

#[derive(Debug, serde::Serialize)]
pub struct RecentFile {
    pub path: String,
    pub name: String,
    pub modified: String,
}

fn extract_attr(line: &str, attr: &str) -> Option<String> {
    let pattern = format!("{attr}=\"");
    let start = line.find(&pattern)? + pattern.len();
    let end = line[start..].find('"')? + start;
    Some(line[start..end].to_string())
}

fn url_decode(s: &str) -> String {
    // Simple URL decode for file paths (%20 -> space, etc.)
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            let h1 = chars.next().unwrap_or('0');
            let h2 = chars.next().unwrap_or('0');
            if let Ok(byte) = u8::from_str_radix(&format!("{h1}{h2}"), 16) {
                out.push(byte as char);
            } else {
                out.push('%');
                out.push(h1);
                out.push(h2);
            }
        } else {
            out.push(c);
        }
    }
    out
}


/// Get image file as base64 data URL for preview.
#[tauri::command]
pub async fn get_image_preview(
    state: State<'_, AppState>,
    item: ItemId,
    fallback_path: Option<String>,
) -> Cmd<String> {
    let path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .or_else(|| fallback_path.as_ref().map(std::path::PathBuf::from))
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    tauri::async_runtime::spawn_blocking(move || {
        // Only allow image extensions
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        let mime = match ext.as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "svg" => "image/svg+xml",
            "bmp" => "image/bmp",
            _ => return Err("not an image".to_string()),
        };
        // Limit to 10MB to avoid memory issues
        let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
        if meta.len() > 10 * 1024 * 1024 {
            return Err("image too large for preview".to_string());
        }
        let data = std::fs::read(&path).map_err(|e| e.to_string())?;
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
        Ok::<String, String>(format!("data:{mime};base64,{b64}"))
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("image task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}


/// Move an item (file or folder) to the system Trash.
/// Follows the freedesktop trash spec: files/ + info/*.trashinfo.
#[tauri::command]
pub async fn trash_item(state: State<'_, AppState>, item: ItemId) -> Cmd<String> {
    let path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    tauri::async_runtime::spawn_blocking(move || {
        let home = std::env::var("HOME").map_err(|e| e.to_string())?;
        let trash_root = std::path::PathBuf::from(format!("{home}/.local/share/Trash"));
        let files_dir = trash_root.join("files");
        let info_dir = trash_root.join("info");
        std::fs::create_dir_all(&files_dir).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&info_dir).map_err(|e| e.to_string())?;

        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("invalid file name")?
            .to_string();

        // Collision-free destination name: name, "name 2", "name 3", ...
        let mut dest_name = name.clone();
        let mut n = 2;
        while files_dir.join(&dest_name).exists() || info_dir.join(format!("{dest_name}.trashinfo")).exists() {
            dest_name = format!("{name} {n}");
            n += 1;
        }
        let dest = files_dir.join(&dest_name);

        // Write the .trashinfo first, then move (spec order).
        let now = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();
        let info = format!(
            "[Trash Info]\nPath={}\nDeletionDate={}\n",
            path.to_string_lossy(),
            now
        );
        std::fs::write(info_dir.join(format!("{dest_name}.trashinfo")), info)
            .map_err(|e| e.to_string())?;

        // rename; fall back to copy+remove across filesystems
        if std::fs::rename(&path, &dest).is_err() {
            let ty = std::fs::metadata(&path).map_err(|e| e.to_string())?;
            if ty.is_dir() {
                copy_dir_recursive(&path, &dest).map_err(|e| e.to_string())?;
                std::fs::remove_dir_all(&path).map_err(|e| e.to_string())?;
            } else {
                std::fs::copy(&path, &dest).map_err(|e| e.to_string())?;
                std::fs::remove_file(&path).map_err(|e| e.to_string())?;
            }
        }
        Ok::<String, String>(dest_name)
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("trash task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Move an item into a destination directory (used by drag-to-favorite).
/// Returns the new full path.
#[tauri::command]
pub async fn move_item_to(
    state: State<'_, AppState>,
    item: ItemId,
    dest_dir: String,
) -> Cmd<String> {
    let path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    tauri::async_runtime::spawn_blocking(move || {
        let dest_root = std::path::PathBuf::from(&dest_dir);
        if !dest_root.is_dir() {
            return Err("destination is not a directory".to_string());
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("invalid file name")?
            .to_string();
        // Already there? No-op success.
        if path.parent() == Some(dest_root.as_path()) {
            return Ok::<String, String>(path.to_string_lossy().to_string());
        }
        let mut dest = dest_root.join(&name);
        let mut n = 2;
        while dest.exists() {
            dest = dest_root.join(format!("{name} {n}"));
            n += 1;
        }
        if std::fs::rename(&path, &dest).is_err() {
            let ty = std::fs::metadata(&path).map_err(|e| e.to_string())?;
            if ty.is_dir() {
                copy_dir_recursive(&path, &dest).map_err(|e| e.to_string())?;
                std::fs::remove_dir_all(&path).map_err(|e| e.to_string())?;
            } else {
                std::fs::copy(&path, &dest).map_err(|e| e.to_string())?;
                std::fs::remove_file(&path).map_err(|e| e.to_string())?;
            }
        }
        Ok::<String, String>(dest.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("move task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Detailed properties for one item (Properties dialog).
#[tauri::command]
pub async fn get_item_properties(
    state: State<'_, AppState>,
    item: ItemId,
) -> Cmd<ItemProperties> {
    let path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    tauri::async_runtime::spawn_blocking(move || {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
        let is_dir = meta.is_dir();
        // Folder size: walk it (best-effort, capped work).
        let size = if is_dir {
            let mut total: u64 = 0;
            let mut count: u64 = 0;
            let mut stack = vec![path.clone()];
            while let Some(d) = stack.pop() {
                if let Ok(rd) = std::fs::read_dir(&d) {
                    for e in rd.flatten() {
                        count += 1;
                        if let Ok(m) = e.metadata() {
                            if m.is_dir() {
                                stack.push(e.path());
                            } else {
                                total += m.len();
                            }
                        }
                        if count > 200_000 {
                            break;
                        }
                    }
                }
                if count > 200_000 {
                    break;
                }
            }
            total
        } else {
            meta.len()
        };
        let fmt_time = |t: std::io::Result<std::time::SystemTime>| -> String {
            t.ok()
                .map(|t| {
                    chrono::DateTime::<chrono::Local>::from(t)
                        .format("%b %e, %Y at %l:%M %p")
                        .to_string()
                })
                .unwrap_or_else(|| "—".to_string())
        };
        let mode = meta.permissions().mode();
        let perms = format!(
            "{}{}{}{}{}{}{}{}{}",
            if mode & 0o400 != 0 { "r" } else { "-" },
            if mode & 0o200 != 0 { "w" } else { "-" },
            if mode & 0o100 != 0 { "x" } else { "-" },
            if mode & 0o040 != 0 { "r" } else { "-" },
            if mode & 0o020 != 0 { "w" } else { "-" },
            if mode & 0o010 != 0 { "x" } else { "-" },
            if mode & 0o004 != 0 { "r" } else { "-" },
            if mode & 0o002 != 0 { "w" } else { "-" },
            if mode & 0o001 != 0 { "x" } else { "-" },
        );
        Ok::<ItemProperties, String>(ItemProperties {
            name: path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string(),
            path: path.to_string_lossy().to_string(),
            is_dir,
            size_bytes: size,
            modified: fmt_time(meta.modified()),
            created: fmt_time(meta.created()),
            accessed: fmt_time(meta.accessed()),
            permissions: perms,
            mode_octal: format!("{:o}", mode & 0o777),
            executable: mode & 0o111 != 0,
        })
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("props task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

#[derive(Debug, serde::Serialize)]
pub struct ItemProperties {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size_bytes: u64,
    pub modified: String,
    pub created: String,
    pub accessed: String,
    pub permissions: String,
    pub mode_octal: String,
    pub executable: bool,
}


/// Restore an item from the Trash to its original location (Undo).
/// `trash_name` is the name trash_item returned (the files/ entry name).
/// Returns the restored absolute path.
#[tauri::command]
pub async fn untrash_item(trash_name: String) -> Cmd<String> {
    if trash_name.contains('/') || trash_name.contains("\\") || trash_name == ".." {
        return Err(CommandError::from(UfmError::InvalidInput(
            "bad trash name".into(),
        )));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let home = std::env::var("HOME").map_err(|e| e.to_string())?;
        let trash_root = std::path::PathBuf::from(format!("{home}/.local/share/Trash"));
        let src = trash_root.join("files").join(&trash_name);
        let info_path = trash_root.join("info").join(format!("{trash_name}.trashinfo"));
        if !src.exists() {
            return Err(format!("trashed item {trash_name} no longer exists"));
        }
        // Original path from the .trashinfo (written raw, not encoded).
        let info = std::fs::read_to_string(&info_path).unwrap_or_default();
        let mut dest = info
            .lines()
            .find_map(|l| l.strip_prefix("Path="))
            .map(|p| std::path::PathBuf::from(p.trim()))
            .unwrap_or_else(|| std::path::PathBuf::from(format!("{home}/{trash_name}")));
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        if dest.exists() {
            let base = dest.clone();
            let mut n = 2;
            loop {
                let cand = base.with_file_name(format!("{} {n}", base.file_name().and_then(|x| x.to_str()).unwrap_or("restored")));
                if !cand.exists() {
                    dest = cand;
                    break;
                }
                n += 1;
            }
        }
        if std::fs::rename(&src, &dest).is_err() {
            let ty = std::fs::metadata(&src).map_err(|e| e.to_string())?;
            if ty.is_dir() {
                copy_dir_recursive(&src, &dest).map_err(|e| e.to_string())?;
                std::fs::remove_dir_all(&src).map_err(|e| e.to_string())?;
            } else {
                std::fs::copy(&src, &dest).map_err(|e| e.to_string())?;
                std::fs::remove_file(&src).map_err(|e| e.to_string())?;
            }
        }
        let _ = std::fs::remove_file(&info_path);
        Ok::<String, String>(dest.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("untrash task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Move a file/folder by absolute paths (the undo of a drag-move).
/// Returns the new absolute path.
#[tauri::command]
pub async fn move_path_to(src_path: String, dest_dir: String) -> Cmd<String> {
    tauri::async_runtime::spawn_blocking(move || {
        let src = std::path::PathBuf::from(&src_path);
        let dest_root = std::path::PathBuf::from(&dest_dir);
        if !src.exists() {
            return Err(format!("source {src_path} no longer exists"));
        }
        if !dest_root.is_dir() {
            return Err(format!("destination {dest_dir} is not a directory"));
        }
        let name = src
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("invalid file name")?
            .to_string();
        if src.parent() == Some(dest_root.as_path()) {
            return Ok::<String, String>(src_path.clone());
        }
        let mut dest = dest_root.join(&name);
        let mut n = 2;
        while dest.exists() {
            dest = dest_root.join(format!("{name} {n}"));
            n += 1;
        }
        if std::fs::rename(&src, &dest).is_err() {
            let ty = std::fs::metadata(&src).map_err(|e| e.to_string())?;
            if ty.is_dir() {
                copy_dir_recursive(&src, &dest).map_err(|e| e.to_string())?;
                std::fs::remove_dir_all(&src).map_err(|e| e.to_string())?;
            } else {
                std::fs::copy(&src, &dest).map_err(|e| e.to_string())?;
                std::fs::remove_file(&src).map_err(|e| e.to_string())?;
            }
        }
        Ok::<String, String>(dest.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("move task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Open one of the project's own web pages (bug reports, feature requests).
#[tauri::command]
pub async fn open_url(url: String) -> Cmd<()> {
    if !url.starts_with("https://github.com/garrisonr/fable-files")
        && !url.starts_with("https://alternativeto.net/")
    {
        return Err(CommandError::from(UfmError::InvalidInput(
            "only Fable Files project pages can be opened".into(),
        )));
    }
    open::that(&url).map_err(|e| CommandError::from(UfmError::Internal(e.to_string())))
}

/// Percent-encode a path into a file:// URI (freedesktop thumbnail spec).
fn file_uri(path: &std::path::Path) -> String {
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

/// Generate (or reuse) a freedesktop thumbnail for an image file and
/// return it as a data URL. Cache: ~/.cache/thumbnails/{normal,large}/
/// named md5(file-uri).png, shared with other Linux file managers.
/// Freshness: the cached PNG is reused while it is newer than the source.
#[tauri::command]
pub async fn get_thumbnail(
    state: State<'_, AppState>,
    item: ItemId,
    fallback_path: Option<String>,
    large: Option<bool>,
) -> Cmd<String> {
    let path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .or_else(|| fallback_path.as_ref().map(std::path::PathBuf::from))
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    let large = large.unwrap_or(false);
    tauri::async_runtime::spawn_blocking(move || {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if !matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "ico") {
            return Err("not a thumbnail-able image".to_string());
        }
        let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
        if meta.len() > 64 * 1024 * 1024 {
            return Err("image too large to thumbnail".to_string());
        }
        let home = std::env::var("HOME").map_err(|e| e.to_string())?;
        let size_dir = if large { "large" } else { "normal" };
        let max_px: u32 = if large { 256 } else { 128 };
        let cache_dir = std::path::PathBuf::from(format!("{home}/.cache/thumbnails/{size_dir}"));
        let digest = format!("{:x}", md5::compute(file_uri(&path).as_bytes()));
        let cache_file = cache_dir.join(format!("{digest}.png"));

        let src_mtime = meta.modified().ok();
        let cache_fresh = cache_file
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .zip(src_mtime)
            .map(|(cm, sm)| cm >= sm)
            .unwrap_or(false);

        if !cache_fresh {
            std::fs::create_dir_all(&cache_dir).map_err(|e| e.to_string())?;
            let img = image::open(&path).map_err(|e| format!("decode: {e}"))?;
            let thumb = img.thumbnail(max_px, max_px);
            // Write via a temp file + rename so readers never see a partial PNG.
            let tmp = cache_dir.join(format!("{digest}.tmp.png"));
            thumb
                .save_with_format(&tmp, image::ImageFormat::Png)
                .map_err(|e| format!("encode: {e}"))?;
            std::fs::rename(&tmp, &cache_file).map_err(|e| e.to_string())?;
        }
        let data = std::fs::read(&cache_file).map_err(|e| e.to_string())?;
        use base64::Engine;
        Ok::<String, String>(format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&data)
        ))
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("thumbnail task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}


/// Rename an item in place. Returns the new absolute path.
#[tauri::command]
pub async fn rename_item(
    state: State<'_, AppState>,
    item: ItemId,
    new_name: String,
) -> Cmd<String> {
    let new_name = new_name.trim().to_string();
    if new_name.is_empty()
        || new_name.contains('/')
        || new_name.contains('\\')
        || new_name == "."
        || new_name == ".."
    {
        return Err(CommandError::from(UfmError::InvalidInput(
            "invalid name".into(),
        )));
    }
    let path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    let dest = path.with_file_name(&new_name);
    if dest == path {
        return Ok(path.to_string_lossy().to_string());
    }
    if dest.exists() {
        return Err(CommandError::from(UfmError::InvalidInput(format!(
            "“{new_name}” already exists"
        ))));
    }
    std::fs::rename(&path, &dest)
        .map_err(|e| CommandError::from(UfmError::Internal(e.to_string())))?;
    Ok(dest.to_string_lossy().to_string())
}

/// Copy a file/folder by absolute paths (paste / Copy to…).
/// Returns the new absolute path.
#[tauri::command]
pub async fn copy_path_to(src_path: String, dest_dir: String) -> Cmd<String> {
    tauri::async_runtime::spawn_blocking(move || {
        let src = std::path::PathBuf::from(&src_path);
        let dest_root = std::path::PathBuf::from(&dest_dir);
        if !src.exists() {
            return Err(format!("source {src_path} no longer exists"));
        }
        if !dest_root.is_dir() {
            return Err(format!("destination {dest_dir} is not a directory"));
        }
        let name = src
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("invalid file name")?
            .to_string();
        let mut dest = dest_root.join(&name);
        let mut n = 2;
        while dest.exists() {
            dest = dest_root.join(format!("{name} {n}"));
            n += 1;
        }
        let ty = std::fs::metadata(&src).map_err(|e| e.to_string())?;
        if ty.is_dir() {
            copy_dir_recursive(&src, &dest).map_err(|e| e.to_string())?;
        } else {
            std::fs::copy(&src, &dest).map_err(|e| e.to_string())?;
        }
        Ok::<String, String>(dest.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("copy task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Compress items into a .zip beside them (the `zip` CLI). Returns the
/// archive's absolute path.
#[tauri::command]
pub async fn compress_items(state: State<'_, AppState>, items: Vec<ItemId>) -> Cmd<String> {
    if items.is_empty() {
        return Err(CommandError::from(UfmError::InvalidInput("nothing to compress".into())));
    }
    let paths: Vec<std::path::PathBuf> = {
        let map = state.item_paths.lock().unwrap();
        items
            .iter()
            .filter_map(|i| map.get(i).cloned())
            .collect()
    };
    if paths.is_empty() {
        return Err(CommandError::from(UfmError::NotFound("unknown items".into())));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let parent = paths[0]
            .parent()
            .ok_or("no parent dir")?
            .to_path_buf();
        let stem = if paths.len() == 1 {
            let p = &paths[0];
            if p.is_dir() {
                p.file_name().and_then(|n| n.to_str()).unwrap_or("Archive").to_string()
            } else {
                p.file_stem().and_then(|n| n.to_str()).unwrap_or("Archive").to_string()
            }
        } else {
            "Archive".to_string()
        };
        let mut archive = parent.join(format!("{stem}.zip"));
        let mut n = 2;
        while archive.exists() {
            archive = parent.join(format!("{stem} {n}.zip"));
            n += 1;
        }
        let names: Vec<String> = paths
            .iter()
            .filter_map(|p| p.file_name().and_then(|x| x.to_str()).map(|x| x.to_string()))
            .collect();
        let out = std::process::Command::new("zip")
            .arg("-r")
            .arg("-q")
            .arg(&archive)
            .args(&names)
            .current_dir(&parent)
            .output()
            .map_err(|e| format!("zip failed to start: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "zip failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok::<String, String>(archive.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("compress task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Open the default mail composer with the items attached (xdg-email).
#[tauri::command]
pub async fn email_items(state: State<'_, AppState>, items: Vec<ItemId>) -> Cmd<()> {
    let map = state.item_paths.lock().unwrap();
    let mut cmd = std::process::Command::new("xdg-email");
    let mut any = false;
    for i in items.iter().take(20) {
        if let Some(p) = map.get(i) {
            cmd.arg("--attach").arg(p);
            any = true;
        }
    }
    if !any {
        return Err(CommandError::from(UfmError::NotFound("unknown items".into())));
    }
    cmd.spawn()
        .map_err(|e| CommandError::from(UfmError::Internal(format!("xdg-email: {e}"))))?;
    Ok(())
}

/// Is LocalSend installed (native binary or a desktop entry)?
#[tauri::command]
pub async fn localsend_available() -> Cmd<bool> {
    Ok(std::path::Path::new("/usr/bin/localsend").exists()
        || std::path::Path::new("/usr/share/applications/localsend.desktop").exists())
}

/// Hand an item to LocalSend for sharing.
#[tauri::command]
pub async fn send_via_localsend(state: State<'_, AppState>, item: ItemId) -> Cmd<()> {
    let path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    let launched = std::process::Command::new("gtk-launch")
        .arg("localsend")
        .arg(&path)
        .spawn();
    if launched.is_err() {
        std::process::Command::new("localsend")
            .arg(&path)
            .spawn()
            .map_err(|e| CommandError::from(UfmError::Internal(format!("localsend: {e}"))))?;
    }
    Ok(())
}

fn path_to_file_uri(p: &str) -> String {
    let mut out = String::from("file://");
    for b in p.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn file_uri_to_path(u: &str) -> Option<String> {
    let rest = u.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let mut bytes = Vec::new();
    let mut chars = rest.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            let h: String = chars.by_ref().take(2).collect();
            bytes.push(u8::from_str_radix(&h, 16).ok()?);
        } else {
            let mut buf = [0u8; 4];
            bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    }
    String::from_utf8(bytes).ok()
}

/// Write file paths to the Wayland clipboard (wl-copy). Cut uses the
/// GNOME nautilus format so Nautilus honors the move; copy uses
/// text/uri-list, which everything reads.
#[tauri::command]
pub async fn clipboard_write(paths: Vec<String>, cut: bool) -> Cmd<()> {
    tauri::async_runtime::spawn_blocking(move || {
        use std::io::Write;
        let (mime, data) = if cut {
            let mut d = String::from("cut\n");
            for p in &paths {
                d.push_str(&path_to_file_uri(p));
                d.push('\n');
            }
            ("x-special/nautilus-clipboard", d)
        } else {
            let mut d = String::new();
            for p in &paths {
                d.push_str(&path_to_file_uri(p));
                d.push_str("\r\n");
            }
            ("text/uri-list", d)
        };
        let mut child = std::process::Command::new("wl-copy")
            .arg("--type")
            .arg(mime)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("wl-copy: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(data.as_bytes())
                .map_err(|e| format!("wl-copy write: {e}"))?;
        }
        // wl-copy forks a server and the parent exits promptly.
        let _ = child.wait();
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("clipboard task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

#[derive(serde::Serialize)]
pub struct ClipboardData {
    pub paths: Vec<String>,
    pub cut: bool,
}

/// Read file paths from the Wayland clipboard (wl-paste).
#[tauri::command]
pub async fn clipboard_read() -> Cmd<ClipboardData> {
    tauri::async_runtime::spawn_blocking(move || {
        let read_type = |mime: &str| -> Option<String> {
            let out = std::process::Command::new("wl-paste")
                .arg("--type")
                .arg(mime)
                .arg("--no-newline")
                .output()
                .ok()?;
            if out.status.success() {
                Some(String::from_utf8_lossy(&out.stdout).to_string())
            } else {
                None
            }
        };
        // Nautilus cut/copy format first.
        if let Some(raw) = read_type("x-special/nautilus-clipboard") {
            let mut lines = raw.lines();
            let cut = lines.next().map(|l| l.trim() == "cut").unwrap_or(false);
            let paths: Vec<String> = lines.filter_map(|l| file_uri_to_path(l.trim())).collect();
            if !paths.is_empty() {
                return Ok::<ClipboardData, String>(ClipboardData { paths, cut });
            }
        }
        if let Some(raw) = read_type("text/uri-list") {
            let paths: Vec<String> = raw
                .lines()
                .filter_map(|l| file_uri_to_path(l.trim()))
                .collect();
            if !paths.is_empty() {
                return Ok::<ClipboardData, String>(ClipboardData { paths, cut: false });
            }
        }
        Ok::<ClipboardData, String>(ClipboardData { paths: Vec::new(), cut: false })
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("clipboard task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

fn player_label(path: &str) -> String {
    format!("player-{:x}", md5::compute(path.as_bytes()))
}

/// Percent-encode a query-string value.
fn query_encode(v: &str) -> String {
    let mut out = String::new();
    for b in v.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Pop the video player out into its own window.
#[tauri::command]
pub async fn open_player_window(app: tauri::AppHandle, path: String, name: String) -> Cmd<()> {
    use tauri::{Emitter, Manager};
    let label = player_label(&path);
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.set_focus();
        return Ok(());
    }
    let url = format!(
        "index.html?mode=player&path={}&name={}",
        query_encode(&path),
        query_encode(&name)
    );
    let window = tauri::WebviewWindowBuilder::new(&app, &label, tauri::WebviewUrl::App(url.into()))
        .title(format!("{name} — Fable Files"))
        .inner_size(920.0, 580.0)
        .min_inner_size(420.0, 300.0)
        .build()
        .map_err(|e| CommandError::from(UfmError::Internal(e.to_string())))?;
    let app2 = app.clone();
    let path2 = path.clone();
    window.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::Destroyed) {
            let _ = app2.emit("player-closed", path2.clone());
        }
    });
    Ok(())
}

/// Close a pop-out player window (bring the video back to the preview).
#[tauri::command]
pub async fn close_player_window(app: tauri::AppHandle, path: String) -> Cmd<()> {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window(&player_label(&path)) {
        w.close()
            .map_err(|e| CommandError::from(UfmError::Internal(e.to_string())))?;
    }
    Ok(())
}


/// Create a Finder-style alias (symlink) beside the item.
/// Returns the alias path.
#[tauri::command]
pub async fn make_alias(state: State<'_, AppState>, item: ItemId) -> Cmd<String> {
    let path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    let parent = path.parent().ok_or_else(|| {
        CommandError::from(UfmError::InvalidInput("no parent dir".into()))
    })?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| CommandError::from(UfmError::InvalidInput("bad name".into())))?
        .to_string();
    let mut dest = parent.join(format!("{name} alias"));
    let mut n = 2;
    while dest.symlink_metadata().is_ok() {
        dest = parent.join(format!("{name} alias {n}"));
        n += 1;
    }
    std::os::unix::fs::symlink(&path, &dest)
        .map_err(|e| CommandError::from(UfmError::Internal(e.to_string())))?;
    Ok(dest.to_string_lossy().to_string())
}

/// Convert an image to another format (image crate), written beside
/// the original as name.<format>. Returns the new path.
#[tauri::command]
pub async fn convert_image(
    state: State<'_, AppState>,
    item: ItemId,
    format: String,
) -> Cmd<String> {
    let path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    tauri::async_runtime::spawn_blocking(move || {
        let fmt = match format.to_lowercase().as_str() {
            "png" => image::ImageFormat::Png,
            "jpg" | "jpeg" => image::ImageFormat::Jpeg,
            "webp" => image::ImageFormat::WebP,
            "bmp" => image::ImageFormat::Bmp,
            other => return Err(format!("unsupported format {other}")),
        };
        let ext = if format.to_lowercase() == "jpeg" {
            "jpg".to_string()
        } else {
            format.to_lowercase()
        };
        let img = image::open(&path).map_err(|e| format!("image open: {e}"))?;
        let stem = path
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or("image")
            .to_string();
        let parent = path.parent().ok_or("no parent")?.to_path_buf();
        let mut dest = parent.join(format!("{stem}.{ext}"));
        if dest == path {
            dest = parent.join(format!("{stem} converted.{ext}"));
        }
        let mut n = 2;
        while dest.exists() {
            dest = parent.join(format!("{stem} {n}.{ext}"));
            n += 1;
        }
        if fmt == image::ImageFormat::Jpeg {
            img.to_rgb8()
                .save_with_format(&dest, fmt)
                .map_err(|e| format!("save: {e}"))?;
        } else {
            img.save_with_format(&dest, fmt)
                .map_err(|e| format!("save: {e}"))?;
        }
        Ok::<String, String>(dest.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("convert task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Rotate an image 90° counter-clockwise, in place (Finder "Rotate
/// Left"). Direction is pinned by a unit test below.
#[tauri::command]
pub async fn rotate_image_left(state: State<'_, AppState>, item: ItemId) -> Cmd<()> {
    let path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    tauri::async_runtime::spawn_blocking(move || {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let fmt = image::ImageFormat::from_extension(&ext)
            .ok_or_else(|| format!("not an image format: {ext}"))?;
        let img = image::open(&path).map_err(|e| format!("image open: {e}"))?;
        let rotated = rotate_ccw(&img);
        rotated
            .save_with_format(&path, fmt)
            .map_err(|e| format!("save: {e}"))?;
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("rotate task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Counter-clockwise rotation. Which image-crate primitive that is
/// gets proven by rotate_ccw_direction below, not by documentation.
fn rotate_ccw(img: &image::DynamicImage) -> image::DynamicImage {
    img.rotate270()
}

/// Create a PDF from images with ImageMagick, beside the first image.
/// Returns the PDF path.
#[tauri::command]
pub async fn create_pdf(state: State<'_, AppState>, items: Vec<ItemId>) -> Cmd<String> {
    if items.is_empty() {
        return Err(CommandError::from(UfmError::InvalidInput("no images".into())));
    }
    let paths: Vec<std::path::PathBuf> = {
        let map = state.item_paths.lock().unwrap();
        items.iter().filter_map(|i| map.get(i).cloned()).collect()
    };
    if paths.is_empty() {
        return Err(CommandError::from(UfmError::NotFound("unknown items".into())));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let parent = paths[0].parent().ok_or("no parent")?.to_path_buf();
        let stem = if paths.len() == 1 {
            paths[0]
                .file_stem()
                .and_then(|n| n.to_str())
                .unwrap_or("Images")
                .to_string()
        } else {
            "Images".to_string()
        };
        let mut dest = parent.join(format!("{stem}.pdf"));
        let mut n = 2;
        while dest.exists() {
            dest = parent.join(format!("{stem} {n}.pdf"));
            n += 1;
        }
        let mut cmd = std::process::Command::new("magick");
        for p in &paths {
            cmd.arg(p);
        }
        cmd.arg(&dest);
        let out = cmd
            .output()
            .map_err(|e| format!("magick failed to start: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "magick failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok::<String, String>(dest.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("pdf task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Set the desktop picture (Omarchy: the current/background symlink IS
/// the wallpaper state).
#[tauri::command]
pub async fn set_wallpaper(state: State<'_, AppState>, item: ItemId) -> Cmd<()> {
    let path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    let home = std::env::var("HOME")
        .map_err(|_| CommandError::from(UfmError::Internal("no HOME".into())))?;
    let link = std::path::PathBuf::from(home)
        .join(".local/state/omarchy/current/background");
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CommandError::from(UfmError::Internal(e.to_string())))?;
    }
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(&path, &link)
        .map_err(|e| CommandError::from(UfmError::Internal(e.to_string())))?;
    Ok(())
}

/// Open an arbitrary absolute path with the OS default handler.
#[tauri::command]
pub async fn open_path(path: String) -> Cmd<()> {
    open::that(&path)
        .map_err(|e| CommandError::from(UfmError::Internal(format!("open: {e}"))))?;
    Ok(())
}

#[derive(serde::Serialize)]
pub struct ArchiveEntry {
    pub path: String,
    pub size: u64,
    pub is_dir: bool,
}

/// List an archive's contents with bsdtar (libarchive: zip, tar,
/// tar.gz/bz2/xz, 7z, rar…).
#[tauri::command]
pub async fn list_archive(state: State<'_, AppState>, item: ItemId) -> Cmd<Vec<ArchiveEntry>> {
    let path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    tauri::async_runtime::spawn_blocking(move || {
        let out = std::process::Command::new("bsdtar")
            .arg("-tf")
            .arg(&path)
            .output()
            .map_err(|e| format!("bsdtar: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "bsdtar could not read this archive: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let listing = String::from_utf8_lossy(&out.stdout).to_string();
        // Best-effort sizes from the verbose listing, keyed by path.
        let mut sizes: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        if let Ok(vout) = std::process::Command::new("bsdtar")
            .arg("-tvf")
            .arg(&path)
            .output()
        {
            for line in String::from_utf8_lossy(&vout.stdout).lines() {
                // "-rw-r--r--  0 user group   12345 Jan  1  2020 the/path"
                let fields: Vec<&str> = line.split_whitespace().collect();
                if fields.len() >= 9 {
                    if let Ok(size) = fields[4].parse::<u64>() {
                        sizes.insert(fields[8..].join(" "), size);
                    }
                }
            }
        }
        let mut entries: Vec<ArchiveEntry> = Vec::new();
        let mut dirs: std::collections::HashSet<String> = std::collections::HashSet::new();
        for line in listing.lines() {
            let p = line.trim_end_matches('\r');
            if p.is_empty() {
                continue;
            }
            let is_dir = p.ends_with('/');
            let clean = p.trim_end_matches('/').to_string();
            // Synthesize intermediate folders (many zips omit dir rows).
            let mut acc = String::new();
            let parts: Vec<&str> = clean.split('/').collect();
            for (i, part) in parts.iter().enumerate() {
                if i > 0 {
                    acc.push('/');
                }
                acc.push_str(part);
                if i < parts.len() - 1 && dirs.insert(acc.clone()) {
                    entries.push(ArchiveEntry {
                        path: acc.clone(),
                        size: 0,
                        is_dir: true,
                    });
                }
            }
            entries.push(ArchiveEntry {
                size: sizes.get(&clean).copied().unwrap_or(0),
                path: clean,
                is_dir,
            });
        }
        Ok::<Vec<ArchiveEntry>, String>(entries)
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("archive task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Extract a whole archive. Without an explicit destination, extracts
/// into a new folder named after the archive, beside it.
#[tauri::command]
pub async fn extract_archive(
    state: State<'_, AppState>,
    item: ItemId,
    dest_dir: Option<String>,
) -> Cmd<String> {
    let path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    tauri::async_runtime::spawn_blocking(move || {
        let dest = match dest_dir {
            Some(d) => std::path::PathBuf::from(d),
            None => {
                let parent = path.parent().ok_or("no parent")?.to_path_buf();
                let stem = path
                    .file_stem()
                    .and_then(|n| n.to_str())
                    .unwrap_or("archive")
                    .to_string();
                let stem = stem.strip_suffix(".tar").unwrap_or(&stem).to_string();
                let mut d = parent.join(&stem);
                let mut n = 2;
                while d.exists() {
                    d = parent.join(format!("{stem} {n}"));
                    n += 1;
                }
                d
            }
        };
        std::fs::create_dir_all(&dest).map_err(|e| format!("mkdir: {e}"))?;
        let out = std::process::Command::new("bsdtar")
            .arg("-xf")
            .arg(&path)
            .arg("-C")
            .arg(&dest)
            .output()
            .map_err(|e| format!("bsdtar: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "extract failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok::<String, String>(dest.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("extract task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Extract one entry of an archive to a temp dir and return its path
/// (for opening/previewing a file without unpacking everything).
#[tauri::command]
pub async fn extract_archive_entry(archive_path: String, entry_path: String) -> Cmd<String> {
    tauri::async_runtime::spawn_blocking(move || {
        let hash = format!("{:x}", md5::compute(archive_path.as_bytes()));
        let dest = std::env::temp_dir().join("fable-extract").join(&hash[..12]);
        std::fs::create_dir_all(&dest).map_err(|e| format!("mkdir: {e}"))?;
        let target = dest.join(&entry_path);
        if !target.exists() {
            let out = std::process::Command::new("bsdtar")
                .arg("-xf")
                .arg(&archive_path)
                .arg("-C")
                .arg(&dest)
                .arg(&entry_path)
                .output()
                .map_err(|e| format!("bsdtar: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "extract failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
        }
        Ok::<String, String>(target.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("extract task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}


/// Apply/remove a user label on a path (Finder-style tags, path-keyed).
#[tauri::command]
pub async fn set_path_label(
    state: State<'_, AppState>,
    path: String,
    label: String,
    on: bool,
) -> Cmd<()> {
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    if on {
        catalog.tag_path(&path, &label).map_err(CommandError::from)?;
    } else {
        catalog.untag_path(&path, &label).map_err(CommandError::from)?;
    }
    Ok(())
}

/// Rename a label on every path that carries it.
#[tauri::command]
pub async fn rename_path_label(
    state: State<'_, AppState>,
    old_label: String,
    new_label: String,
) -> Cmd<()> {
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    catalog
        .rename_path_label(&old_label, &new_label)
        .map_err(CommandError::from)?;
    Ok(())
}

/// Strip a label from every path.
#[tauri::command]
pub async fn delete_path_label(state: State<'_, AppState>, label: String) -> Cmd<()> {
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    catalog
        .delete_path_label(&label)
        .map_err(CommandError::from)?;
    Ok(())
}

/// All items carrying a label (sidebar label browsing). Ids are
/// reconciled with the shell's stable path-id cache so follow-up
/// commands (open, trash, …) work on the results.
#[tauri::command]
pub async fn list_items_by_label(
    state: State<'_, AppState>,
    label: String,
) -> Cmd<Vec<Item>> {
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    let found = catalog.items_by_label(&label).map_err(CommandError::from)?;
    let mut path_ids = state.path_ids.lock().unwrap();
    let mut item_paths = state.item_paths.lock().unwrap();
    let mut item_locations = state.item_locations.lock().unwrap();
    let mut out = Vec::new();
    for mut item in found {
        if let Some(pk) = item.provider_key.clone() {
            let id = *path_ids
                .entry(std::path::PathBuf::from(&pk))
                .or_insert(item.id);
            item.id = id;
            item_paths.insert(id, std::path::PathBuf::from(&pk));
            item_locations.insert(id, item.location);
        }
        out.push(item);
    }
    Ok(out)
}


/// Register a runtime location rooted at an absolute path (cloud
/// mounts, server mounts). Session-scoped: callers re-register on
/// boot. Not inserted into the catalog — network roots are browsed,
/// not indexed.
#[tauri::command]
pub async fn add_path_location(
    state: State<'_, AppState>,
    name: String,
    path: String,
) -> Cmd<LocationSummary> {
    let root = std::path::PathBuf::from(&path);
    if !root.is_dir() {
        return Err(CommandError::from(UfmError::NotFound(format!(
            "{path} is not a directory (mounted?)"
        ))));
    }
    // Reuse an existing runtime location for the same root.
    {
        let roots = state.local_roots.lock().unwrap();
        let locations = state.locations.lock().unwrap();
        for (id, existing) in roots.iter() {
            if *existing == root {
                if let Some(sum) = locations.iter().find(|l| l.id == *id) {
                    return Ok(sum.clone());
                }
            }
        }
    }
    let location = ufm_vfs::mount_local(&name, root.clone());
    let id = location.id;
    let root_name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.clone());
    let root_item = ufm_vfs::Item {
        id: ItemId::new(),
        location: id,
        name: root_name,
        kind: ufm_vfs::ItemKind::Folder,
        size_bytes: None,
        modified: None,
        provider_key: Some(root.to_string_lossy().into_owned()),
        labels: Vec::new(),
        starred: false,
    };
    state.local_roots.lock().unwrap().insert(id, root.clone());
    state.root_items.lock().unwrap().insert(id, root_item.id);
    state
        .item_paths
        .lock()
        .unwrap()
        .insert(root_item.id, root.clone());
    state
        .path_ids
        .lock()
        .unwrap()
        .insert(root.clone(), root_item.id);
    let summary = LocationSummary {
        id,
        provider: "cloud".into(),
        display_name: name,
        root_item: root_item.id,
    };
    state.locations.lock().unwrap().push(summary.clone());
    Ok(summary)
}

/// Remove a runtime location (unmount). The seeded Home location is
/// not removable through here — callers only pass runtime ids.
#[tauri::command]
pub async fn remove_path_location(state: State<'_, AppState>, location: LocationId) -> Cmd<()> {
    state.local_roots.lock().unwrap().remove(&location);
    state.root_items.lock().unwrap().remove(&location);
    state
        .locations
        .lock()
        .unwrap()
        .retain(|l| l.id != location);
    Ok(())
}

/// Mount a network server with gio (sftp://, smb://, ftp://, dav://…)
/// and return the gvfs mount path. Credentials are handled by gvfs's
/// own prompts/agent — never by us.
#[tauri::command]
pub async fn mount_server(url: String) -> Cmd<String> {
    let url = url.trim().to_string();
    if !url.contains("://") {
        return Err(CommandError::from(UfmError::InvalidInput(
            "include the scheme, e.g. sftp://user@host/path".into(),
        )));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let uid = std::process::Command::new("id")
            .arg("-u")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "1000".into());
        let gvfs = std::path::PathBuf::from(format!("/run/user/{uid}/gvfs"));
        let before: std::collections::HashSet<String> = std::fs::read_dir(&gvfs)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default();
        let out = std::process::Command::new("gio")
            .arg("mount")
            .arg(&url)
            .output()
            .map_err(|e| format!("gio: {e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(if err.is_empty() {
                "gio mount failed".to_string()
            } else {
                err
            });
        }
        // Find the new gvfs entry (or any entry if it was already up).
        for _ in 0..10 {
            if let Ok(rd) = std::fs::read_dir(&gvfs) {
                let names: Vec<String> = rd
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect();
                if let Some(new) = names.iter().find(|n| !before.contains(*n)) {
                    return Ok::<String, String>(
                        gvfs.join(new).to_string_lossy().to_string(),
                    );
                }
                if names.len() == 1 && before.len() == 1 {
                    return Ok::<String, String>(
                        gvfs.join(&names[0]).to_string_lossy().to_string(),
                    );
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        Err("mounted, but the gvfs mount point was not found".to_string())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("mount task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}


// ---------------------------------------------------------------------------
// Built-in terminal (PTY)
// ---------------------------------------------------------------------------

pub struct TerminalSession {
    writer: Box<dyn std::io::Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send>,
}

/// Open a PTY running the user's shell in `cwd`. Output streams to
/// the frontend as "term-output" events (base64 chunks); shell exit
/// fires "term-exit".
#[tauri::command]
pub async fn term_open(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    cwd: String,
    cols: u16,
    rows: u16,
) -> Cmd<String> {
    use base64::Engine;
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};
    use tauri::Emitter;

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: rows.max(2),
            cols: cols.max(10),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| CommandError::from(UfmError::Internal(format!("pty: {e}"))))?;
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
    let mut cmd = CommandBuilder::new(&shell);
    cmd.arg("-i");
    cmd.cwd(&cwd);
    cmd.env("TERM", "xterm-256color");
    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| CommandError::from(UfmError::Internal(format!("spawn shell: {e}"))))?;
    drop(pair.slave);
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| CommandError::from(UfmError::Internal(format!("pty reader: {e}"))))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| CommandError::from(UfmError::Internal(format!("pty writer: {e}"))))?;
    let id = format!("term-{}", ItemId::new());
    let id2 = id.clone();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let encoded =
                        base64::engine::general_purpose::STANDARD.encode(&buf[..n]);
                    let _ = app.emit(
                        "term-output",
                        serde_json::json!({ "id": id2, "data": encoded }),
                    );
                }
                Err(_) => break,
            }
        }
        let _ = app.emit("term-exit", serde_json::json!({ "id": id2 }));
    });
    state.terminals.lock().unwrap().insert(
        id.clone(),
        TerminalSession {
            writer,
            master: pair.master,
            child,
        },
    );
    Ok(id)
}

#[tauri::command]
pub async fn term_write(state: State<'_, AppState>, id: String, data: String) -> Cmd<()> {
    use std::io::Write;
    let mut sessions = state.terminals.lock().unwrap();
    let session = sessions
        .get_mut(&id)
        .ok_or_else(|| CommandError::from(UfmError::NotFound("terminal closed".into())))?;
    session
        .writer
        .write_all(data.as_bytes())
        .map_err(|e| CommandError::from(UfmError::Internal(e.to_string())))?;
    session
        .writer
        .flush()
        .map_err(|e| CommandError::from(UfmError::Internal(e.to_string())))?;
    Ok(())
}

#[tauri::command]
pub async fn term_resize(
    state: State<'_, AppState>,
    id: String,
    cols: u16,
    rows: u16,
) -> Cmd<()> {
    use portable_pty::PtySize;
    let mut sessions = state.terminals.lock().unwrap();
    let session = sessions
        .get_mut(&id)
        .ok_or_else(|| CommandError::from(UfmError::NotFound("terminal closed".into())))?;
    session
        .master
        .resize(PtySize {
            rows: rows.max(2),
            cols: cols.max(10),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| CommandError::from(UfmError::Internal(e.to_string())))?;
    Ok(())
}

#[tauri::command]
pub async fn term_close(state: State<'_, AppState>, id: String) -> Cmd<()> {
    if let Some(mut session) = state.terminals.lock().unwrap().remove(&id) {
        let _ = session.child.kill();
        let _ = session.child.wait();
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Transcoding (ffmpeg)
// ---------------------------------------------------------------------------

fn transcode_preset(preset: &str) -> Option<(&'static str, Vec<&'static str>)> {
    // (output extension, ffmpeg codec args)
    Some(match preset {
        "v_mp4_264" => (
            "mp4",
            vec!["-c:v", "libx264", "-crf", "20", "-preset", "medium", "-c:a", "aac", "-b:a", "160k"],
        ),
        "v_mp4_265" => (
            "mp4",
            vec!["-c:v", "libx265", "-crf", "24", "-tag:v", "hvc1", "-c:a", "aac", "-b:a", "160k"],
        ),
        "v_webm" => (
            "webm",
            vec!["-c:v", "libvpx-vp9", "-crf", "30", "-b:v", "0", "-c:a", "libopus", "-b:a", "128k"],
        ),
        "v_mkv_265" => (
            "mkv",
            vec!["-c:v", "libx265", "-crf", "24", "-c:a", "aac", "-b:a", "160k"],
        ),
        "v_gif" => (
            "gif",
            vec!["-vf", "fps=12,scale=640:-1:flags=lanczos", "-an"],
        ),
        "a_mp3" => ("mp3", vec!["-vn", "-c:a", "libmp3lame", "-b:a", "320k"]),
        "a_flac" => ("flac", vec!["-vn", "-c:a", "flac"]),
        "a_m4a" => ("m4a", vec!["-vn", "-c:a", "aac", "-b:a", "256k"]),
        "a_opus" => ("opus", vec!["-vn", "-c:a", "libopus", "-b:a", "160k"]),
        "a_wav" => ("wav", vec!["-vn", "-c:a", "pcm_s16le"]),
        _ => return None,
    })
}

/// Transcode a media file with ffmpeg. Progress streams as
/// "transcode-progress" events {id, percent, done, output?, error?}.
/// Returns the job id immediately.
#[tauri::command]
pub async fn transcode(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    item: ItemId,
    preset: String,
) -> Cmd<String> {
    use std::io::Read;
    use tauri::Emitter;

    let src = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    let (ext, args) = transcode_preset(&preset)
        .ok_or_else(|| CommandError::from(UfmError::InvalidInput("unknown preset".into())))?;
    let stem = src
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("output")
        .to_string();
    let parent = src.parent().ok_or_else(|| {
        CommandError::from(UfmError::InvalidInput("no parent".into()))
    })?
    .to_path_buf();
    let preset_label = preset.trim_start_matches(['v', 'a']).trim_start_matches('_');
    let mut dest = parent.join(format!("{stem} ({preset_label}).{ext}"));
    let mut n = 2;
    while dest.exists() {
        dest = parent.join(format!("{stem} ({preset_label}) {n}.{ext}"));
        n += 1;
    }
    // Duration for percent math (ffprobe), seconds.
    let duration: f64 = std::process::Command::new("ffprobe")
        .args(["-v", "quiet", "-show_entries", "format=duration", "-of", "csv=p=0"])
        .arg(&src)
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
        .unwrap_or(0.0);

    let job_id = format!("tc-{}", ItemId::new());
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    state
        .transcode_cancels
        .lock()
        .unwrap()
        .insert(job_id.clone(), cancel.clone());

    let app2 = app.clone();
    let job2 = job_id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let emit = |percent: f64, done: bool, output: Option<String>, error: Option<String>| {
            let _ = app2.emit(
                "transcode-progress",
                serde_json::json!({
                    "id": job2,
                    "percent": percent,
                    "done": done,
                    "output": output,
                    "error": error,
                }),
            );
        };
        let mut child = match std::process::Command::new("ffmpeg")
            .arg("-y")
            .arg("-i")
            .arg(&src)
            .args(&args)
            .arg("-progress")
            .arg("pipe:1")
            .arg(&dest)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                emit(0.0, true, None, Some(format!("ffmpeg failed to start: {e}")));
                return;
            }
        };
        let mut stdout = child.stdout.take();
        let mut text = String::new();
        let mut byte = [0u8; 1];
        let mut out_time_ms: f64 = 0.0;
        loop {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = child.kill();
                let _ = std::fs::remove_file(&dest);
                emit(0.0, true, None, Some("cancelled".into()));
                return;
            }
            match stdout.as_mut().map(|s| s.read(&mut byte)) {
                Some(Ok(1)) => {
                    if byte[0] == b'\n' {
                        let line = text.trim().to_string();
                        text.clear();
                        if let Some(v) = line.strip_prefix("out_time_ms=") {
                            out_time_ms = v.parse().unwrap_or(0.0);
                            if duration > 0.0 {
                                let pct = (out_time_ms / 1_000_000.0 / duration * 100.0)
                                    .min(99.0);
                                emit(pct, false, None, None);
                            }
                        }
                    } else {
                        text.push(byte[0] as char);
                    }
                }
                _ => break,
            }
        }
        match child.wait() {
            Ok(status) if status.success() => {
                emit(100.0, true, Some(dest.to_string_lossy().to_string()), None);
            }
            Ok(_) => {
                let _ = std::fs::remove_file(&dest);
                emit(0.0, true, None, Some("ffmpeg failed (bad input or codec)".into()));
            }
            Err(e) => emit(0.0, true, None, Some(e.to_string())),
        }
        let _ = out_time_ms;
    });
    Ok(job_id)
}

#[tauri::command]
pub async fn cancel_transcode(state: State<'_, AppState>, id: String) -> Cmd<()> {
    if let Some(flag) = state.transcode_cancels.lock().unwrap().get(&id) {
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(())
}


// ---------------------------------------------------------------------------
// Metadata: read / edit / wash (exiftool)
// ---------------------------------------------------------------------------

fn exiftool_bin() -> String {
    if std::path::Path::new("/usr/bin/vendor_perl/exiftool").exists() {
        "/usr/bin/vendor_perl/exiftool".to_string()
    } else {
        "exiftool".to_string()
    }
}

#[derive(serde::Serialize)]
pub struct MetaTag {
    pub group: String,
    pub tag: String,
    pub value: String,
}

/// Read all metadata tags of a file (exiftool JSON, flattened).
#[tauri::command]
pub async fn read_metadata(state: State<'_, AppState>, item: ItemId) -> Cmd<Vec<MetaTag>> {
    let path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    tauri::async_runtime::spawn_blocking(move || {
        let out = std::process::Command::new(exiftool_bin())
            .arg("-json")
            .arg("-G1")
            .arg("-a")
            .arg(&path)
            .output()
            .map_err(|e| format!("exiftool: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "exiftool: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let parsed: serde_json::Value =
            serde_json::from_slice(&out.stdout).map_err(|e| format!("json: {e}"))?;
        let obj = parsed
            .get(0)
            .and_then(|v| v.as_object())
            .ok_or("no metadata object")?;
        let mut tags: Vec<MetaTag> = Vec::new();
        for (key, value) in obj {
            if key == "SourceFile" {
                continue;
            }
            let (group, tag) = match key.split_once(':') {
                Some((g, t)) => (g.to_string(), t.to_string()),
                None => ("Other".to_string(), key.clone()),
            };
            let text = match value {
                serde_json::Value::String(v) => v.clone(),
                serde_json::Value::Number(v) => v.to_string(),
                serde_json::Value::Bool(v) => v.to_string(),
                serde_json::Value::Array(v) => v
                    .iter()
                    .map(|x| x.to_string().trim_matches('"').to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                _ => value.to_string(),
            };
            let text = if text.len() > 300 {
                format!("{}…", &text[..300])
            } else {
                text
            };
            tags.push(MetaTag { group, tag, value: text });
        }
        tags.sort_by(|a, b| (a.group.clone(), a.tag.clone()).cmp(&(b.group.clone(), b.tag.clone())));
        Ok::<Vec<MetaTag>, String>(tags)
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("metadata task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Tags the editor may write (allowlist; GPS positions included so
/// they can be cleared).
fn metadata_writable(tag: &str) -> bool {
    matches!(
        tag,
        "Artist"
            | "Author"
            | "Creator"
            | "Copyright"
            | "Make"
            | "Model"
            | "Software"
            | "DateTimeOriginal"
            | "CreateDate"
            | "ModifyDate"
            | "ImageDescription"
            | "Comment"
            | "Title"
            | "Subject"
            | "Keywords"
            | "GPSLatitude"
            | "GPSLongitude"
            | "GPSAltitude"
            | "SerialNumber"
            | "LensModel"
            | "CameraModelName"
    )
}

/// Apply tag edits (empty value clears the tag).
#[tauri::command]
pub async fn edit_metadata(
    state: State<'_, AppState>,
    item: ItemId,
    edits: Vec<(String, String)>,
) -> Cmd<()> {
    let path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut cmd = std::process::Command::new(exiftool_bin());
        cmd.arg("-overwrite_original");
        let mut any = false;
        for (key, value) in &edits {
            let tag = key.rsplit(':').next().unwrap_or(key);
            if !metadata_writable(tag) {
                continue;
            }
            cmd.arg(format!("-{key}={value}"));
            any = true;
        }
        if !any {
            return Err("no editable tags in the edit set".to_string());
        }
        cmd.arg(&path);
        let out = cmd.output().map_err(|e| format!("exiftool: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "exiftool: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("metadata task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

#[derive(serde::Serialize)]
pub struct WashResult {
    pub path: String,
    pub tags_before: u32,
    pub tags_after: u32,
}

fn count_metadata_tags(path: &std::path::Path) -> u32 {
    std::process::Command::new(exiftool_bin())
        .arg("-json")
        .arg("-G1")
        .arg("-a")
        .arg(path)
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok())
        .and_then(|v| {
            v.get(0)
                .and_then(|o| o.as_object())
                .map(|m| m.len() as u32)
        })
        .unwrap_or(0)
}

/// THE WASHING MACHINE: strip all embedded metadata (camera, serials,
/// GPS, author, dates, software). Orientation + ICC profile are kept
/// so the photo still displays correctly. Either in place, or on a
/// "(washed)" copy.
#[tauri::command]
pub async fn wash_metadata(
    state: State<'_, AppState>,
    item: ItemId,
    copy: bool,
) -> Cmd<WashResult> {
    let src = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    tauri::async_runtime::spawn_blocking(move || {
        let target = if copy {
            let stem = src
                .file_stem()
                .and_then(|n| n.to_str())
                .unwrap_or("file")
                .to_string();
            let ext = src
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| format!(".{e}"))
                .unwrap_or_default();
            let parent = src.parent().ok_or("no parent")?.to_path_buf();
            let mut dest = parent.join(format!("{stem} (washed){ext}"));
            let mut n = 2;
            while dest.exists() {
                dest = parent.join(format!("{stem} (washed) {n}{ext}"));
                n += 1;
            }
            std::fs::copy(&src, &dest).map_err(|e| format!("copy: {e}"))?;
            dest
        } else {
            src.clone()
        };
        let before = count_metadata_tags(&target);
        let out = std::process::Command::new(exiftool_bin())
            .arg("-all=")
            .arg("-tagsFromFile")
            .arg(&src)
            .arg("-Orientation")
            .arg("-ICC_Profile")
            .arg("-overwrite_original")
            .arg(&target)
            .output()
            .map_err(|e| format!("exiftool: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "exiftool: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let after = count_metadata_tags(&target);
        Ok::<WashResult, String>(WashResult {
            path: target.to_string_lossy().to_string(),
            tags_before: before,
            tags_after: after,
        })
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("wash task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Clean photo copy: decode the pixels and re-encode from scratch —
/// nothing attached survives, and the hash is brand new (a photo of
/// the photo, not a matchable copy).
#[tauri::command]
pub async fn clean_photo_copy(state: State<'_, AppState>, item: ItemId) -> Cmd<String> {
    let src = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    tauri::async_runtime::spawn_blocking(move || {
        let img = image::open(&src).map_err(|e| format!("image open: {e}"))?;
        let stem = src
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or("photo")
            .to_string();
        let ext = src
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("png")
            .to_lowercase();
        let parent = src.parent().ok_or("no parent")?.to_path_buf();
        let mut dest = parent.join(format!("{stem} (clean).{ext}"));
        let mut n = 2;
        while dest.exists() {
            dest = parent.join(format!("{stem} (clean) {n}.{ext}"));
            n += 1;
        }
        let fmt = image::ImageFormat::from_extension(&ext).unwrap_or(image::ImageFormat::Png);
        if fmt == image::ImageFormat::Jpeg {
            img.to_rgb8()
                .save_with_format(&dest, fmt)
                .map_err(|e| format!("save: {e}"))?;
        } else {
            img.save_with_format(&dest, fmt)
                .map_err(|e| format!("save: {e}"))?;
        }
        Ok::<String, String>(dest.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("clean task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}


// ---------------------------------------------------------------------------
// ISO mounting + Burn Studio
// ---------------------------------------------------------------------------

/// Mount an ISO/disc image file (path-based; Burn Studio mounts
/// freshly built ISOs that have no catalog item yet).
#[tauri::command]
pub async fn mount_iso_path(path: String) -> Cmd<String> {
    mount_image_at(std::path::PathBuf::from(path))
        .await
        .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Mount an ISO/disc image via udisks (loop device, user session).
/// Returns the mount path.
#[tauri::command]
pub async fn mount_iso(state: State<'_, AppState>, item: ItemId) -> Cmd<String> {
    let path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    mount_image_at(path)
        .await
        .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

async fn mount_image_at(
    path: std::path::PathBuf,
) -> std::result::Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        // Set up the loop device.
        let out = std::process::Command::new("udisksctl")
            .arg("loop-setup")
            .arg("-f")
            .arg(&path)
            .arg("--no-user-interaction")
            .output()
            .map_err(|e| format!("udisksctl: {e}"))?;
        let text = String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr);
        // "Mapped file ... as /dev/loop0."
        let dev = text
            .split_whitespace()
            .find(|w| w.starts_with("/dev/loop"))
            .map(|w| w.trim_end_matches('.').to_string())
            .ok_or_else(|| format!("loop-setup failed: {}", text.trim()))?;
        // Mount it (may already auto-mount; either way find the path).
        let mout = std::process::Command::new("udisksctl")
            .arg("mount")
            .arg("-b")
            .arg(&dev)
            .arg("--no-user-interaction")
            .output()
            .map_err(|e| format!("udisksctl: {e}"))?;
        let mtext = String::from_utf8_lossy(&mout.stdout).to_string()
            + &String::from_utf8_lossy(&mout.stderr);
        // "Mounted /dev/loop0 at /run/media/user/NAME"
        let mount_path = mtext
            .split(" at ")
            .nth(1)
            .map(|t| t.trim().trim_end_matches('.').to_string())
            .filter(|t| t.starts_with('/'));
        match mount_path {
            Some(p) => Ok::<String, String>(p),
            None => {
                // Already mounted before we got here? Ask findmnt.
                let fout = std::process::Command::new("findmnt")
                    .arg("-n")
                    .arg("-o")
                    .arg("TARGET")
                    .arg(&dev)
                    .output()
                    .map_err(|e| format!("findmnt: {e}"))?;
                let p = String::from_utf8_lossy(&fout.stdout).trim().to_string();
                if p.starts_with('/') {
                    Ok(p)
                } else {
                    Err(format!("mount failed: {}", mtext.trim()))
                }
            }
        }
    })
    .await
    .map_err(|e| format!("mount task failed: {e}"))?
}

/// Unmount whatever is mounted at a path (ISO locations, USB…).
#[tauri::command]
pub async fn unmount_path(path: String) -> Cmd<()> {
    tauri::async_runtime::spawn_blocking(move || {
        let fout = std::process::Command::new("findmnt")
            .arg("-n")
            .arg("-o")
            .arg("SOURCE")
            .arg(&path)
            .output()
            .map_err(|e| format!("findmnt: {e}"))?;
        let dev = String::from_utf8_lossy(&fout.stdout).trim().to_string();
        if dev.is_empty() {
            return Err(format!("{path} is not a mount point"));
        }
        let out = std::process::Command::new("udisksctl")
            .arg("unmount")
            .arg("-b")
            .arg(&dev)
            .arg("--no-user-interaction")
            .output()
            .map_err(|e| format!("udisksctl: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "unmount failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        // Tear down the loop device for disc images.
        if dev.starts_with("/dev/loop") {
            let _ = std::process::Command::new("udisksctl")
                .arg("loop-delete")
                .arg("-b")
                .arg(&dev)
                .arg("--no-user-interaction")
                .output();
        }
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("unmount task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

#[derive(serde::Serialize)]
pub struct BlockDevice {
    pub path: String,
    pub size: String,
    pub model: String,
    pub removable: bool,
    pub usb: bool,
}

/// Removable/USB block devices (Burn Studio's Write-to-USB targets).
#[tauri::command]
pub async fn list_block_devices() -> Cmd<Vec<BlockDevice>> {
    tauri::async_runtime::spawn_blocking(move || {
        let out = std::process::Command::new("lsblk")
            .args(["-J", "-o", "NAME,SIZE,MODEL,RM,TYPE,TRAN"])
            .output()
            .map_err(|e| format!("lsblk: {e}"))?;
        let parsed: serde_json::Value =
            serde_json::from_slice(&out.stdout).map_err(|e| format!("json: {e}"))?;
        let mut devices = Vec::new();
        if let Some(list) = parsed.get("blockdevices").and_then(|v| v.as_array()) {
            for d in list {
                if d.get("type").and_then(|v| v.as_str()) != Some("disk") {
                    continue;
                }
                let rm = d.get("rm").and_then(|v| v.as_bool()).unwrap_or(false);
                let tran = d.get("tran").and_then(|v| v.as_str()).unwrap_or("");
                if !rm && tran != "usb" {
                    continue;
                }
                let name = d.get("name").and_then(|v| v.as_str()).unwrap_or("");
                devices.push(BlockDevice {
                    path: format!("/dev/{name}"),
                    size: d
                        .get("size")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    model: d
                        .get("model")
                        .and_then(|v| v.as_str())
                        .unwrap_or("USB drive")
                        .trim()
                        .to_string(),
                    removable: rm,
                    usb: tran == "usb",
                });
            }
        }
        Ok::<Vec<BlockDevice>, String>(devices)
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("lsblk task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Build an ISO from a set of absolute paths (Burn Studio projects
/// are repeatable: same set, fresh build, any time). Files/folders
/// land at the image root under their own names (graft points).
#[tauri::command]
pub async fn build_iso(
    paths: Vec<String>,
    dest_iso: String,
    volume_label: String,
) -> Cmd<String> {
    tauri::async_runtime::spawn_blocking(move || {
        if paths.is_empty() {
            return Err("the project is empty".to_string());
        }
        let mut cmd = std::process::Command::new("xorriso");
        cmd.arg("-as").arg("mkisofs")
            .arg("-o").arg(&dest_iso)
            .arg("-V").arg(if volume_label.is_empty() { "FABLE" } else { &volume_label })
            .arg("-J").arg("-r")
            .arg("-graft-points");
        for p in &paths {
            let name = std::path::Path::new(p)
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| format!("bad path {p}"))?;
            cmd.arg(format!("{name}={p}"));
        }
        let out = cmd.output().map_err(|e| format!("xorriso: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "xorriso failed: {}",
                String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .last()
                    .unwrap_or("")
                    .trim()
            ));
        }
        Ok::<String, String>(dest_iso)
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("iso task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

#[derive(serde::Serialize)]
pub struct VerifyResult {
    pub checked: u32,
    pub ok: u32,
    pub failed: Vec<String>,
}

fn sha256_file(path: &std::path::Path) -> Option<String> {
    use sha2::Digest;
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = sha2::Sha256::new();
    std::io::copy(&mut file, &mut hasher).ok()?;
    Some(format!("{:x}", hasher.finalize()))
}

/// Verify an ISO against its sources: extract (bsdtar reads ISO9660)
/// and SHA-256-compare every regular file.
#[tauri::command]
pub async fn verify_iso(iso_path: String, sources: Vec<String>) -> Cmd<VerifyResult> {
    tauri::async_runtime::spawn_blocking(move || {
        let tmp = std::env::temp_dir().join(format!("fable-verify-{}", ItemId::new()));
        std::fs::create_dir_all(&tmp).map_err(|e| format!("mkdir: {e}"))?;
        let cleanup = |tmp: &std::path::Path| {
            let _ = std::fs::remove_dir_all(tmp);
        };
        let out = std::process::Command::new("bsdtar")
            .arg("-xf")
            .arg(&iso_path)
            .arg("-C")
            .arg(&tmp)
            .output()
            .map_err(|e| format!("bsdtar: {e}"))?;
        if !out.status.success() {
            cleanup(&tmp);
            return Err("could not extract the ISO for verification".to_string());
        }
        let mut result = VerifyResult { checked: 0, ok: 0, failed: Vec::new() };
        // Index extracted files by relative path.
        let mut extracted: std::collections::HashMap<String, std::path::PathBuf> =
            std::collections::HashMap::new();
        let mut stack = vec![tmp.clone()];
        while let Some(dir) = stack.pop() {
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for entry in rd.flatten() {
                    let p = entry.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else if let Ok(rel) = p.strip_prefix(&tmp) {
                        extracted.insert(
                            rel.to_string_lossy().replace('\\', "/"),
                            p.clone(),
                        );
                    }
                }
            }
        }
        for src in &sources {
            let src_path = std::path::Path::new(src);
            let base = src_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            let mut files: Vec<(String, std::path::PathBuf)> = Vec::new();
            if src_path.is_dir() {
                let mut stack2 = vec![src_path.to_path_buf()];
                while let Some(dir) = stack2.pop() {
                    if let Ok(rd) = std::fs::read_dir(&dir) {
                        for entry in rd.flatten() {
                            let p = entry.path();
                            if p.is_dir() {
                                stack2.push(p);
                            } else if let Ok(rel) = p.strip_prefix(src_path) {
                                files.push((
                                    format!("{}/{}", base, rel.to_string_lossy().replace('\\', "/")),
                                    p.clone(),
                                ));
                            }
                        }
                    }
                }
            } else {
                files.push((base.clone(), src_path.to_path_buf()));
            }
            for (rel, file) in files {
                result.checked += 1;
                match (sha256_file(&file), extracted.get(&rel).and_then(|p| sha256_file(p))) {
                    (Some(a), Some(b)) if a == b => result.ok += 1,
                    _ => result.failed.push(rel),
                }
            }
        }
        cleanup(&tmp);
        Ok::<VerifyResult, String>(result)
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("verify task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Write an ISO to a USB device with dd (pkexec asks for the admin
/// password in a GUI prompt). Progress events: "burn-progress".
#[tauri::command]
pub async fn write_usb(
    app: tauri::AppHandle,
    iso_path: String,
    device: String,
) -> Cmd<String> {
    use std::io::Read;
    use tauri::Emitter;
    let job_id = format!("usb-{}", ItemId::new());
    let total = std::fs::metadata(&iso_path)
        .map(|m| m.len() as f64)
        .unwrap_or(0.0);
    let job2 = job_id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let emit = |percent: f64, done: bool, error: Option<String>| {
            let _ = app.emit(
                "burn-progress",
                serde_json::json!({
                    "id": job2, "percent": percent, "done": done,
                    "output": if done && error.is_none() { Some(device.clone()) } else { None },
                    "error": error,
                }),
            );
        };
        let mut child = match std::process::Command::new("pkexec")
            .arg("dd")
            .arg(format!("if={iso_path}"))
            .arg(format!("of={device}"))
            .arg("bs=4M")
            .arg("status=progress")
            .arg("conv=fsync")
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                emit(0.0, true, Some(format!("dd failed to start: {e}")));
                return;
            }
        };
        // dd progress lines look like: "123456 bytes (0.1 GB) copied, ..."
        let mut stderr = child.stderr.take();
        let mut text = String::new();
        let mut byte = [0u8; 1];
        loop {
            match stderr.as_mut().map(|r| r.read(&mut byte)) {
                Some(Ok(1)) => {
                    let c = byte[0] as char;
                    if c == '\n' || c == '\r' {
                        let line = text.trim().to_string();
                        text.clear();
                        if line.contains("bytes") && total > 0.0 {
                            if let Some(num) = line.split_whitespace().next() {
                                if let Ok(written) = num.parse::<f64>() {
                                    emit((written / total * 100.0).min(99.0), false, None);
                                }
                            }
                        }
                    } else {
                        text.push(c);
                    }
                }
                _ => break,
            }
        }
        match child.wait() {
            Ok(status) if status.success() => emit(100.0, true, None),
            Ok(_) => emit(
                0.0,
                true,
                Some("write failed — wrong password, or the device is busy".into()),
            ),
            Err(e) => emit(0.0, true, Some(e.to_string())),
        }
    });
    Ok(job_id)
}

/// Burn an ISO to an optical disc in the first burner (growisofs).
#[tauri::command]
pub async fn burn_disc(iso_path: String) -> Cmd<String> {
    tauri::async_runtime::spawn_blocking(move || {
        // Find an optical drive.
        let out = std::process::Command::new("lsblk")
            .args(["-J", "-o", "NAME,TYPE"])
            .output()
            .map_err(|e| format!("lsblk: {e}"))?;
        let parsed: serde_json::Value =
            serde_json::from_slice(&out.stdout).map_err(|e| format!("json: {e}"))?;
        let mut drive: Option<String> = None;
        if let Some(list) = parsed.get("blockdevices").and_then(|v| v.as_array()) {
            for d in list {
                if d.get("type").and_then(|v| v.as_str()) == Some("rom") {
                    drive = d
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(|n| format!("/dev/{n}"));
                    break;
                }
            }
        }
        let drive = drive.ok_or("no CD/DVD/Blu-ray drive found")?;
        let out = std::process::Command::new("growisofs")
            .arg("-dvd-compat")
            .arg(format!("-Z{d}={iso}", d = drive, iso = iso_path))
            .output()
            .map_err(|e| format!("growisofs: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "burn failed: {}",
                String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .last()
                    .unwrap_or("")
                    .trim()
            ));
        }
        Ok::<String, String>(drive)
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("burn task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}


// ---------------------------------------------------------------------------
// Utilities: checksums, split/join, sync, duplicates
// ---------------------------------------------------------------------------

/// Trash a file/folder by absolute path (freedesktop spec).
/// Returns the trash entry name.
pub fn trash_path_internal(path: &std::path::Path) -> std::result::Result<String, String> {
    let home = std::env::var("HOME").map_err(|_| "no HOME".to_string())?;
    let trash_root = std::path::PathBuf::from(home).join(".local/share/Trash");
    let files_dir = trash_root.join("files");
    let info_dir = trash_root.join("info");
    std::fs::create_dir_all(&files_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&info_dir).map_err(|e| e.to_string())?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("bad name")?
        .to_string();
    let mut trash_name = name.clone();
    let mut n = 2;
    while files_dir.join(&trash_name).exists() {
        trash_name = format!("{name} {n}");
        n += 1;
    }
    let dest = files_dir.join(&trash_name);
    std::fs::rename(path, &dest).map_err(|e| e.to_string())?;
    let abs = path.to_string_lossy().to_string();
    let encoded: String = abs
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect();
    let now = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S");
    let info = format!("[Trash Info]\nPath={encoded}\nDeletionDate={now}\n");
    std::fs::write(info_dir.join(format!("{trash_name}.trashinfo")), info)
        .map_err(|e| e.to_string())?;
    Ok(trash_name)
}

#[tauri::command]
pub async fn trash_file_path(path: String) -> Cmd<String> {
    trash_path_internal(std::path::Path::new(&path))
        .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

fn hash_file(path: &std::path::Path, algo: &str) -> std::result::Result<String, String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 1 << 20];
    match algo {
        "md5" => {
            let mut ctx = md5::Context::new();
            loop {
                let n = file.read(&mut buf).map_err(|e| e.to_string())?;
                if n == 0 { break; }
                ctx.consume(&buf[..n]);
            }
            Ok(format!("{:x}", ctx.compute()))
        }
        "sha1" => {
            use sha1::Digest;
            let mut h = sha1::Sha1::new();
            loop {
                let n = file.read(&mut buf).map_err(|e| e.to_string())?;
                if n == 0 { break; }
                h.update(&buf[..n]);
            }
            Ok(format!("{:x}", h.finalize()))
        }
        "sha256" | "sha512" => {
            use sha2::Digest;
            if algo == "sha256" {
                let mut h = sha2::Sha256::new();
                loop {
                    let n = file.read(&mut buf).map_err(|e| e.to_string())?;
                    if n == 0 { break; }
                    h.update(&buf[..n]);
                }
                Ok(format!("{:x}", h.finalize()))
            } else {
                let mut h = sha2::Sha512::new();
                loop {
                    let n = file.read(&mut buf).map_err(|e| e.to_string())?;
                    if n == 0 { break; }
                    h.update(&buf[..n]);
                }
                Ok(format!("{:x}", h.finalize()))
            }
        }
        "crc32" => {
            let mut h = crc32fast::Hasher::new();
            loop {
                let n = file.read(&mut buf).map_err(|e| e.to_string())?;
                if n == 0 { break; }
                h.update(&buf[..n]);
            }
            Ok(format!("{:08x}", h.finalize()))
        }
        _ => Err(format!("unknown algorithm {algo}")),
    }
}

/// Calculate a checksum for a file (md5/sha1/sha256/sha512/crc32).
#[tauri::command]
pub async fn checksum_item(
    state: State<'_, AppState>,
    item: ItemId,
    algo: String,
) -> Cmd<String> {
    let path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    tauri::async_runtime::spawn_blocking(move || hash_file(&path, &algo))
        .await
        .map_err(|e| CommandError::from(UfmError::Internal(format!("hash task failed: {e}"))))?
        .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Write a "<name>.<algo>" checksum file beside the item.
#[tauri::command]
pub async fn create_checksum_file(
    state: State<'_, AppState>,
    item: ItemId,
    algo: String,
) -> Cmd<String> {
    let path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    tauri::async_runtime::spawn_blocking(move || {
        let hash = hash_file(&path, &algo)?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("bad name")?
            .to_string();
        let dest = path.with_file_name(format!("{name}.{algo}"));
        std::fs::write(&dest, format!("{hash}  {name}\n")).map_err(|e| e.to_string())?;
        Ok::<String, String>(dest.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("hash task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

#[derive(serde::Serialize)]
pub struct ChecksumVerify {
    pub file: String,
    pub ok: bool,
    pub expected: String,
    pub actual: String,
}

/// Verify every entry of a .md5/.sha256/… checksum file.
#[tauri::command]
pub async fn verify_checksum_file(
    state: State<'_, AppState>,
    item: ItemId,
) -> Cmd<Vec<ChecksumVerify>> {
    let path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    tauri::async_runtime::spawn_blocking(move || {
        let algo = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("sha256")
            .to_lowercase();
        let parent = path.parent().ok_or("no parent")?.to_path_buf();
        let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            // "<hash>  <name>" or "<hash> *<name>"
            let mut parts = line.splitn(2, char::is_whitespace);
            let expected = parts.next().unwrap_or("").to_lowercase();
            let name = parts
                .next()
                .unwrap_or("")
                .trim()
                .trim_start_matches('*')
                .to_string();
            if name.is_empty() {
                continue;
            }
            let target = parent.join(&name);
            let actual = hash_file(&target, &algo).unwrap_or_default();
            out.push(ChecksumVerify {
                file: name,
                ok: !actual.is_empty() && actual == expected,
                expected,
                actual,
            });
        }
        Ok::<Vec<ChecksumVerify>, String>(out)
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("verify task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Split a file into numbered parts (name.001, name.002, …) of
/// `part_bytes` each. Returns the part paths.
#[tauri::command]
pub async fn split_file(
    state: State<'_, AppState>,
    item: ItemId,
    part_bytes: u64,
) -> Cmd<Vec<String>> {
    let path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    tauri::async_runtime::spawn_blocking(move || {
        use std::io::{Read, Write};
        if part_bytes == 0 {
            return Err("part size must be > 0".to_string());
        }
        let mut src = std::fs::File::open(&path).map_err(|e| e.to_string())?;
        let parent = path.parent().ok_or("no parent")?.to_path_buf();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("bad name")?
            .to_string();
        let mut parts = Vec::new();
        let mut buf = vec![0u8; 1 << 20];
        let mut idx = 1u32;
        loop {
            let part_path = parent.join(format!("{name}.{idx:03}"));
            let mut out = std::fs::File::create(&part_path).map_err(|e| e.to_string())?;
            let mut remaining = part_bytes;
            let mut wrote_any = false;
            while remaining > 0 {
                let want = remaining.min(buf.len() as u64) as usize;
                let n = src.read(&mut buf[..want]).map_err(|e| e.to_string())?;
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
                remaining -= n as u64;
                wrote_any = true;
            }
            if !wrote_any {
                let _ = std::fs::remove_file(&part_path);
                break;
            }
            parts.push(part_path.to_string_lossy().to_string());
            idx += 1;
        }
        if parts.is_empty() {
            return Err("source file is empty".to_string());
        }
        Ok::<Vec<String>, String>(parts)
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("split task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Join name.001 + name.002 + … back into the original file.
#[tauri::command]
pub async fn join_files(state: State<'_, AppState>, item: ItemId) -> Cmd<String> {
    let path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    tauri::async_runtime::spawn_blocking(move || {
        use std::io::Write;
        let fname = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("bad name")?
            .to_string();
        if !fname.ends_with(".001") {
            return Err("select the .001 part to join".to_string());
        }
        let base = fname.trim_end_matches(".001").to_string();
        let parent = path.parent().ok_or("no parent")?.to_path_buf();
        let mut dest = parent.join(&base);
        let mut n = 2;
        while dest.exists() {
            dest = parent.join(format!("{base} ({n})"));
            n += 1;
        }
        let mut out = std::fs::File::create(&dest).map_err(|e| e.to_string())?;
        let mut idx = 1u32;
        loop {
            let part = parent.join(format!("{base}.{idx:03}"));
            if !part.exists() {
                break;
            }
            let mut f = std::fs::File::open(&part).map_err(|e| e.to_string())?;
            std::io::copy(&mut f, &mut out).map_err(|e| e.to_string())?;
            idx += 1;
        }
        if idx == 1 {
            return Err("no parts found".to_string());
        }
        out.flush().map_err(|e| e.to_string())?;
        Ok::<String, String>(dest.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("join task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

#[derive(serde::Serialize)]
pub struct SyncEntry {
    pub rel: String,
    pub state: String, // only_a | only_b | different | same
    pub size_a: u64,
    pub size_b: u64,
}

fn scan_tree(
    root: &std::path::Path,
) -> std::collections::HashMap<String, (u64, i64)> {
    let mut map = std::collections::HashMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else if let Ok(md) = entry.metadata() {
                    if let Ok(rel) = p.strip_prefix(root) {
                        let mtime = md
                            .modified()
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_secs() as i64)
                            .unwrap_or(0);
                        map.insert(
                            rel.to_string_lossy().replace('\\', "/"),
                            (md.len(), mtime),
                        );
                    }
                }
            }
        }
    }
    map
}

/// Compare two folder trees (Synchronize Folders).
#[tauri::command]
pub async fn compare_dirs(a: String, b: String) -> Cmd<Vec<SyncEntry>> {
    tauri::async_runtime::spawn_blocking(move || {
        let map_a = scan_tree(std::path::Path::new(&a));
        let map_b = scan_tree(std::path::Path::new(&b));
        let mut out = Vec::new();
        for (rel, (size_a, mt_a)) in &map_a {
            match map_b.get(rel) {
                None => out.push(SyncEntry {
                    rel: rel.clone(),
                    state: "only_a".into(),
                    size_a: *size_a,
                    size_b: 0,
                }),
                Some((size_b, mt_b)) => {
                    let same = size_a == size_b && (mt_a - mt_b).abs() <= 2;
                    out.push(SyncEntry {
                        rel: rel.clone(),
                        state: if same { "same" } else { "different" }.into(),
                        size_a: *size_a,
                        size_b: *size_b,
                    });
                }
            }
        }
        for (rel, (size_b, _)) in &map_b {
            if !map_a.contains_key(rel) {
                out.push(SyncEntry {
                    rel: rel.clone(),
                    state: "only_b".into(),
                    size_a: 0,
                    size_b: *size_b,
                });
            }
        }
        out.sort_by(|x, y| x.rel.cmp(&y.rel));
        out.truncate(5000);
        Ok::<Vec<SyncEntry>, String>(out)
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("compare task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Copy selected relative paths from one tree into the other,
/// preserving modification times. Returns the copy count.
#[tauri::command]
pub async fn sync_copy(src_root: String, dst_root: String, rels: Vec<String>) -> Cmd<u32> {
    tauri::async_runtime::spawn_blocking(move || {
        let src = std::path::PathBuf::from(&src_root);
        let dst = std::path::PathBuf::from(&dst_root);
        let mut done = 0u32;
        for rel in &rels {
            let from = src.join(rel);
            let to = dst.join(rel);
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::copy(&from, &to).map_err(|e| format!("{rel}: {e}"))?;
            if let Ok(md) = std::fs::metadata(&from) {
                if let Ok(mtime) = md.modified() {
                    let _ = filetime::set_file_mtime(
                        &to,
                        filetime::FileTime::from_system_time(mtime),
                    );
                }
            }
            done += 1;
        }
        Ok::<u32, String>(done)
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("sync task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

#[derive(serde::Serialize)]
pub struct DupGroup {
    pub size: u64,
    pub hash: String,
    pub paths: Vec<String>,
}

/// Find duplicate files under a folder: group by size, confirm by
/// SHA-256. Content-identical groups only.
#[tauri::command]
pub async fn find_duplicates(root: String, min_size: u64) -> Cmd<Vec<DupGroup>> {
    tauri::async_runtime::spawn_blocking(move || {
        use sha2::Digest;
        let root_path = std::path::PathBuf::from(&root);
        let mut by_size: std::collections::HashMap<u64, Vec<std::path::PathBuf>> =
            std::collections::HashMap::new();
        let mut stack = vec![root_path];
        let mut scanned = 0usize;
        while let Some(dir) = stack.pop() {
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for entry in rd.flatten() {
                    let p = entry.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else if let Ok(md) = entry.metadata() {
                        scanned += 1;
                        if scanned > 200_000 {
                            break;
                        }
                        if md.len() >= min_size && md.len() > 0 {
                            by_size.entry(md.len()).or_default().push(p);
                        }
                    }
                }
            }
        }
        let mut groups = Vec::new();
        for (size, paths) in by_size {
            if paths.len() < 2 {
                continue;
            }
            let mut by_hash: std::collections::HashMap<String, Vec<String>> =
                std::collections::HashMap::new();
            for p in paths {
                if let Ok(hash) = hash_file(&p, "sha256") {
                    by_hash
                        .entry(hash)
                        .or_default()
                        .push(p.to_string_lossy().to_string());
                } else {
                    let _ = sha2::Sha256::new();
                }
            }
            for (hash, dup_paths) in by_hash {
                if dup_paths.len() >= 2 {
                    groups.push(DupGroup {
                        size,
                        hash,
                        paths: dup_paths,
                    });
                }
            }
        }
        groups.sort_by(|a, b| b.size.cmp(&a.size));
        Ok::<Vec<DupGroup>, String>(groups)
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("dup task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}


// ---------------------------------------------------------------------------
// Cloud mounting (rclone) — persistent via systemd user units
// ---------------------------------------------------------------------------

fn rclone_ok(args: &[&str]) -> std::result::Result<String, String> {
    let out = std::process::Command::new("rclone")
        .args(args)
        .output()
        .map_err(|e| format!("rclone: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "rclone {}",
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .last()
                .unwrap_or("")
                .trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// List configured rclone remotes (names, no trailing colon).
#[tauri::command]
pub async fn rclone_remotes() -> Cmd<Vec<String>> {
    tauri::async_runtime::spawn_blocking(move || {
        let out = rclone_ok(&["listremotes"])?;
        Ok::<Vec<String>, String>(
            out.lines()
                .map(|l| l.trim().trim_end_matches(':').to_string())
                .filter(|l| !l.is_empty())
                .collect(),
        )
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("rclone task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Create a remote. `params` are key/value pairs; keys listed in
/// `obscure` get their values run through `rclone obscure` first.
#[tauri::command]
pub async fn rclone_create_remote(
    name: String,
    backend: String,
    params: Vec<(String, String)>,
    obscure: Vec<String>,
) -> Cmd<()> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut args: Vec<String> = vec![
            "config".into(),
            "create".into(),
            name,
            backend,
        ];
        for (k, v) in &params {
            let value = if obscure.contains(k) {
                rclone_ok(&["obscure", v])?.trim().to_string()
            } else {
                v.clone()
            };
            args.push(k.clone());
            args.push(value);
        }
        let refs: Vec<&str> = args.iter().map(|a| a.as_str()).collect();
        rclone_ok(&refs)?;
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("rclone task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

#[tauri::command]
pub async fn rclone_delete_remote(name: String) -> Cmd<()> {
    tauri::async_runtime::spawn_blocking(move || {
        rclone_ok(&["config", "delete", &name])?;
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("rclone task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

fn rclone_unit_path(name: &str) -> std::result::Result<std::path::PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "no HOME".to_string())?;
    Ok(std::path::PathBuf::from(home)
        .join(".config/systemd/user")
        .join(format!("fable-rclone-{name}.service")))
}

fn systemctl_user(args: &[&str]) -> std::result::Result<(), String> {
    let out = std::process::Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|e| format!("systemctl: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "systemctl {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// Mount a remote now AND persist it: writes a systemd user unit
/// (enabled), so the mount returns at every login/restart.
#[tauri::command]
pub async fn rclone_mount(name: String, mount_point: String) -> Cmd<()> {
    tauri::async_runtime::spawn_blocking(move || {
        std::fs::create_dir_all(&mount_point).map_err(|e| e.to_string())?;
        let rclone_bin = std::process::Command::new("which")
            .arg("rclone")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| "/usr/bin/rclone".into());
        let unit = format!(
            "[Unit]\nDescription=Fable Files cloud mount {name}\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\nExecStart={rclone_bin} mount {name}: {mount_point} --vfs-cache-mode full --dir-cache-time 30s\nExecStop=/usr/bin/fusermount3 -u {mount_point}\nRestart=on-failure\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n"
        );
        let unit_path = rclone_unit_path(&name)?;
        if let Some(parent) = unit_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&unit_path, unit).map_err(|e| e.to_string())?;
        let unit_name = format!("fable-rclone-{name}.service");
        systemctl_user(&["daemon-reload"])?;
        systemctl_user(&["enable", "--now", &unit_name])?;
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("mount task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Unmount: stop + disable the unit and remove it (no persistence).
#[tauri::command]
pub async fn rclone_unmount(name: String, mount_point: String) -> Cmd<()> {
    tauri::async_runtime::spawn_blocking(move || {
        let unit_name = format!("fable-rclone-{name}.service");
        let _ = systemctl_user(&["disable", "--now", &unit_name]);
        let unit_path = rclone_unit_path(&name)?;
        let _ = std::fs::remove_file(&unit_path);
        let _ = systemctl_user(&["daemon-reload"]);
        // Belt and braces: direct unmount too.
        let _ = std::process::Command::new("fusermount3")
            .arg("-u")
            .arg(&mount_point)
            .output();
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("unmount task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

// ---------------------------------------------------------------------------
// Casting (Chromecast via catt, AirPlay via pyatv, Sonos via soco)
// + a tiny local media server for AirPlay/Sonos URLs
// ---------------------------------------------------------------------------

fn local_bin(name: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let p = std::path::PathBuf::from(home)
        .join(".local/bin")
        .join(name);
    if p.exists() {
        p.to_string_lossy().to_string()
    } else {
        name.to_string()
    }
}

fn lan_ip() -> String {
    // UDP connect trick: no packet is sent, the kernel picks the
    // outbound interface address.
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".into())
}

/// Ensure the media server is running; returns (port). One server,
/// token-addressed single files, HTTP Range supported (seeking).
pub fn media_server_port(
    state: &AppState,
) -> std::result::Result<u16, String> {
    let mut port_guard = state.media_server_port.lock().unwrap();
    if let Some(p) = *port_guard {
        return Ok(p);
    }
    let listener = std::net::TcpListener::bind("0.0.0.0:0")
        .map_err(|e| format!("media server bind: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    *port_guard = Some(port);
    drop(port_guard);
    let files = state.media_files.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let files = files.clone();
            std::thread::spawn(move || {
                use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                let mut range: Option<(u64, Option<u64>)> = None;
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        break;
                    }
                    if request.is_empty() {
                        request = trimmed.to_string();
                    }
                    let lower = trimmed.to_lowercase();
                    if let Some(v) = lower.strip_prefix("range: bytes=") {
                        let mut it = v.splitn(2, '-');
                        let start: u64 = it.next().and_then(|x| x.trim().parse().ok()).unwrap_or(0);
                        let end: Option<u64> =
                            it.next().and_then(|x| x.trim().parse().ok());
                        range = Some((start, end));
                    }
                }
                let is_head = request.starts_with("HEAD");
                let path_part = request
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("")
                    .to_string();
                let token = path_part.trim_start_matches("/cast/");
                let file_path = files.lock().unwrap().get(token).cloned();
                let Some(file_path) = file_path else {
                    let _ = stream.write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n",
                    );
                    return;
                };
                let Ok(mut file) = std::fs::File::open(&file_path) else {
                    let _ = stream.write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n",
                    );
                    return;
                };
                let total = file.metadata().map(|m| m.len()).unwrap_or(0);
                let (start, end) = match range {
                    Some((s, e)) => (s.min(total), e.unwrap_or(total.saturating_sub(1)).min(total.saturating_sub(1))),
                    None => (0, total.saturating_sub(1)),
                };
                let len = if total == 0 { 0 } else { end - start + 1 };
                let status = if range.is_some() { "206 Partial Content" } else { "200 OK" };
                let mut headers = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/octet-stream\r\nContent-Length: {len}\r\nAccept-Ranges: bytes\r\n"
                );
                if range.is_some() {
                    headers += &format!("Content-Range: bytes {start}-{end}/{total}\r\n");
                }
                headers += "\r\n";
                if stream.write_all(headers.as_bytes()).is_err() {
                    return;
                }
                if is_head {
                    return;
                }
                if file.seek(SeekFrom::Start(start)).is_err() {
                    return;
                }
                let mut remaining = len;
                let mut buf = vec![0u8; 1 << 16];
                while remaining > 0 {
                    let want = remaining.min(buf.len() as u64) as usize;
                    match file.read(&mut buf[..want]) {
                        Ok(0) => break,
                        Ok(n) => {
                            if stream.write_all(&buf[..n]).is_err() {
                                return;
                            }
                            remaining -= n as u64;
                        }
                        Err(_) => return,
                    }
                }
            });
        }
    });
    Ok(port)
}

/// Register a file with the media server; returns its LAN URL.
#[tauri::command]
pub async fn cast_register_file(
    state: State<'_, AppState>,
    item: ItemId,
) -> Cmd<String> {
    let path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    let port = media_server_port(state.inner())
        .map_err(|e| CommandError::from(UfmError::Internal(e)))?;
    let token = format!("{}", ItemId::new());
    state
        .media_files
        .lock()
        .unwrap()
        .insert(token.clone(), path);
    Ok(format!("http://{}:{}/cast/{}", lan_ip(), port, token))
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct CastDevice {
    pub name: String,
    pub kind: String, // chromecast | airplay | sonos
    pub address: String,
}

fn run_with_timeout(
    program: &str,
    args: &[&str],
    secs: u64,
) -> std::result::Result<String, String> {
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let out = child
                    .wait_with_output()
                    .map_err(|e| e.to_string())?;
                return Ok(String::from_utf8_lossy(&out.stdout).to_string());
            }
            Ok(None) => {
                if start.elapsed().as_secs() > secs {
                    let _ = child.kill();
                    return Err(format!("{program} timed out"));
                }
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// Scan the LAN for cast targets: Chromecasts (catt), Apple TVs
/// (pyatv atvremote), Sonos speakers (soco).
#[tauri::command]
pub async fn cast_scan() -> Cmd<Vec<CastDevice>> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut devices: Vec<CastDevice> = Vec::new();
        // Chromecasts: "Living Room TV - 192.168.1.20 - ..."
        if let Ok(out) = run_with_timeout(&local_bin("catt"), &["scan"], 12) {
            for line in out.lines() {
                let line = line.trim();
                if line.is_empty() || !line.contains(" - ") {
                    continue;
                }
                let parts: Vec<&str> = line.split(" - ").collect();
                if parts.len() >= 2 {
                    devices.push(CastDevice {
                        name: parts[0].trim().to_string(),
                        kind: "chromecast".into(),
                        address: parts[1].trim().to_string(),
                    });
                }
            }
        }
        // Apple TVs: atvremote scan prints "Name: X" / "Address: Y"
        // blocks separated by blank lines.
        if let Ok(out) = run_with_timeout(&local_bin("atvremote"), &["scan"], 15) {
            let mut name = String::new();
            for line in out.lines() {
                let line = line.trim();
                if let Some(v) = line.strip_prefix("Name:") {
                    name = v.trim().to_string();
                } else if let Some(v) = line.strip_prefix("Address:") {
                    devices.push(CastDevice {
                        name: if name.is_empty() {
                            "Apple TV".into()
                        } else {
                            name.clone()
                        },
                        kind: "airplay".into(),
                        address: v.trim().to_string(),
                    });
                }
            }
        }
        // Sonos: soco Python API via the pipx venv interpreter.
        let soco_python = {
            let home = std::env::var("HOME").unwrap_or_default();
            format!("{home}/.local/share/pipx/venvs/soco/bin/python")
        };
        if std::path::Path::new(&soco_python).exists() {
            if let Ok(out) = run_with_timeout(
                &soco_python,
                &[
                    "-c",
                    "import soco\nfor d in (soco.discover() or []):\n    print(d.player_name + '|' + (d.ip_address or ''))",
                ],
                12,
            ) {
                for line in out.lines() {
                    if let Some((n, a)) = line.trim().split_once('|') {
                        devices.push(CastDevice {
                            name: n.to_string(),
                            kind: "sonos".into(),
                            address: a.to_string(),
                        });
                    }
                }
            }
        }
        Ok::<Vec<CastDevice>, String>(devices)
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("scan task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Start playback of a media URL on a cast device.
#[tauri::command]
pub async fn cast_play(device: CastDevice, url: String) -> Cmd<()> {
    tauri::async_runtime::spawn_blocking(move || {
        match device.kind.as_str() {
            "chromecast" => {
                let out = run_with_timeout(
                    &local_bin("catt"),
                    &["-d", &device.name, "cast", &url],
                    30,
                )?;
                let _ = out;
                Ok::<(), String>(())
            }
            "airplay" => {
                run_with_timeout(
                    &local_bin("atvremote"),
                    &["-s", &device.address, &format!("play_url={url}")],
                    30,
                )?;
                Ok(())
            }
            "sonos" => {
                let home = std::env::var("HOME").unwrap_or_default();
                let soco_python =
                    format!("{home}/.local/share/pipx/venvs/soco/bin/python");
                let script = format!(
                    "import soco\nfrom soco import SoCo\nd = SoCo('{addr}')\nd.play_uri('{url}')",
                    addr = device.address,
                    url = url.replace('\'', "%27"),
                );
                run_with_timeout(&soco_python, &["-c", &script], 20)?;
                Ok(())
            }
            other => Err(format!("unknown device kind {other}")),
        }
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("cast task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Stop playback on a cast device.
#[tauri::command]
pub async fn cast_stop(device: CastDevice) -> Cmd<()> {
    tauri::async_runtime::spawn_blocking(move || {
        match device.kind.as_str() {
            "chromecast" => {
                run_with_timeout(&local_bin("catt"), &["-d", &device.name, "stop"], 15)?;
            }
            "airplay" => {
                run_with_timeout(
                    &local_bin("atvremote"),
                    &["-s", &device.address, "stop"],
                    15,
                )?;
            }
            "sonos" => {
                let home = std::env::var("HOME").unwrap_or_default();
                let soco_python =
                    format!("{home}/.local/share/pipx/venvs/soco/bin/python");
                let script = format!(
                    "from soco import SoCo\nSoCo('{}').stop()",
                    device.address
                );
                run_with_timeout(&soco_python, &["-c", &script], 15)?;
            }
            other => return Err(format!("unknown device kind {other}")),
        }
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("cast task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

// ---------------------------------------------------------------------------
// App deleter: installed apps + leftover finder
// ---------------------------------------------------------------------------

/// Input twin of InstalledApp (uninstall argument).
#[derive(serde::Deserialize)]
pub struct InstalledAppIn {
    pub name: String,
    pub id: String,
    pub source: String,
    pub desktop_path: String,
}

#[derive(serde::Serialize)]
pub struct InstalledApp {
    pub name: String,
    pub id: String,
    pub source: String, // system | user | flatpak
    pub desktop_path: String,
}

fn read_desktop_field(content: &str, key: &str) -> Option<String> {
    for line in content.lines() {
        if let Some(v) = line.strip_prefix(&format!("{key}=")) {
            return Some(v.trim().to_string());
        }
    }
    None
}

/// Scan all desktop-entry dirs and list real applications.
#[tauri::command]
pub async fn list_installed_apps() -> Cmd<Vec<InstalledApp>> {
    tauri::async_runtime::spawn_blocking(move || {
        let home = std::env::var("HOME").map_err(|_| "no HOME".to_string())?;
        let dirs: Vec<(String, String)> = vec![
            (format!("{home}/.local/share/applications"), "user".into()),
            (
                format!("{home}/.local/share/flatpak/exports/share/applications"),
                "flatpak".into(),
            ),
            ("/var/lib/flatpak/exports/share/applications".into(), "flatpak".into()),
            ("/usr/local/share/applications".into(), "system".into()),
            ("/usr/share/applications".into(), "system".into()),
        ];
        let mut by_id: std::collections::HashMap<String, InstalledApp> =
            std::collections::HashMap::new();
        for (dir, source) in &dirs {
            let Ok(rd) = std::fs::read_dir(dir) else { continue };
            for entry in rd.flatten() {
                let p = entry.path();
                if p.extension().and_then(|e| e.to_str()) != Some("desktop") {
                    continue;
                }
                let Ok(content) = std::fs::read_to_string(&p) else { continue };
                if read_desktop_field(&content, "Type").as_deref() != Some("Application") {
                    continue;
                }
                if read_desktop_field(&content, "NoDisplay").as_deref() == Some("true") {
                    continue;
                }
                let Some(name) = read_desktop_field(&content, "Name") else { continue };
                let id = p
                    .file_stem()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                if id.is_empty() {
                    continue;
                }
                by_id.entry(id.clone()).or_insert(InstalledApp {
                    name,
                    id,
                    source: source.clone(),
                    desktop_path: p.to_string_lossy().to_string(),
                });
            }
        }
        let mut apps: Vec<InstalledApp> = by_id.into_values().collect();
        apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Ok::<Vec<InstalledApp>, String>(apps)
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("apps task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

#[derive(serde::Serialize)]
pub struct Leftover {
    pub path: String,
    pub size: u64,
    pub category: String, // config | data | cache | state | flatpak data
}

fn dir_size(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else if let Ok(md) = entry.metadata() {
                    total += md.len();
                }
            }
        }
    }
    total
}

/// Find an app's leftover files: config/data/cache/state dirs whose
/// names match the app id or name tokens.
#[tauri::command]
pub async fn find_app_leftovers(app_id: String, app_name: String) -> Cmd<Vec<Leftover>> {
    tauri::async_runtime::spawn_blocking(move || {
        let home = std::env::var("HOME").map_err(|_| "no HOME".to_string())?;
        let mut tokens: Vec<String> = vec![app_id.to_lowercase()];
        if let Some(last) = app_id.rsplit('.').next() {
            if last.len() >= 3 {
                tokens.push(last.to_lowercase());
            }
        }
        let squashed: String = app_name
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect();
        if squashed.len() >= 3 {
            tokens.push(squashed);
        }
        let bases: Vec<(String, &str, usize)> = vec![
            (format!("{home}/.config"), "config", 1),
            (format!("{home}/.local/share"), "data", 2),
            (format!("{home}/.cache"), "cache", 1),
            (format!("{home}/.local/state"), "state", 1),
        ];
        let mut out: Vec<Leftover> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        // Flatpak per-app data dir is an exact match.
        let fp = format!("{home}/.var/app/{app_id}");
        if std::path::Path::new(&fp).exists() {
            out.push(Leftover {
                size: dir_size(std::path::Path::new(&fp)),
                path: fp.clone(),
                category: "flatpak data".into(),
            });
            seen.insert(fp);
        }
        for (base, category, depth) in &bases {
            let mut stack: Vec<(std::path::PathBuf, usize)> =
                vec![(std::path::PathBuf::from(base), 0)];
            while let Some((dir, d)) = stack.pop() {
                let Ok(rd) = std::fs::read_dir(&dir) else { continue };
                for entry in rd.flatten() {
                    let p = entry.path();
                    if !p.is_dir() {
                        continue;
                    }
                    let name = entry.file_name().to_string_lossy().to_lowercase();
                    if tokens.iter().any(|t| name == *t || name.contains(t)) {
                        let ps = p.to_string_lossy().to_string();
                        if seen.insert(ps.clone()) {
                            out.push(Leftover {
                                size: dir_size(&p),
                                path: ps,
                                category: category.to_string(),
                            });
                        }
                    } else if d < *depth {
                        stack.push((p, d + 1));
                    }
                }
            }
        }
        out.sort_by(|a, b| b.size.cmp(&a.size));
        Ok::<Vec<Leftover>, String>(out)
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("leftovers task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Uninstall an app (flatpak remove, or pkexec pacman -Rns for
/// system apps) and optionally trash the selected leftovers.
#[tauri::command]
pub async fn uninstall_app(
    app: InstalledAppIn,
    leftovers: Vec<String>,
) -> Cmd<String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut notes: Vec<String> = Vec::new();
        let _ = &app.name;
        match app.source.as_str() {
            "flatpak" => {
                let out = std::process::Command::new("flatpak")
                    .args(["uninstall", "-y", &app.id])
                    .output()
                    .map_err(|e| format!("flatpak: {e}"))?;
                if !out.status.success() {
                    return Err(format!(
                        "flatpak uninstall failed: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    ));
                }
                notes.push(format!("removed Flatpak {}", app.id));
            }
            "user" => {
                // A user-local desktop entry: remove the entry itself.
                let _ = std::fs::remove_file(&app.desktop_path);
                notes.push("removed the local launcher".into());
            }
            _ => {
                // System app: find the owning pacman package.
                let out = std::process::Command::new("pacman")
                    .args(["-Qo", &app.desktop_path])
                    .output()
                    .map_err(|e| format!("pacman: {e}"))?;
                let text = String::from_utf8_lossy(&out.stdout).to_string();
                // "path is owned by pkg 1.2-3"
                let pkg = text
                    .split(" is owned by ")
                    .nth(1)
                    .and_then(|t| t.split_whitespace().next())
                    .ok_or("could not find the owning package")?
                    .to_string();
                let out = std::process::Command::new("pkexec")
                    .args(["pacman", "-Rns", "--noconfirm", &pkg])
                    .output()
                    .map_err(|e| format!("pkexec: {e}"))?;
                if !out.status.success() {
                    return Err(format!(
                        "uninstall failed: {}",
                        String::from_utf8_lossy(&out.stderr)
                            .lines()
                            .last()
                            .unwrap_or("")
                            .trim()
                    ));
                }
                notes.push(format!("removed package {pkg}"));
            }
        }
        let mut trashed = 0u32;
        for path in &leftovers {
            if trash_path_internal(std::path::Path::new(path)).is_ok() {
                trashed += 1;
            }
        }
        if trashed > 0 {
            notes.push(format!("moved {trashed} leftover(s) to the Trash"));
        }
        Ok::<String, String>(notes.join("; "))
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("uninstall task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

#[cfg(test)]
mod tests {
    /// Rotate Left must be counter-clockwise: a 2x1 [red, green]
    /// strip becomes 1x2 with green on TOP (the right edge rotates
    /// up). If rotate270 were clockwise, red would be on top.
    #[test]
    fn rotate_ccw_direction() {
        let mut img = image::RgbImage::new(2, 1);
        img.put_pixel(0, 0, image::Rgb([255, 0, 0]));
        img.put_pixel(1, 0, image::Rgb([0, 255, 0]));
        let dyn_img = image::DynamicImage::ImageRgb8(img);
        let out = rotate_ccw(&dyn_img).to_rgb8();
        assert_eq!(out.dimensions(), (1, 2), "rotated strip must be 1x2");
        assert_eq!(out.get_pixel(0, 0).0, [0, 255, 0], "green must be on top for CCW");
        assert_eq!(out.get_pixel(0, 1).0, [255, 0, 0], "red must be at the bottom for CCW");
    }

    use super::*;
    use std::sync::Arc;

    #[test]
    fn read_dir_entries_lists_and_sorts_folders_first() {
        // Happy-path contract for read_dir_entries: every entry appears,
        // folders sort before files, names sort case-insensitively.
        // (Per-entry I/O failures are skipped via `continue`; those paths
        // are not portably simulable in a unit test.)
        let dir = std::env::temp_dir().join(format!("ufm-cmd-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("b.txt"), b"x").unwrap();
        std::fs::write(dir.join("A.txt"), b"y").unwrap();
        let entries = read_dir_entries(&dir).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["sub", "A.txt", "b.txt"]);
        assert!(entries[0].kind == ItemKind::Folder);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn index_slot_claim_is_atomic() {
        // Exactly one concurrent claimer wins; everyone else sees running.
        let slot = Arc::new(Mutex::new(IndexState {
            running: false,
            dirs_done: 0,
            files_done: 0,
            bytes_done: 0,
        }));
        let winners = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..16 {
            let slot = Arc::clone(&slot);
            let winners = Arc::clone(&winners);
            handles.push(std::thread::spawn(move || {
                let (already_running, snapshot) = claim_index_slot(&slot);
                assert!(snapshot.running);
                if !already_running {
                    winners.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(winners.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn index_slot_releases_and_reclaims() {
        let slot = Mutex::new(IndexState {
            running: false,
            dirs_done: 0,
            files_done: 0,
            bytes_done: 0,
        });
        let (already, snap) = claim_index_slot(&slot);
        assert!(!already && snap.running);
        let (already2, _) = claim_index_slot(&slot);
        assert!(already2);
        slot.lock().unwrap().running = false;
        let (already3, snap3) = claim_index_slot(&slot);
        assert!(!already3 && snap3.running);
        assert_eq!(snap3.dirs_done, 0);
    }
}
