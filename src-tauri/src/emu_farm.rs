//! The alias keeper (amendment §5): maintains the unified view —
//! the `~/ROMs` symlink farm in ES-DE layout plus RetroArch
//! playlists — by reconciliation. Desired state comes from the
//! catalog (identities + copies, ranked by Library Memory's live
//! mount set); only the delta is ever applied. Interrupted runs
//! heal on the next trigger; nothing here can dangle, because a
//! game with no present copy simply has no link.
//!
//! Fable itself never reads the view. Play resolves real paths
//! from the catalog (prime directive) — the view exists for ES-DE,
//! for RetroArch's own browsing, and for the man himself.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::Ordering;

use serde::Serialize;
use tauri::{Emitter, Manager, State};
use ufm_emu::{DesiredLink, FarmCopy, FarmGame, FarmOp};

use crate::commands::{Cmd, CommandError};
use crate::emu::{
    esde_id, identity_of, load_settings, probe_env, save_settings, state_of, EmuSettings,
};
use crate::AppState;
use ufm_core::UfmError;

const README_NAME: &str = "README — Fable keeps this folder.txt";
const README_TEXT: &str = "This folder is a window, not a warehouse.\n\
\n\
Every entry in here is a link to a game that really lives on one of your drives —\n\
nothing is copied, and deleting a link never deletes a game. Fable keeps this\n\
folder up to date as drives come and go: when a drive unplugs, its games quietly\n\
leave; when it comes back, so do they.\n\
\n\
Your games themselves never move, and Play in Fable always opens the real file.\n";

#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeeperReport {
    /// Catalog identities in scope (v1 systems with games).
    pub identities: usize,
    /// Links that should exist (identities with a present copy).
    pub links: usize,
    pub created: usize,
    pub repointed: usize,
    pub removed: usize,
    /// Desired links skipped because a real file sits in the way
    /// (never overwritten — the view is Fable-managed links only).
    pub blocked: Vec<String>,
    /// Playlist files written (names only).
    pub playlists: Vec<String>,
    /// Distinct volumes the view's games currently live on.
    pub drives: usize,
    pub error: Option<String>,
}

/// A copy's presence + rank, by its home volume (see emu.rs's
/// home_volume — an unplugged drive's files belong to its volume
/// record, not to the internal disk that happens to prefix them).
fn mount_rank(
    path: &str,
    mounted: &[crate::commands::MountedVolume],
    volumes: &[ufm_catalog::VolumeRecord],
) -> Option<(u32, String)> {
    let home = crate::emu::home_volume(path, mounted, volumes)?;
    home.live.then(|| (home.rank, home.root.clone()))
}

/// Group the shelf query into farm games + the systems the keeper
/// manages (any system with at least one catalog game).
fn compute_games(
    catalog: &ufm_catalog::Catalog,
    mounted: &[crate::commands::MountedVolume],
) -> Result<(Vec<FarmGame>, BTreeSet<String>), String> {
    let volumes = catalog.volumes().unwrap_or_default();
    let rows = catalog.rom_home_games(None).map_err(|e| e.to_string())?;
    let mut by_identity: BTreeMap<String, FarmGame> = BTreeMap::new();
    let mut managed: BTreeSet<String> = BTreeSet::new();
    for row in &rows {
        let system = esde_id(&row.platform);
        if ufm_emu::table::system_by_id(&system).is_none() {
            continue;
        }
        managed.insert(system.clone());
        let Some(pk) = row.item.provider_key.clone() else {
            continue;
        };
        let (present, rank) = match mount_rank(&pk, mounted, &volumes) {
            Some((rank, _)) => (true, rank),
            None => (false, u32::MAX),
        };
        let entry = by_identity
            .entry(identity_of(row))
            .or_insert_with(|| FarmGame {
                system: system.clone(),
                identity: identity_of(row),
                title: row.title.clone(),
                copies: Vec::new(),
            });
        entry.copies.push(FarmCopy {
            path: pk,
            present,
            rank,
        });
    }
    Ok((by_identity.into_values().collect(), managed))
}

/// The desired link set + the mounts the targets live on.
fn compute_desired(
    catalog: &ufm_catalog::Catalog,
    mounted: &[crate::commands::MountedVolume],
) -> Result<(Vec<FarmGame>, Vec<DesiredLink>, BTreeSet<String>, usize), String> {
    let volumes = catalog.volumes().unwrap_or_default();
    let (games, managed) = compute_games(catalog, mounted)?;
    let desired = ufm_emu::desired_links(&games);
    let mut mounts: BTreeSet<String> = BTreeSet::new();
    for link in &desired {
        if let Some((_, mp)) = mount_rank(&link.target, mounted, &volumes) {
            mounts.insert(mp);
        }
    }
    Ok((games, desired, managed, mounts.len()))
}

/// Read the farm's actual symlinks in the managed system folders:
/// (rel, target) pairs. Real files are invisible here by design —
/// the keeper never touches what it didn't plant.
fn read_actual(root: &Path, managed: &BTreeSet<String>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for system in managed {
        let dir = root.join(system);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            let Ok(md) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if !md.file_type().is_symlink() {
                continue;
            }
            if let Ok(target) = std::fs::read_link(&path) {
                out.push((
                    format!("{}/{}", system, e.file_name().to_string_lossy()),
                    target.to_string_lossy().into_owned(),
                ));
            }
        }
    }
    out
}

/// Can this directory hold symlinks at all? (The view root must
/// live on a symlink-capable filesystem — exFAT refuses, and the
/// keeper must say so instead of failing link by link.)
fn symlinks_supported(root: &Path) -> bool {
    let probe = root.join(".fable-link-probe");
    let target = Path::new(".");
    let _ = std::fs::remove_file(&probe);
    match std::os::unix::fs::symlink(target, &probe) {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// One reconciliation pass. Safe to call any time; delta-only.
pub fn run_keeper(catalog_path: &Path, data_base: &Path) -> KeeperReport {
    run_keeper_with_mounts(
        catalog_path,
        data_base,
        &crate::commands::current_filesystems(),
    )
}

/// The pass against an explicit sighting set — the app always feeds
/// it Library Memory's live mounts; the acceptance harness feeds it
/// a set with a volume removed to prove the unplug behavior
/// (repoint at a surviving copy, retire when none survives, never
/// dangle) without anyone touching a cable.
pub fn run_keeper_with_mounts(
    catalog_path: &Path,
    data_base: &Path,
    mounted: &[crate::commands::MountedVolume],
) -> KeeperReport {
    let mut report = KeeperReport::default();
    let settings = load_settings(data_base);
    let catalog = match ufm_catalog::Catalog::open(catalog_path) {
        Ok(c) => c,
        Err(e) => {
            report.error = Some(format!("couldn't open the library: {e}"));
            return report;
        }
    };
    let mounted = mounted.to_vec();
    let (games, desired, managed, drives) = match compute_desired(&catalog, &mounted) {
        Ok(v) => v,
        Err(e) => {
            report.error = Some(e);
            return report;
        }
    };
    report.identities = games.len();
    report.links = desired.len();
    report.drives = drives;

    if settings.view_enabled {
        let root = settings.view_root_path();
        // --- the farm -------------------------------------------------
        match std::fs::create_dir_all(&root) {
            Ok(()) => {
                if symlinks_supported(&root) {
                    let readme = root.join(README_NAME);
                    if std::fs::read_to_string(&readme).ok().as_deref() != Some(README_TEXT) {
                        let _ = std::fs::write(&readme, README_TEXT);
                    }
                    let actual = read_actual(&root, &managed);
                    for op in ufm_emu::farm_delta(&desired, &actual) {
                        match op {
                            FarmOp::Create { rel, target } => {
                                let path = root.join(&rel);
                                if let Some(parent) = path.parent() {
                                    let _ = std::fs::create_dir_all(parent);
                                }
                                if path.exists() || std::fs::symlink_metadata(&path).is_ok() {
                                    // Something real sits there —
                                    // never overwrite a user's file.
                                    report.blocked.push(rel);
                                } else if std::os::unix::fs::symlink(&target, &path).is_ok() {
                                    report.created += 1;
                                } else {
                                    report.blocked.push(rel);
                                }
                            }
                            FarmOp::Repoint { rel, target } => {
                                let path = root.join(&rel);
                                let _ = std::fs::remove_file(&path);
                                if std::os::unix::fs::symlink(&target, &path).is_ok() {
                                    report.repointed += 1;
                                } else {
                                    report.blocked.push(rel);
                                }
                            }
                            FarmOp::Remove { rel } => {
                                if std::fs::remove_file(root.join(&rel)).is_ok() {
                                    report.removed += 1;
                                }
                            }
                        }
                    }
                } else {
                    report.error = Some(format!(
                        "{} can't hold links (the drive it's on doesn't support them), so the combined folder can't live there.",
                        root.display()
                    ));
                }
            }
            Err(e) => {
                report.error = Some(format!(
                    "couldn't create the combined folder at {}: {e}",
                    root.display()
                ));
            }
        }

        // --- RetroArch playlists (the second output) ------------------
        let (env, layout) = probe_env();
        if let Some(layout) = layout {
            // title lookup per identity, for playlist labels.
            let titles: BTreeMap<String, String> = games
                .iter()
                .map(|g| (g.identity.clone(), g.title.clone()))
                .collect();
            // identity -> its desired target (best present copy).
            let targets: BTreeMap<String, String> = desired
                .iter()
                .map(|l| (l.identity.clone(), l.target.clone()))
                .collect();
            for system in &managed {
                let Some(sys) = ufm_emu::table::system_by_id(system) else {
                    continue;
                };
                let Some(playlist_name) = sys.playlist_name.clone() else {
                    continue;
                };
                // The default core, when it's actually installed.
                let (core_path, core_name) = match sys.default() {
                    Some(def) => match &def.core_file {
                        Some(cf) if env.cores.contains(cf) => {
                            let p = layout.cores_dir.join(cf);
                            if p.is_file() {
                                (
                                    p.to_string_lossy().into_owned(),
                                    def.core_name.clone().unwrap_or_default(),
                                )
                            } else {
                                (String::new(), String::new())
                            }
                        }
                        _ => (String::new(), String::new()),
                    },
                    None => (String::new(), String::new()),
                };
                let entries: Vec<ufm_emu::PlaylistEntry> = games
                    .iter()
                    .filter(|g| &g.system == system)
                    .filter_map(|g| {
                        targets.get(&g.identity).map(|t| ufm_emu::PlaylistEntry {
                            path: t.clone(),
                            label: titles
                                .get(&g.identity)
                                .cloned()
                                .unwrap_or_else(|| g.title.clone()),
                            core_path: core_path.clone(),
                            core_name: core_name.clone(),
                        })
                    })
                    .collect();
                if entries.is_empty() {
                    continue;
                }
                let db_name = format!("{playlist_name}.lpl");
                let text = ufm_emu::playlist_json(&db_name, &core_path, &core_name, &entries);
                let path = layout.playlists_dir.join(&db_name);
                if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
                    let _ = std::fs::create_dir_all(&layout.playlists_dir);
                    if std::fs::write(&path, &text).is_ok() {
                        report.playlists.push(db_name);
                    }
                }
            }
        }
    }
    report
}

/// Fire-and-forget keeper run (app start, sighting changes,
/// re-index completion, settings changes). Guarded: one pass at a
/// time; a skipped pass heals at the next trigger because every
/// pass is a full delta computation.
pub fn spawn_keeper(app: tauri::AppHandle) {
    {
        let state = app.state::<AppState>();
        if state.emu_keeper_running.swap(true, Ordering::SeqCst) {
            return;
        }
    }
    tauri::async_runtime::spawn_blocking(move || {
        let report = {
            let state = app.state::<AppState>();
            run_keeper(&state.catalog_path, &state.data_base)
        };
        if report.created + report.repointed + report.removed > 0 || report.error.is_some() {
            tracing::info!(
                target: "ufm",
                "alias keeper: {} links ({} created, {} repointed, {} removed), playlists {:?}, error {:?}",
                report.links, report.created, report.repointed, report.removed,
                report.playlists, report.error
            );
        }
        let state = app.state::<AppState>();
        state.emu_keeper_running.store(false, Ordering::SeqCst);
        let _ = app.emit("emu-view-changed", report);
    });
}

// ---------------------------------------------------------------------------
// Preferences → ROMs: the Emulators + Unified view cards' data.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrefsOptionWire {
    pub id: String,
    pub label: String,
    pub verdict: String,
    pub state: ufm_emu::OptState,
    pub state_word: String,
    pub class: ufm_emu::OptClass,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrefsSystemWire {
    /// Catalog platform key (the choice map's key).
    pub platform: String,
    pub system_id: String,
    pub name: String,
    pub short_name: String,
    pub games: i64,
    pub chosen: String,
    pub options: Vec<PrefsOptionWire>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewStateWire {
    pub enabled: bool,
    pub root: String,
    /// Identities with a present copy (= links that belong there).
    pub games: usize,
    pub drives: usize,
    /// Links actually on disk right now.
    pub links_on_disk: usize,
    pub retroarch_present: bool,
    pub last_report: KeeperReport,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmuPrefsWire {
    pub systems: Vec<PrefsSystemWire>,
    pub view: ViewStateWire,
}

fn view_state(state: &AppState, settings: &EmuSettings, report: KeeperReport) -> ViewStateWire {
    let root = settings.view_root_path();
    let links_on_disk = {
        let catalog = ufm_catalog::Catalog::open(&state.catalog_path).ok();
        match catalog {
            Some(cat) => {
                let mounted = crate::commands::current_filesystems();
                compute_games(&cat, &mounted)
                    .map(|(_, managed)| read_actual(&root, &managed).len())
                    .unwrap_or(0)
            }
            None => 0,
        }
    };
    let (_, layout) = probe_env();
    ViewStateWire {
        enabled: settings.view_enabled,
        root: root.to_string_lossy().into_owned(),
        games: report.links,
        drives: report.drives,
        links_on_disk,
        retroarch_present: layout.is_some(),
        last_report: report,
    }
}

#[tauri::command]
pub async fn emu_prefs(app: tauri::AppHandle) -> Cmd<EmuPrefsWire> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let settings = load_settings(&state.data_base);
        let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
            .map_err(CommandError::from)?;
        let counts = catalog.rom_home_counts().map_err(CommandError::from)?;
        let (env, layout) = probe_env();
        let mut systems = Vec::new();
        for (platform, games, _missing) in &counts.per_system {
            if *games <= 0 {
                continue;
            }
            let system_id = esde_id(platform);
            let Some(sys) = ufm_emu::table::system_by_id(&system_id) else {
                continue;
            };
            let chosen = settings
                .choices
                .get(platform)
                .cloned()
                .or_else(|| sys.default_option.clone())
                .unwrap_or_default();
            systems.push(PrefsSystemWire {
                platform: platform.clone(),
                system_id: sys.id.clone(),
                name: sys.fullname.clone(),
                short_name: sys.short_name.clone(),
                games: *games,
                chosen,
                options: sys
                    .options
                    .iter()
                    .map(|o| {
                        let st = state_of(o, &env, layout.as_ref());
                        PrefsOptionWire {
                            id: o.id.clone(),
                            label: o.label.clone(),
                            verdict: o.verdict.clone(),
                            state: st,
                            state_word: st.word().to_string(),
                            class: o.class,
                        }
                    })
                    .collect(),
            });
        }
        // The view state rides a fresh (read-only-cost) keeper pass
        // computation: run the real keeper — Preferences is exactly
        // where "updated when drives come and go" gets verified.
        let report = run_keeper(&state.catalog_path, &state.data_base);
        Ok(EmuPrefsWire {
            systems,
            view: view_state(&state, &settings, report),
        })
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("prefs task failed: {e}"))))?
}

/// Save a system's emulator choice from Preferences.
#[tauri::command]
pub fn emu_set_choice(
    state: State<'_, AppState>,
    platform: String,
    option_id: String,
) -> Cmd<()> {
    let system_id = esde_id(&platform);
    let sys = ufm_emu::table::system_by_id(&system_id).ok_or_else(|| {
        CommandError::from(UfmError::NotFound(format!("unknown system {platform}")))
    })?;
    if sys.option(&option_id).is_none() {
        return Err(CommandError::from(UfmError::InvalidInput(format!(
            "{option_id} isn't an option for {}",
            sys.fullname
        ))));
    }
    let mut settings = load_settings(&state.data_base);
    settings.choices.insert(platform, option_id);
    save_settings(&state.data_base, &settings).map_err(|e| {
        CommandError::from(UfmError::Internal(e))
    })
}

/// Toggle/move the unified view; enabling or moving runs the
/// keeper so the folder is true the moment the card says so.
#[tauri::command]
pub async fn emu_set_view(
    app: tauri::AppHandle,
    enabled: bool,
    root: Option<String>,
) -> Cmd<ViewStateWire> {
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app2.state::<AppState>();
        let mut settings = load_settings(&state.data_base);
        settings.view_enabled = enabled;
        if let Some(r) = root {
            settings.view_root = if r.trim().is_empty() {
                crate::emu::default_view_root()
                    .to_string_lossy()
                    .into_owned()
            } else {
                r
            };
        }
        save_settings(&state.data_base, &settings)
            .map_err(|e| CommandError::from(UfmError::Internal(e)))?;
        let settings = load_settings(&state.data_base);
        let report = run_keeper(&state.catalog_path, &state.data_base);
        Ok(view_state(&state, &settings, report))
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("view task failed: {e}"))))?
}

/// [Rebuild now] on the Unified view card.
#[tauri::command]
pub async fn emu_rebuild_view(app: tauri::AppHandle) -> Cmd<ViewStateWire> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let settings = load_settings(&state.data_base);
        let report = run_keeper(&state.catalog_path, &state.data_base);
        Ok(view_state(&state, &settings, report))
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("rebuild task failed: {e}"))))?
}
