//! Fable Files — Tauri v2 application entry.
//!
//! The Rust core owns truth; this shell owns presentation. The web UI gets
//! typed commands and domain events, nothing more.
//!
//! `AppState` is built once in the `setup` hook (it needs the app-data dir
//! from the running app) and then managed for the command layer.

pub mod commands;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::Manager;
use ufm_catalog::Catalog;
use ufm_core::{ItemId, LocationId};
use ufm_preview::PreviewRegistry;

/// Progress snapshot for the background indexer. Serialized to the UI on the
/// `index-progress` event and returned by `index_status` / `start_indexing`.
#[derive(Debug, Clone, Serialize)]
pub struct IndexState {
    pub running: bool,
    pub dirs_done: u64,
    pub files_done: u64,
    pub bytes_done: u64,
}

/// Shared application state: the catalog, registries, and services.
/// Built once at startup; commands borrow it.
///
/// LOCK-ORDER DISCIPLINE (deadlock prevention): any code that holds more
/// than one of `path_ids` / `item_paths` / `item_locations` at the same time
/// MUST acquire them in the order `path_ids -> item_paths -> item_locations`.
/// Taking a subset is fine as long as the relative order is preserved
/// (e.g. `preview` takes `item_paths -> item_locations`). Never introduce a
/// new multi-lock site that violates this order — Tauri runs commands
/// concurrently and ABBA locking hangs the app permanently.
pub struct AppState {
    pub catalog: Mutex<Catalog>,
    /// Kept so background tasks (search reads, the indexer) can open their
    /// own short-lived connections instead of borrowing the managed one.
    pub catalog_path: PathBuf,
    pub checkpoint_path: PathBuf,
    pub previews: PreviewRegistry,
    pub themes_dir: PathBuf,
    pub active_theme: Mutex<String>,
    pub local_roots: Mutex<HashMap<LocationId, PathBuf>>,
    pub root_items: Mutex<HashMap<LocationId, ItemId>>,
    pub locations: Mutex<Vec<commands::LocationSummary>>,
    /// Live-browse cache: item id -> absolute path. Populated by `list_dir`
    /// and `search`. The catalog is the durable store; this is just a shortcut
    /// so `preview` / `list_dir` don't need a catalog item reader.
    pub item_paths: Mutex<HashMap<ItemId, PathBuf>>,
    /// Reverse map so re-listing a directory reuses stable item ids.
    pub path_ids: Mutex<HashMap<PathBuf, ItemId>>,
    pub item_locations: Mutex<HashMap<ItemId, LocationId>>,
    pub index_state: Mutex<IndexState>,
}

/// One row of the `locations.json` sidecar. The catalog persists locations but
/// stores no root paths (`ufm_vfs::mount_local` drops the `PathBuf`), so this
/// tiny file keeps id -> root path (+ root item id) durable across restarts.
#[derive(Debug, Serialize, Deserialize)]
struct PersistedLocation {
    id: String,
    root_item: String,
    provider: String,
    display_name: String,
    root: String,
}

impl AppState {
    fn build(app: &tauri::App) -> Result<Self, String> {
        let app_data = app
            .path()
            .app_data_dir()
            .map_err(|e| format!("app_data_dir: {e}"))?;
        let base = app_data.join("fable-files");
        std::fs::create_dir_all(&base)
            .map_err(|e| format!("create {}: {e}", base.display()))?;
        let catalog_path = base.join("catalog.db");
        let checkpoint_path = base.join("index-checkpoint.json");
        let themes_dir = app_data.join("themes");
        std::fs::create_dir_all(&themes_dir)
            .map_err(|e| format!("create {}: {e}", themes_dir.display()))?;

        let catalog =
            Catalog::open(&catalog_path).map_err(|e| format!("open catalog: {e}"))?;

        let state = Self {
            catalog: Mutex::new(catalog),
            catalog_path,
            checkpoint_path,
            previews: ufm_preview::registry_with_builtins(),
            themes_dir: themes_dir.clone(),
            active_theme: Mutex::new("grizzette".to_string()),
            local_roots: Mutex::new(HashMap::new()),
            root_items: Mutex::new(HashMap::new()),
            locations: Mutex::new(Vec::new()),
            item_paths: Mutex::new(HashMap::new()),
            path_ids: Mutex::new(HashMap::new()),
            item_locations: Mutex::new(HashMap::new()),
            index_state: Mutex::new(IndexState {
                running: false,
                dirs_done: 0,
                files_done: 0,
                bytes_done: 0,
            }),
        };

        seed_themes(&themes_dir)?;
        state.restore_or_seed_locations(&base)?;
        Ok(state)
    }

    /// Rebuild the in-memory location maps from the sidecar, or seed the
    /// Home location on a truly fresh install (empty catalog).
    fn restore_or_seed_locations(&self, base: &Path) -> Result<(), String> {
        let sidecar = base.join("locations.json");
        if let Ok(data) = std::fs::read(&sidecar) {
            if let Ok(saved) = serde_json::from_slice::<Vec<PersistedLocation>>(&data) {
                let mut restored = 0;
                for loc in saved {
                    let (Ok(id), Ok(root_item)) = (
                        loc.id.parse::<uuid::Uuid>(),
                        loc.root_item.parse::<uuid::Uuid>(),
                    ) else {
                        continue;
                    };
                    let id = LocationId(id);
                    let root_item = ItemId(root_item);
                    let root = PathBuf::from(&loc.root);
                    if !root.is_dir() {
                        continue;
                    }
                    self.local_roots.lock().unwrap().insert(id, root);
                    self.root_items.lock().unwrap().insert(id, root_item);
                    self.locations.lock().unwrap().push(commands::LocationSummary {
                        id,
                        provider: loc.provider.clone(),
                        display_name: loc.display_name.clone(),
                        root_item,
                    });
                    restored += 1;
                }
                if restored > 0 {
                    tracing::info!(target: "ufm", "restored {restored} locations from sidecar");
                    return Ok(());
                }
            }
        }

        // No usable sidecar: seed only when the catalog itself is empty.
        let count = self
            .catalog
            .lock()
            .unwrap()
            .location_count()
            .map_err(|e| e.to_string())?;
        if count > 0 {
            tracing::warn!(
                target: "ufm",
                "catalog has {count} locations but no usable sidecar; skipping seed"
            );
            return Ok(());
        }

        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .map(PathBuf::from)
            .map_err(|_| "HOME/USERPROFILE is not set".to_string())?;
        let location = ufm_vfs::mount_local("Home", home.clone());
        let id = location.id;
        self.catalog
            .lock()
            .unwrap()
            .insert_location(&location)
            .map_err(|e| e.to_string())?;
        let root_name = home
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Home".to_string());
        let root_item = ufm_vfs::Item {
            id: ItemId::new(),
            location: id,
            name: root_name,
            kind: ufm_vfs::ItemKind::Folder,
            size_bytes: None,
            modified: None,
            provider_key: Some(home.to_string_lossy().into_owned()),
            labels: Vec::new(),
            starred: false,
        };
        self.catalog
            .lock()
            .unwrap()
            .insert_item(&root_item)
            .map_err(|e| e.to_string())?;

        self.local_roots.lock().unwrap().insert(id, home.clone());
        self.root_items.lock().unwrap().insert(id, root_item.id);
        self.locations.lock().unwrap().push(commands::LocationSummary {
            id,
            provider: location.provider.clone(),
            display_name: location.display_name.clone(),
            root_item: root_item.id,
        });

        let saved = vec![PersistedLocation {
            id: id.to_string(),
            root_item: root_item.id.to_string(),
            provider: location.provider,
            display_name: location.display_name,
            root: home.to_string_lossy().into_owned(),
        }];
        let json = serde_json::to_string_pretty(&saved).map_err(|e| e.to_string())?;
        // Atomic write: temp file + rename, never truncate the good copy.
        let tmp = sidecar.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &sidecar).map_err(|e| e.to_string())?;
        tracing::info!(target: "ufm", "seeded Home location {id}");
        Ok(())
    }
}

/// Copy the repo's bundled themes into the app-data themes dir on first run.
/// Only what is actually in the repo ships — today that is just `grizzette`.
fn seed_themes(themes_dir: &Path) -> Result<(), String> {
    let empty = std::fs::read_dir(themes_dir)
        .map(|mut it| it.next().is_none())
        .unwrap_or(true);
    if !empty {
        return Ok(());
    }
    let src = repo_themes_dir();
    let entries = match std::fs::read_dir(&src) {
        Ok(entries) => entries,
        Err(e) => {
            // Non-fatal: a packaged app whose bundle lacks the themes dir
            // (or a dev checkout moved after build) must still launch — the
            // shell falls back to its built-in default styling. Never fail
            // startup over themes.
            tracing::warn!(target: "ufm", "no repo themes dir at {} ({e}); continuing without seeding", src.display());
            return Ok(());
        }
    };
    let mut seeded = 0;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let from = entry.path();
        if !from.is_dir() {
            continue;
        }
        let to = themes_dir.join(entry.file_name());
        copy_dir_recursive(&from, &to)
            .map_err(|e| format!("seed theme {}: {e}", from.display()))?;
        seeded += 1;
    }
    tracing::info!(target: "ufm", "seeded {seeded} themes from {}", src.display());
    Ok(())
}

/// Locate the repo's `themes/` dir: the installed app's Resources first,
/// then the compile-time repo checkout (dev runs).
fn repo_themes_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            // macOS bundle layout: <App>.app/Contents/MacOS/<exe>
            //                     <App>.app/Contents/Resources/themes
            let cand = exe_dir.join("../Resources/themes");
            if cand.is_dir() {
                return cand;
            }
        }
    }
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../themes"))
}

fn copy_dir_recursive(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &dest)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &dest)?;
        }
        // Symlinks are skipped deliberately — theme packages are plain data.
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter("ufm=debug")
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| -> Result<(), Box<dyn std::error::Error>> {
            let state = AppState::build(app).map_err(|e| format!("state init: {e}"))?;
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_locations,
            commands::list_dir,
            commands::start_copy,
            commands::pause_job,
            commands::resume_job,
            commands::cancel_job,
            commands::search,
            commands::start_indexing,
            commands::index_status,
            commands::list_themes,
            commands::apply_theme,
            commands::theme_contrast_report,
            commands::tag_item,
            commands::toggle_star,
            commands::list_collections,
            commands::preview,
            commands::open_item,
            commands::set_executable,
            commands::preview_text,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Fable Files");
}
