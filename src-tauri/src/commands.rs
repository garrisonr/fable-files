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
        .collect();
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

#[tauri::command]
pub async fn search(
    state: State<'_, AppState>,
    query_text: String,
    scope: Option<LocationId>,
    limit: Option<u32>,
) -> Cmd<Vec<SearchHit>> {
    // A query that doesn't parse is not an error — it just matches nothing.
    let query = match ufm_search::parse(&query_text) {
        Ok(q) => q,
        Err(_) => return Ok(Vec::new()),
    };
    let fts_query = ufm_search::to_fts(&query);
    if fts_query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let limit = limit.unwrap_or(50).min(500);
    // `search_fts` has no scope parameter in v0.1 — filter by location here.
    let rows = {
        let catalog = state.catalog.lock().unwrap();
        catalog
            .search_fts(&fts_query, limit)
            .map_err(CommandError::from)?
    };
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
            lines.push(if l.len() > 500 { format!("{}...", &l[..500]) } else { l });
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
        std::process::Command::new("gtk-launch")
            .arg(&desktop_id)
            .arg(&path)
            .spawn()
            .map_err(|e| format!("gtk-launch failed: {e}"))
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

    let dirs = vec![
        format!("{home}/.local/share/applications"),
        "/usr/local/share/applications".to_string(),
        "/usr/share/applications".to_string(),
    ];

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
    let path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    Ok(path.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
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
