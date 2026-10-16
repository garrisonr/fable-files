//! Emulation v2.1 — the fleet prep pass and emulator update
//! checks. The preinstall answer to "why do I have to wait for a
//! download when I press Play?": Fable keeps the emulators for
//! the systems he OWNS GAMES FOR ready in the background — the
//! default option of every system with at least one catalog game
//! (the fleet), installed through the same channels Play uses
//! (emu::ensure_option), serialized one item at a time on the
//! keeper pattern, checkpointed per item into EmuSettings so an
//! interrupted pass re-probes and skips what's already Ready.
//!
//! Update checks ride the same module: at most one check a day,
//! against each channel's own truth (Flathub's update list, the
//! buildbot's zip headers, GitHub release assets, ES-DE's
//! registry) — and they NEVER apply on their own. Findings land
//! in the settings record; the Preferences "Emulator updates"
//! card and the ROMs-door pill render them; applying takes his
//! click (the Governor philosophy: background work asks first).
//! A failed check is silent — no error toast will ever blame the
//! network at breakfast.
//!
//! BIOS files are never part of any of this — user-supplied only,
//! forever (emu.rs owns that law).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{Emitter, Manager};
use ufm_emu::{InstallSpec, OptClass, OptState, OptionKind, SystemEntry};

use crate::commands::{Cmd, CommandError};
use crate::emu::{self, EmuSettings, Progress, UpdateItem};
use crate::AppState;
use ufm_core::UfmError;

/// Emitted whenever prep state or the updates record changes —
/// the ROMs-door pill and any open Preferences view refresh off it.
pub const FLEET_CHANGED_EVENT: &str = "emu-fleet-changed";

const DAY_SECS: i64 = 86_400;
const RETROARCH_APP_ID: &str = "org.libretro.RetroArch";

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// The fleet (pure computation over the systems table).
// ---------------------------------------------------------------------------

/// One thing the prep pass keeps ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetItem {
    /// Settings-record key: "retroarch" | "option:<system>:<option>".
    pub key: String,
    pub label: String,
    pub system_id: Option<String>,
    pub option_id: Option<String>,
}

fn item_key(system_id: &str, option_id: &str) -> String {
    format!("option:{system_id}:{option_id}")
}

/// The fleet, from the set of ES-DE system ids with at least one
/// catalog game: RetroArch itself first (when any fleet core
/// rides on it), then each system's DEFAULT option — DefaultAble
/// options with a real install channel only. BestEffort/ManualOnly
/// defaults and channel-less options stay on-tap through the Play
/// sheet, exactly as v2 left them; a system whose games he has
/// but whose default needs his hands gets nothing forced on it.
pub fn compute_fleet(
    table: &[SystemEntry],
    systems_with_games: &BTreeSet<String>,
) -> Vec<FleetItem> {
    let mut items: Vec<FleetItem> = Vec::new();
    let mut any_core = false;
    for sys in table {
        if !systems_with_games.contains(&sys.id) {
            continue;
        }
        let Some(opt) = sys.default() else { continue };
        if opt.class != OptClass::DefaultAble {
            continue;
        }
        if matches!(opt.install, InstallSpec::None) {
            continue;
        }
        if opt.kind == OptionKind::LibretroCore {
            any_core = true;
        }
        items.push(FleetItem {
            key: item_key(&sys.id, &opt.id),
            label: opt.label.clone(),
            system_id: Some(sys.id.clone()),
            option_id: Some(opt.id.clone()),
        });
    }
    items.sort_by(|a, b| a.label.cmp(&b.label));
    if any_core {
        items.insert(
            0,
            FleetItem {
                key: "retroarch".to_string(),
                label: "RetroArch".to_string(),
                system_id: None,
                option_id: None,
            },
        );
    }
    items
}

/// ES-DE system ids with at least one catalog game (the catalog is
/// the only membership source — the unified view is never read).
fn systems_with_games(catalog_path: &Path) -> Result<BTreeSet<String>, String> {
    let catalog = ufm_catalog::Catalog::open(catalog_path).map_err(|e| e.to_string())?;
    let counts = catalog.rom_home_counts().map_err(|e| e.to_string())?;
    let mut out = BTreeSet::new();
    for (platform, games, _missing) in &counts.per_system {
        if *games > 0 {
            out.insert(emu::esde_id(platform));
        }
    }
    Ok(out)
}

/// The live fleet: catalog membership plus any extra systems a
/// caller insists on (the acceptance harness extends the fleet to
/// ps3 without owning a PS3 game; production callers pass none).
pub fn fleet_at(
    catalog_path: &Path,
    extra_systems: &BTreeSet<String>,
) -> Result<Vec<FleetItem>, String> {
    let mut systems = systems_with_games(catalog_path)?;
    systems.extend(extra_systems.iter().cloned());
    Ok(compute_fleet(ufm_emu::systems_table(), &systems))
}

fn option_of(item: &FleetItem) -> Option<&'static ufm_emu::EmuOption> {
    let (Some(sys_id), Some(opt_id)) = (&item.system_id, &item.option_id) else {
        return None;
    };
    ufm_emu::table::system_by_id(sys_id)?.option(opt_id)
}

/// Load → mutate → save, so a prep pass never clobbers settings
/// another surface wrote mid-pass (choices, overrides).
fn mutate_settings(base: &Path, f: impl FnOnce(&mut EmuSettings)) -> Result<(), String> {
    let mut settings = emu::load_settings(base);
    f(&mut settings);
    emu::save_settings(base, &settings)
}

// ---------------------------------------------------------------------------
// The prep pass.
// ---------------------------------------------------------------------------

/// What one prep pass did (also the acceptance harness's report).
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepReport {
    pub total: usize,
    pub ready: usize,
    pub installed: usize,
    pub failed: Vec<String>,
}

/// After an install lands, record the channel baselines update
/// checks compare against. Facts come from the channel's own
/// mouth at install time; a baseline Fable can't learn is left
/// absent (checks bootstrap those silently instead).
fn record_install_baseline(base: &Path, opt: &ufm_emu::EmuOption) {
    match &opt.install {
        InstallSpec::RetroArchCore => {
            if let Some(core) = &opt.core_file {
                if let Some((len, modified)) = emu::core_remote_facts(core) {
                    let core = core.clone();
                    let _ = mutate_settings(base, |s| {
                        s.core_records.insert(
                            core,
                            emu::CoreRecord {
                                len,
                                modified,
                                at: now_unix(),
                            },
                        );
                    });
                }
            }
        }
        InstallSpec::GitHubAppImage {
            repo,
            asset_suffix,
            file_name,
        } => {
            if let Ok((_url, digest, asset)) =
                emu::resolve_github_appimage(repo, asset_suffix)
            {
                let file_name = file_name.clone();
                let _ = mutate_settings(base, |s| {
                    s.app_records.insert(
                        file_name,
                        emu::AppImageRecord {
                            asset,
                            sha256: digest.unwrap_or_default(),
                            at: now_unix(),
                        },
                    );
                });
            }
        }
        _ => {}
    }
}

/// One serialized pass over the fleet. Items already Ready are
/// verified and skipped (probe truth, never the record's word);
/// the rest install in fleet order through emu::ensure_option —
/// the same machinery the Play sheet uses, so a prepped system
/// and a sheet-installed system are indistinguishable. Each
/// item's outcome is checkpointed into EmuSettings the moment it
/// lands, and `notify` fires so the door pill and Preferences can
/// refresh mid-pass. One item's failure never stops the pass.
pub fn run_prep_pass_at(
    catalog_path: &Path,
    data_base: &Path,
    extra_systems: &BTreeSet<String>,
    progress: &Progress,
    notify: &dyn Fn(),
) -> Result<PrepReport, String> {
    let fleet = fleet_at(catalog_path, extra_systems)?;
    let mut report = PrepReport {
        total: fleet.len(),
        ..Default::default()
    };
    let (mut env, mut layout) = emu::probe_env();
    for item in &fleet {
        let outcome: Result<bool, String> = (|| {
            if item.key == "retroarch" {
                if layout.is_some() {
                    return Ok(false);
                }
                emu::ensure_retroarch(progress)?;
                return Ok(true);
            }
            let opt = option_of(item)
                .ok_or_else(|| format!("{} is no longer in the table", item.label))?;
            if ufm_emu::option_state(opt, &env) == OptState::Ready {
                return Ok(false);
            }
            emu::ensure_option(opt, progress)?;
            Ok(true)
        })();
        let installed = matches!(outcome, Ok(true));
        if installed {
            let probed = emu::probe_env();
            env = probed.0;
            layout = probed.1;
        }
        match outcome {
            Ok(_) => {
                report.ready += 1;
                if installed {
                    report.installed += 1;
                    if let Some(opt) = option_of(item) {
                        record_install_baseline(data_base, opt);
                    }
                }
                let key = item.key.clone();
                let _ = mutate_settings(data_base, |s| {
                    s.prep.items.insert(
                        key,
                        emu::PrepItemState {
                            state: "ready".to_string(),
                            at: now_unix(),
                            error: String::new(),
                        },
                    );
                });
            }
            Err(e) => {
                report.failed.push(format!("{}: {e}", item.label));
                let key = item.key.clone();
                let _ = mutate_settings(data_base, |s| {
                    s.prep.items.insert(
                        key,
                        emu::PrepItemState {
                            state: "failed".to_string(),
                            at: now_unix(),
                            error: e,
                        },
                    );
                });
            }
        }
        notify();
    }
    let _ = mutate_settings(data_base, |s| {
        s.prep.last_run = now_unix();
    });
    Ok(report)
}

// ---------------------------------------------------------------------------
// Update checks.
// ---------------------------------------------------------------------------

/// The daily throttle (pure): a check is due when the last one is
/// at least a day old (or never happened).
pub fn check_due(checked_at: i64, now: i64) -> bool {
    now - checked_at >= DAY_SECS
}

/// Run one update check (unless throttled, `force` aside) and
/// persist its findings. Every channel is consulted through its
/// own truth; a channel that can't answer is skipped quietly —
/// the check as a whole only errors when the fleet itself can't
/// be computed. Baselines for present-but-unrecorded items are
/// learned silently (a core Fable didn't fetch can't be proven
/// stale; the NEXT buildbot change will flag it honestly).
pub fn run_update_check_at(
    catalog_path: &Path,
    data_base: &Path,
    extra_systems: &BTreeSet<String>,
    force: bool,
) -> Result<emu::UpdateState, String> {
    let settings = emu::load_settings(data_base);
    if !force && !check_due(settings.updates.checked_at, now_unix()) {
        return Ok(settings.updates);
    }
    let fleet = fleet_at(catalog_path, extra_systems)?;
    let (env, layout) = emu::probe_env();
    let mut items: Vec<UpdateItem> = Vec::new();
    let mut core_baselines: BTreeMap<String, emu::CoreRecord> = BTreeMap::new();
    let mut app_baselines: BTreeMap<String, emu::AppImageRecord> = BTreeMap::new();

    // -- Flatpak channel: RetroArch (when the flatpak flavor is
    // the live one) + any fleet default installed as a flatpak.
    let mut flatpak_ids: BTreeSet<String> = BTreeSet::new();
    if layout
        .as_ref()
        .map(|l| l.flavor == ufm_emu::RaFlavor::Flatpak)
        .unwrap_or(false)
    {
        flatpak_ids.insert(RETROARCH_APP_ID.to_string());
    }
    for item in &fleet {
        if let Some(opt) = option_of(item) {
            if let InstallSpec::Flatpak { app_id } = &opt.install {
                flatpak_ids.insert(app_id.clone());
            }
        }
    }
    if !flatpak_ids.is_empty() {
        let pending = emu::flatpak_pending_updates();
        for id in flatpak_ids.intersection(&pending) {
            let label = if id == RETROARCH_APP_ID {
                "RetroArch".to_string()
            } else {
                fleet
                    .iter()
                    .find(|i| {
                        option_of(i)
                            .map(|o| {
                                matches!(&o.install, InstallSpec::Flatpak { app_id } if app_id == id)
                            })
                            .unwrap_or(false)
                    })
                    .map(|i| i.label.clone())
                    .unwrap_or_else(|| id.clone())
            };
            items.push(UpdateItem {
                key: format!("flatpak:{id}"),
                label,
                detail: "A newer version is waiting on Flathub.".to_string(),
            });
        }
    }

    // -- Buildbot channel: fleet cores on disk vs the zip headers.
    for item in &fleet {
        let Some(opt) = option_of(item) else { continue };
        if opt.kind != OptionKind::LibretroCore {
            continue;
        }
        let Some(core) = &opt.core_file else { continue };
        if !env.cores.contains(core) {
            continue;
        }
        let Some((len, modified)) = emu::core_remote_facts(core) else {
            continue;
        };
        match settings.core_records.get(core) {
            None => {
                core_baselines.insert(
                    core.clone(),
                    emu::CoreRecord {
                        len,
                        modified,
                        at: now_unix(),
                    },
                );
            }
            Some(rec) => {
                if rec.len != len || rec.modified != modified {
                    items.push(UpdateItem {
                        key: format!("core:{core}"),
                        label: format!(
                            "{} core",
                            opt.core_name.clone().unwrap_or_else(|| core.clone())
                        ),
                        detail: "The buildbot has a newer build of this core."
                            .to_string(),
                    });
                }
            }
        }
    }

    // -- GitHub AppImage channel: the managed file vs the latest
    // release's published digest (asset names carry versions, and
    // the digest is ground truth — a differing file is an update,
    // never a guess).
    for item in &fleet {
        let Some(opt) = option_of(item) else { continue };
        let InstallSpec::GitHubAppImage {
            repo,
            asset_suffix,
            file_name,
        } = &opt.install
        else {
            continue;
        };
        let path = emu::applications_dir().join(file_name);
        if !path.is_file() {
            continue;
        }
        let Ok((_url, digest, asset)) = emu::resolve_github_appimage(repo, asset_suffix)
        else {
            continue;
        };
        match &digest {
            Some(d) => match emu::sha256_file(&path) {
                Some(h) if h == *d => {
                    if !settings.app_records.contains_key(file_name) {
                        app_baselines.insert(
                            file_name.clone(),
                            emu::AppImageRecord {
                                asset,
                                sha256: d.clone(),
                                at: now_unix(),
                            },
                        );
                    }
                }
                Some(_) => items.push(UpdateItem {
                    key: format!("appimage:{file_name}"),
                    label: opt.label.clone(),
                    detail: format!("A newer release ({asset}) is available."),
                }),
                None => {}
            },
            None => match settings.app_records.get(file_name) {
                Some(rec) if rec.asset != asset => items.push(UpdateItem {
                    key: format!("appimage:{file_name}"),
                    label: opt.label.clone(),
                    detail: format!("A newer release ({asset}) is available."),
                }),
                Some(_) => {}
                None => {
                    app_baselines.insert(
                        file_name.clone(),
                        emu::AppImageRecord {
                            asset,
                            sha256: String::new(),
                            at: now_unix(),
                        },
                    );
                }
            },
        }
    }

    // -- ES-DE: the recorded version vs the registry's newest.
    let esde = crate::emu_esde::esde_state(data_base);
    if esde.installed && !settings.esde_version.is_empty() {
        if let Ok((_url, _sha, version)) = crate::emu_esde::resolve_esde() {
            if version != settings.esde_version {
                items.push(UpdateItem {
                    key: "esde".to_string(),
                    label: "ES-DE".to_string(),
                    detail: format!(
                        "ES-DE {version} is out — you have {}.",
                        settings.esde_version
                    ),
                });
            }
        }
    }

    let state = emu::UpdateState {
        checked_at: now_unix(),
        items,
    };
    let persisted = state.clone();
    mutate_settings(data_base, |s| {
        for (k, v) in core_baselines {
            s.core_records.insert(k, v);
        }
        for (k, v) in app_baselines {
            s.app_records.insert(k, v);
        }
        s.updates = persisted;
    })?;
    Ok(state)
}

// ---------------------------------------------------------------------------
// Applying (his click only — never the background).
// ---------------------------------------------------------------------------

/// Apply ONE update by its record key, through the channel's own
/// install machinery (idempotent + verified, same as first
/// install). On success the item leaves the updates record; the
/// channel baseline is refreshed so it can't re-flag itself.
pub fn apply_update_at(
    data_base: &Path,
    key: &str,
    progress: &Progress,
) -> Result<(), String> {
    if key == "esde" {
        crate::emu_esde::install_esde(data_base, progress)?;
    } else if let Some(app_id) = key.strip_prefix("flatpak:") {
        emu::flatpak_update(app_id)?;
    } else if let Some(core) = key.strip_prefix("core:") {
        let (_env, layout) = emu::probe_env();
        let layout = layout.ok_or_else(|| {
            "RetroArch isn't installed — there's no cores folder to refresh."
                .to_string()
        })?;
        emu::refetch_core(&layout, core, progress)?;
        if let Some((len, modified)) = emu::core_remote_facts(core) {
            let core = core.to_string();
            mutate_settings(data_base, |s| {
                s.core_records.insert(
                    core,
                    emu::CoreRecord {
                        len,
                        modified,
                        at: now_unix(),
                    },
                );
            })?;
        }
    } else if let Some(file_name) = key.strip_prefix("appimage:") {
        let mut target: Option<(String, String)> = None;
        for sys in ufm_emu::systems_table() {
            for opt in &sys.options {
                if let InstallSpec::GitHubAppImage {
                    repo,
                    asset_suffix,
                    file_name: f,
                } = &opt.install
                {
                    if f == file_name {
                        target = Some((repo.clone(), asset_suffix.clone()));
                    }
                }
            }
        }
        let (repo, suffix) = target
            .ok_or_else(|| format!("No emulator in the table installs {file_name}"))?;
        let (url, digest, asset) = emu::resolve_github_appimage(&repo, &suffix)?;
        emu::install_appimage(
            &url,
            digest.as_deref(),
            file_name,
            "the emulator",
            progress,
        )?;
        let file_name = file_name.to_string();
        mutate_settings(data_base, |s| {
            s.app_records.insert(
                file_name,
                emu::AppImageRecord {
                    asset,
                    sha256: digest.unwrap_or_default(),
                    at: now_unix(),
                },
            );
        })?;
    } else {
        return Err(format!("Unknown update key {key}"));
    }
    let key = key.to_string();
    mutate_settings(data_base, |s| {
        s.updates.items.retain(|i| i.key != key);
    })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Spawns (keeper-pattern single flight) + commands + prefs wires.
// ---------------------------------------------------------------------------

fn emit_progress(app: &tauri::AppHandle) -> impl Fn(&str, Option<u32>) + Send + Sync {
    let app = app.clone();
    move |label: &str, pct: Option<u32>| {
        let _ = app.emit(
            "emu-install-progress",
            serde_json::json!({ "label": label, "pct": pct }),
        );
    }
}

/// Fire-and-forget prep pass (app start, identify completion).
/// Guarded by the same atomic the badge reads: one pass at a
/// time; a skipped pass heals at the next trigger because every
/// pass is a full probe of the fleet.
pub fn spawn_prep(app: tauri::AppHandle) {
    spawn_prep_chain(app, false);
}

/// App start: the prep pass, then — throttle permitting — the
/// daily update check, serialized behind the same guard so the
/// two background jobs never race a settings write.
pub fn spawn_prep_and_check(app: tauri::AppHandle) {
    spawn_prep_chain(app, true);
}

fn spawn_prep_chain(app: tauri::AppHandle, then_check: bool) {
    {
        let state = app.state::<AppState>();
        if state.emu_prep_running.swap(true, Ordering::SeqCst) {
            return;
        }
    }
    tauri::async_runtime::spawn_blocking(move || {
        let (catalog_path, data_base) = {
            let state = app.state::<AppState>();
            (state.catalog_path.clone(), state.data_base.clone())
        };
        let progress = emit_progress(&app);
        let app_notify = app.clone();
        let notify = move || {
            let _ = app_notify.emit(FLEET_CHANGED_EVENT, ());
        };
        match run_prep_pass_at(
            &catalog_path,
            &data_base,
            &BTreeSet::new(),
            &progress,
            &notify,
        ) {
            Ok(r) => tracing::info!(
                target: "ufm",
                "emu prep: {} fleet items, {} ready, {} installed, {} failed",
                r.total,
                r.ready,
                r.installed,
                r.failed.len()
            ),
            Err(e) => tracing::warn!(target: "ufm", "emu prep pass failed: {e}"),
        }
        if then_check {
            match run_update_check_at(
                &catalog_path,
                &data_base,
                &BTreeSet::new(),
                false,
            ) {
                Ok(u) => tracing::info!(
                    target: "ufm",
                    "emu update check: {} waiting",
                    u.items.len()
                ),
                Err(e) => {
                    tracing::warn!(target: "ufm", "emu update check failed: {e}")
                }
            }
        }
        let state = app.state::<AppState>();
        state.emu_prep_running.store(false, Ordering::SeqCst);
        let _ = app.emit(FLEET_CHANGED_EVENT, ());
    });
}

/// The ROMs-door pill's whole data: persisted findings + whether
/// a prep pass is running right now. Cheap on purpose — the door
/// refreshes on every catalog change.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetBadgeWire {
    pub updates: usize,
    pub prepping: bool,
}

#[tauri::command]
pub fn emu_fleet_badge(state: tauri::State<'_, AppState>) -> Cmd<FleetBadgeWire> {
    let settings = emu::load_settings(&state.data_base);
    Ok(FleetBadgeWire {
        updates: settings.updates.items.len(),
        prepping: state.emu_prep_running.load(Ordering::SeqCst),
    })
}

#[tauri::command]
pub async fn emu_apply_update(app: tauri::AppHandle, key: String) -> Cmd<()> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let data_base = state.data_base.clone();
        let progress = emit_progress(&app);
        apply_update_at(&data_base, &key, &progress)
            .map_err(|e| CommandError::from(UfmError::Internal(e)))?;
        let _ = app.emit(FLEET_CHANGED_EVENT, ());
        Ok(())
    })
    .await
    .map_err(|e| {
        CommandError::from(UfmError::Internal(format!("apply task failed: {e}")))
    })?
}

/// Apply every waiting update, one at a time, in record order.
/// Returns the failures (empty = all applied) — items that failed
/// stay on the record, so the card keeps showing them honestly.
#[tauri::command]
pub async fn emu_apply_all_updates(app: tauri::AppHandle) -> Cmd<Vec<String>> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let data_base = state.data_base.clone();
        let keys: Vec<String> = emu::load_settings(&data_base)
            .updates
            .items
            .iter()
            .map(|i| i.key.clone())
            .collect();
        let progress = emit_progress(&app);
        let mut failures = Vec::new();
        for key in keys {
            if let Err(e) = apply_update_at(&data_base, &key, &progress) {
                failures.push(format!("{key}: {e}"));
            }
        }
        let _ = app.emit(FLEET_CHANGED_EVENT, ());
        Ok(failures)
    })
    .await
    .map_err(|e| {
        CommandError::from(UfmError::Internal(format!("apply-all task failed: {e}")))
    })?
}

// ---------------------------------------------------------------------------
// Preferences wires (emu_farm::emu_prefs embeds these).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepFailWire {
    pub label: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepWire {
    pub running: bool,
    /// Unix seconds of the last completed pass (0 = never).
    pub last_run: i64,
    pub total: usize,
    /// Fleet items the live probe calls Ready right now.
    pub ready: usize,
    /// Items the last pass failed on that still aren't Ready.
    pub failed: Vec<PrepFailWire>,
}

pub fn prep_wire(state: &AppState, settings: &EmuSettings) -> PrepWire {
    let fleet = fleet_at(&state.catalog_path, &BTreeSet::new()).unwrap_or_default();
    let (env, layout) = emu::probe_env();
    let mut ready = 0;
    let mut failed = Vec::new();
    for item in &fleet {
        let is_ready = if item.key == "retroarch" {
            layout.is_some()
        } else {
            option_of(item)
                .map(|o| ufm_emu::option_state(o, &env) == OptState::Ready)
                .unwrap_or(false)
        };
        if is_ready {
            ready += 1;
        } else if let Some(rec) = settings.prep.items.get(&item.key) {
            if rec.state == "failed" {
                failed.push(PrepFailWire {
                    label: item.label.clone(),
                    error: rec.error.clone(),
                });
            }
        }
    }
    PrepWire {
        running: state.emu_prep_running.load(Ordering::SeqCst),
        last_run: settings.prep.last_run,
        total: fleet.len(),
        ready,
        failed,
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdatesWire {
    pub checked_at: i64,
    pub items: Vec<UpdateItem>,
}

pub fn updates_wire(settings: &EmuSettings) -> UpdatesWire {
    UpdatesWire {
        checked_at: settings.updates.checked_at,
        items: settings.updates.items.clone(),
    }
}

// ---------------------------------------------------------------------------
// Tests — pure fleet math against the REAL table (house style).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn systems(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn fleet_for_nes_snes_is_retroarch_plus_two_cores() {
        let fleet = compute_fleet(ufm_emu::systems_table(), &systems(&["nes", "snes"]));
        assert_eq!(fleet.len(), 3, "fleet: {fleet:?}");
        assert_eq!(fleet[0].key, "retroarch");
        let labels: Vec<&str> = fleet.iter().map(|i| i.label.as_str()).collect();
        assert!(
            labels.iter().any(|l| l.contains("Mesen")),
            "labels: {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains("Snes9x")),
            "labels: {labels:?}"
        );
    }

    #[test]
    fn fleet_for_no_games_is_empty() {
        assert!(compute_fleet(ufm_emu::systems_table(), &BTreeSet::new()).is_empty());
    }

    #[test]
    fn fleet_for_ps3_is_the_github_appimage_only() {
        // PS3's default is RPCS3 (GitHub AppImage channel): a fleet
        // member with no RetroArch item in front of it.
        let fleet = compute_fleet(ufm_emu::systems_table(), &systems(&["ps3"]));
        assert_eq!(fleet.len(), 1, "fleet: {fleet:?}");
        assert_eq!(fleet[0].system_id.as_deref(), Some("ps3"));
        let opt = option_of(&fleet[0]).expect("option resolves");
        assert!(matches!(
            opt.install,
            InstallSpec::GitHubAppImage { .. }
        ));
    }

    #[test]
    fn check_due_respects_the_daily_throttle() {
        assert!(check_due(0, 1_700_000_000));
        assert!(!check_due(1_700_000_000, 1_700_000_000 + 3_600));
        assert!(check_due(1_700_000_000, 1_700_000_000 + DAY_SECS));
    }
}

// ---------------------------------------------------------------------------
// Acceptance — env-gated, against the man's real box (house
// pattern, cf. emu.rs's acceptance mod). The driver renames the
// artifacts aside; these tests drive the REAL engines.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod acceptance {
    use super::*;

    fn env_paths() -> (std::path::PathBuf, std::path::PathBuf) {
        (
            std::path::PathBuf::from(
                std::env::var("FABLE_EMU_CATALOG").expect("FABLE_EMU_CATALOG"),
            ),
            std::path::PathBuf::from(
                std::env::var("FABLE_EMU_DATA_BASE").expect("FABLE_EMU_DATA_BASE"),
            ),
        )
    }

    /// v2.1 proof 1: with mesen_libretro.so and RPCS3.AppImage
    /// renamed aside by the driver, the real prep engine — fleet
    /// extended to ps3 via FABLE_EMU_PREP_EXTRA_SYSTEMS — restores
    /// both, unattended, through the same channels Play uses, and
    /// the pre-existing fleet (Snes9x) verifies without a fetch.
    #[test]
    #[ignore]
    fn prep_restore_acceptance() {
        if std::env::var("FABLE_EMU_ACCEPT").is_err() {
            return;
        }
        let (catalog_path, data_base) = env_paths();
        let extra: BTreeSet<String> = std::env::var("FABLE_EMU_PREP_EXTRA_SYSTEMS")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
        let progress = |label: &str, pct: Option<u32>| {
            eprintln!("[prep] {label} {pct:?}");
        };
        let notify = || {};
        let report = run_prep_pass_at(
            &catalog_path,
            &data_base,
            &extra,
            &progress,
            &notify,
        )
        .expect("prep pass runs");
        eprintln!(
            "PREP REPORT: total={} ready={} installed={} failed={:?}",
            report.total, report.ready, report.installed, report.failed
        );
        assert!(report.failed.is_empty(), "prep failures: {:?}", report.failed);
        let (_env, layout) = emu::probe_env();
        let layout = layout.expect("retroarch layout after prep");
        assert!(
            layout.cores_dir.join("mesen_libretro.so").is_file(),
            "mesen core restored by the pass"
        );
        assert!(
            emu::applications_dir().join("RPCS3.AppImage").is_file(),
            "RPCS3 AppImage restored by the pass"
        );
        let settings = emu::load_settings(&data_base);
        eprintln!(
            "PREP RECORD: last_run={} items={:?}",
            settings.prep.last_run,
            settings.prep.items.keys().collect::<Vec<_>>()
        );
        assert!(settings.prep.last_run > 0, "pass checkpointed");
    }

    /// v2.1 proof 2: the driver doctors the mesen core record
    /// stale; a forced check must surface it on the updates
    /// record, and the real applier must refetch the core and
    /// clear the item.
    #[test]
    #[ignore]
    fn core_update_acceptance() {
        if std::env::var("FABLE_EMU_ACCEPT").is_err() {
            return;
        }
        let (catalog_path, data_base) = env_paths();
        let state = run_update_check_at(
            &catalog_path,
            &data_base,
            &BTreeSet::new(),
            true,
        )
        .expect("update check runs");
        eprintln!(
            "UPDATE ITEMS: {:?}",
            state.items.iter().map(|i| &i.key).collect::<Vec<_>>()
        );
        assert!(
            state
                .items
                .iter()
                .any(|i| i.key == "core:mesen_libretro.so"),
            "the doctored stale record must surface: {:?}",
            state.items
        );
        let progress = |label: &str, pct: Option<u32>| {
            eprintln!("[apply] {label} {pct:?}");
        };
        apply_update_at(&data_base, "core:mesen_libretro.so", &progress)
            .expect("apply refetches the core");
        let settings = emu::load_settings(&data_base);
        assert!(
            !settings
                .updates
                .items
                .iter()
                .any(|i| i.key == "core:mesen_libretro.so"),
            "applied item leaves the record"
        );
        let (len, modified) =
            emu::core_remote_facts("mesen_libretro.so").expect("buildbot facts");
        let rec = settings
            .core_records
            .get("mesen_libretro.so")
            .expect("record refreshed");
        assert_eq!((rec.len, rec.modified.as_str()), (len, modified.as_str()));
        eprintln!("CORE UPDATE APPLIED: record now matches the buildbot");
    }
}
