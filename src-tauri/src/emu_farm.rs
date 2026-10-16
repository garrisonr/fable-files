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
use ufm_emu::{DesiredLink, FarmCopy, FarmGame, FarmOp, M3uOp};

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
    /// Multi-file game sets represented as folders (§5.3).
    pub sets: usize,
    /// Multi-disc playlist (.m3u) files written this pass.
    pub m3us_written: usize,
    /// Multi-disc playlist (.m3u) files removed this pass.
    pub m3us_removed: usize,
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
///
/// THE VIEW IS NEVER A SOURCE (v2 fix, proven by ES-DE): the
/// catalog also holds rows for the farm's own files (the indexer
/// walks the home tree the farm lives in). A farm row is the
/// mirror's reflection, not a copy — and because it sits on the
/// internal disk it OUTRANKS the real copies on removable
/// drives, so an unfiltered keeper pointed one link per such
/// game at the link's own location (1,047 self-loops on the
/// man's box, ELOOP for every outside reader). Rows under the
/// view root never become copies here.
fn compute_games(
    catalog: &ufm_catalog::Catalog,
    mounted: &[crate::commands::MountedVolume],
    view_root: &Path,
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
        if Path::new(&pk).starts_with(view_root) {
            continue;
        }
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
            path: pk.clone(),
            present,
            rank,
            // Multi-file set membership (§5.3): only present copies
            // get scanned — an absent copy's folder shape can't be
            // read, and the farm only represents what can open.
            set_files: if present {
                scan_set_files(std::path::Path::new(&pk))
            } else {
                Vec::new()
            },
        });
    }
    Ok((by_identity.into_values().collect(), managed))
}

/// Launch-file extensions that can head a multi-file set.
const SET_LAUNCH_EXTS: [&str; 4] = ["cue", "gdi", "ccd", "m3u"];
/// Extensions that mark a same-stem sibling as a set member.
const SET_DATA_EXTS: [&str; 7] = ["bin", "iso", "img", "sub", "wav", "mp3", "flac"];

fn lower_ext(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn read_head(path: &Path, cap: usize) -> Option<String> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(cap as u64)
        .read_to_end(&mut buf)
        .ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// The member files of the set a launch file heads (amendment
/// §5.3), read from the launch file itself wherever it declares
/// its members:
///
/// - `.cue`: its `FILE "…"` lines.
/// - `.gdi`: each track line's quoted file name.
/// - `.ccd`: same-stem `.img` / `.sub` siblings.
/// - `.m3u`: the files it lists.
///
/// …with a same-stem sibling sweep (data extensions) as the
/// fallback when a sheet declares nothing readable. Members must
/// sit BESIDE the launch file (flat sets — the universal shape);
/// a reference into a subfolder declines set treatment (the game
/// keeps its plain link, exactly as v1 left it). Returns the
/// member paths INCLUDING the launch file when ≥2 files belong,
/// else an empty vec (a single-file game).
fn scan_set_files(launch: &Path) -> Vec<String> {
    let ext = lower_ext(launch);
    if !SET_LAUNCH_EXTS.contains(&ext.as_str()) {
        return Vec::new();
    }
    let Some(dir) = launch.parent() else {
        return Vec::new();
    };
    let mut members: BTreeSet<std::path::PathBuf> = BTreeSet::new();
    members.insert(launch.to_path_buf());
    match ext.as_str() {
        "cue" | "m3u" => {
            if let Some(text) = read_head(launch, 512 * 1024) {
                for line in text.lines() {
                    let line = line.trim();
                    let name = if ext == "cue" {
                        let lower = line.to_lowercase();
                        if !lower.starts_with("file ") {
                            continue;
                        }
                        match (line.find('"'), line.rfind('"')) {
                            (Some(a), Some(b)) if b > a => line[a + 1..b].to_string(),
                            _ => continue,
                        }
                    } else {
                        if line.is_empty() || line.starts_with('#') {
                            continue;
                        }
                        line.to_string()
                    };
                    // Flat sets only: a subfolder reference means
                    // the set's shape isn't the farm's folder shape.
                    if name.contains('/') || name.contains('\\') {
                        return Vec::new();
                    }
                    let candidate = dir.join(&name);
                    if candidate.is_file() {
                        members.insert(candidate);
                    }
                }
            }
        }
        "gdi" => {
            if let Some(text) = read_head(launch, 64 * 1024) {
                for line in text.lines().skip(1) {
                    let line = line.trim();
                    let name = match (line.find('"'), line.rfind('"')) {
                        (Some(a), Some(b)) if b > a => line[a + 1..b].to_string(),
                        _ => continue,
                    };
                    if name.contains('/') || name.contains('\\') {
                        return Vec::new();
                    }
                    let candidate = dir.join(&name);
                    if candidate.is_file() {
                        members.insert(candidate);
                    }
                }
            }
        }
        _ => {} // "ccd": the sibling sweep below is its rule.
    }
    if members.len() < 2 {
        // Fallback: same-stem data siblings (ccd triples land here
        // too: .ccd + .img + .sub share a stem).
        let stem = launch
            .file_stem()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if !stem.is_empty() {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for e in entries.flatten() {
                    let p = e.path();
                    let same_stem = p
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_lowercase() == stem)
                        .unwrap_or(false);
                    if same_stem && SET_DATA_EXTS.contains(&lower_ext(&p).as_str()) && p.is_file()
                    {
                        members.insert(p);
                    }
                }
            }
        }
    }
    if members.len() < 2 {
        return Vec::new();
    }
    members
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

/// The desired link set + the mounts the targets live on.
fn compute_desired(
    catalog: &ufm_catalog::Catalog,
    mounted: &[crate::commands::MountedVolume],
    view_root: &Path,
) -> Result<(Vec<FarmGame>, Vec<DesiredLink>, BTreeSet<String>, usize), String> {
    let volumes = catalog.volumes().unwrap_or_default();
    let (games, managed) = compute_games(catalog, mounted, view_root)?;
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
/// (rel, target) pairs — top-level links AND the links inside set
/// folders (§5.3; one level down, the folder's children). Real
/// files are invisible here by design — the keeper never touches
/// what it didn't plant.
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
            if md.file_type().is_symlink() {
                if let Ok(target) = std::fs::read_link(&path) {
                    out.push((
                        format!("{}/{}", system, e.file_name().to_string_lossy()),
                        target.to_string_lossy().into_owned(),
                    ));
                }
            } else if md.is_dir() {
                // A set folder: its member links live one level in.
                let folder = e.file_name().to_string_lossy().into_owned();
                if let Ok(children) = std::fs::read_dir(&path) {
                    for c in children.flatten() {
                        let cpath = c.path();
                        let Ok(cmd) = std::fs::symlink_metadata(&cpath) else {
                            continue;
                        };
                        if cmd.file_type().is_symlink() {
                            if let Ok(target) = std::fs::read_link(&cpath) {
                                out.push((
                                    format!(
                                        "{}/{}/{}",
                                        system,
                                        folder,
                                        c.file_name().to_string_lossy()
                                    ),
                                    target.to_string_lossy().into_owned(),
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

/// The marker line every Fable-written .m3u carries — the keeper
/// manages only files that carry it, and never overwrites a file
/// that doesn't (a man's own playlist of the same name stays his).
pub const M3U_MARKER: &str = "# Fable Files — kept in step with your library";

/// The farm's actual Fable-managed .m3u files: (rel, disc lines).
fn read_actual_m3us(root: &Path, managed: &BTreeSet<String>) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    for system in managed {
        let dir = root.join(system);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.to_lowercase().ends_with(".m3u") {
                continue;
            }
            let path = e.path();
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if !text.contains(M3U_MARKER) {
                continue;
            }
            let lines: Vec<String> = text
                .lines()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(|l| l.to_string())
                .collect();
            out.push((format!("{system}/{name}"), lines));
        }
    }
    out
}

/// Remove now-empty set folders in the managed system dirs (a set
/// whose links all retired leaves its folder shell behind).
/// `remove_dir` only succeeds on empty dirs, so a folder holding
/// anything real is untouchable by construction.
fn cleanup_empty_set_folders(root: &Path, managed: &BTreeSet<String>) {
    for system in managed {
        let dir = root.join(system);
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    let _ = std::fs::remove_dir(e.path());
                }
            }
        }
    }
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
    let view_root = settings.view_root_path();
    let (games, desired, managed, drives) =
        match compute_desired(&catalog, &mounted, &view_root) {
        Ok(v) => v,
        Err(e) => {
            report.error = Some(e);
            return report;
        }
    };
    report.identities = games.len();
    report.links = desired.len();
    report.drives = drives;
    // Multi-disc groups (§5.3): computed once, used by both the
    // farm's .m3u files and the RetroArch playlist entries.
    let desired_m3us = ufm_emu::desired_m3us(&games);

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
                    // Sets among the desired links (folders with
                    // member links inside).
                    {
                        let mut folders = BTreeSet::new();
                        for l in &desired {
                            let mut parts = l.rel.splitn(3, '/');
                            if let (Some(s), Some(f), Some(_)) =
                                (parts.next(), parts.next(), parts.next())
                            {
                                folders.insert(format!("{s}/{f}"));
                            }
                        }
                        report.sets = folders.len();
                    }
                    let actual = read_actual(&root, &managed);
                    // Application order matters around sets: removes
                    // first (a stale plain link vacates the name a
                    // set folder needs, and vice versa), then
                    // repoints, then creates. Any case the ordering
                    // can't settle in one pass heals on the next —
                    // every pass recomputes the full delta.
                    let ops = ufm_emu::farm_delta(&desired, &actual);
                    let ordered = ops.iter().filter(|o| matches!(o, FarmOp::Remove { .. }))
                        .chain(ops.iter().filter(|o| matches!(o, FarmOp::Repoint { .. })))
                        .chain(ops.iter().filter(|o| matches!(o, FarmOp::Create { .. })));
                    cleanup_empty_set_folders(&root, &managed);
                    for op in ordered {
                        match op {
                            FarmOp::Create { rel, target } => {
                                let rel = rel.clone();
                                let path = root.join(&rel);
                                if let Some(parent) = path.parent() {
                                    let _ = std::fs::create_dir_all(parent);
                                }
                                if path.exists() || std::fs::symlink_metadata(&path).is_ok() {
                                    // Something real sits there —
                                    // never overwrite a user's file.
                                    report.blocked.push(rel);
                                } else if std::os::unix::fs::symlink(target, &path).is_ok() {
                                    report.created += 1;
                                } else {
                                    report.blocked.push(rel);
                                }
                            }
                            FarmOp::Repoint { rel, target } => {
                                let path = root.join(rel);
                                let _ = std::fs::remove_file(&path);
                                if std::os::unix::fs::symlink(target, &path).is_ok() {
                                    report.repointed += 1;
                                } else {
                                    report.blocked.push(rel.clone());
                                }
                            }
                            FarmOp::Remove { rel } => {
                                if std::fs::remove_file(root.join(rel)).is_ok() {
                                    report.removed += 1;
                                }
                            }
                        }
                    }
                    cleanup_empty_set_folders(&root, &managed);

                    // --- multi-disc playlists (§5.3) ---------------
                    let actual_m3us = read_actual_m3us(&root, &managed);
                    let managed_m3u_rels: BTreeSet<&str> =
                        actual_m3us.iter().map(|(rel, _)| rel.as_str()).collect();
                    for op in ufm_emu::m3u_delta(&desired_m3us, &actual_m3us) {
                        match op {
                            M3uOp::Write { rel, lines } => {
                                let path = root.join(&rel);
                                if path.exists() && !managed_m3u_rels.contains(rel.as_str()) {
                                    // A file Fable didn't write
                                    // holds the name — never
                                    // overwrite what's his.
                                    report.blocked.push(rel);
                                    continue;
                                }
                                let mut text = String::from("#EXTM3U\n");
                                text.push_str(M3U_MARKER);
                                text.push('\n');
                                for line in &lines {
                                    text.push_str(line);
                                    text.push('\n');
                                }
                                let tmp = path.with_extension("m3u.part");
                                if std::fs::write(&tmp, &text).is_ok()
                                    && std::fs::rename(&tmp, &path).is_ok()
                                {
                                    report.m3us_written += 1;
                                } else {
                                    let _ = std::fs::remove_file(&tmp);
                                    report.blocked.push(rel);
                                }
                            }
                            M3uOp::Remove { rel } => {
                                if std::fs::remove_file(root.join(&rel)).is_ok() {
                                    report.m3us_removed += 1;
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
            // identity -> its launch target: the best present
            // copy's own path (for a set, its launch file — the
            // real cue, never a farm path).
            let targets: BTreeMap<String, String> = games
                .iter()
                .filter_map(|g| {
                    g.copies
                        .iter()
                        .filter(|c| c.present)
                        .min_by(|a, b| a.rank.cmp(&b.rank).then_with(|| a.path.cmp(&b.path)))
                        .map(|c| (g.identity.clone(), c.path.clone()))
                })
                .collect();
            // identity -> its multi-disc group, for this pass.
            let group_of: BTreeMap<&str, &ufm_emu::DesiredM3u> = desired_m3us
                .iter()
                .flat_map(|m| m.identities.iter().map(move |i| (i.as_str(), m)))
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
                // Entries in shelf order. A multi-disc group
                // collapses to ONE entry pointing at the farm's
                // .m3u — the handle RetroArch swaps discs through
                // (§3(d): the .m3u is an alias, and the one place a
                // playlist carries a farm path); single games keep
                // their real paths, as always.
                let mut entries: Vec<ufm_emu::PlaylistEntry> = Vec::new();
                let mut emitted_groups: BTreeSet<&str> = BTreeSet::new();
                for g in games.iter().filter(|g| &g.system == system) {
                    if let Some(m) = group_of.get(g.identity.as_str()) {
                        if emitted_groups.insert(m.rel.as_str()) {
                            entries.push(ufm_emu::PlaylistEntry {
                                path: root
                                    .join(&m.rel)
                                    .to_string_lossy()
                                    .into_owned(),
                                label: m.title.clone(),
                                core_path: core_path.clone(),
                                core_name: core_name.clone(),
                            });
                        }
                        continue;
                    }
                    if let Some(t) = targets.get(&g.identity) {
                        entries.push(ufm_emu::PlaylistEntry {
                            path: t.clone(),
                            label: titles
                                .get(&g.identity)
                                .cloned()
                                .unwrap_or_else(|| g.title.clone()),
                            core_path: core_path.clone(),
                            core_name: core_name.clone(),
                        });
                    }
                }
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
    /// The System files (BIOS) card: every requirement system's
    /// state, whether he owns games for it or not (§6).
    pub bios: Vec<crate::emu::BiosSystemWire>,
    /// The ES-DE companion card (v2): installed? which version?
    pub esde: crate::emu_esde::EsDeStateWire,
    /// Sandbox drive-access grants Fable applied with his
    /// consent (v2, §4.4) — visible here, removable here.
    pub overrides: Vec<OverrideWire>,
}

/// One recorded sandbox grant, for the Preferences list.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverrideWire {
    pub app_id: String,
    /// The emulator's friendly name when the table knows the app.
    pub app_label: String,
    pub filesystem: String,
}

/// Resolve the recorded grants to display rows (friendly names
/// come from the table — the app id is the durable record).
pub fn override_wires(settings: &EmuSettings) -> Vec<OverrideWire> {
    settings
        .overrides
        .iter()
        .map(|o| {
            let label = ufm_emu::systems_table()
                .iter()
                .flat_map(|s| s.options.iter())
                .find(|opt| {
                    matches!(&opt.install, ufm_emu::InstallSpec::Flatpak { app_id } if app_id == &o.app_id)
                })
                .map(|opt| opt.label.clone())
                .unwrap_or_else(|| o.app_id.clone());
            OverrideWire {
                app_id: o.app_id.clone(),
                app_label: label,
                filesystem: o.filesystem.clone(),
            }
        })
        .collect()
}

fn view_state(state: &AppState, settings: &EmuSettings, report: KeeperReport) -> ViewStateWire {
    let root = settings.view_root_path();
    let links_on_disk = {
        let catalog = ufm_catalog::Catalog::open(&state.catalog_path).ok();
        match catalog {
            Some(cat) => {
                let mounted = crate::commands::current_filesystems();
                compute_games(&cat, &mounted, &root)
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
                        let st = state_of(sys, o, &env, layout.as_ref());
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
            bios: crate::emu::bios_state_wires(layout.as_ref(), &settings),
            esde: crate::emu_esde::esde_state(&state.data_base),
            overrides: override_wires(&settings),
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

#[cfg(test)]
mod set_scan_tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fable-set-scan-{}-{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn cue_declares_its_members() {
        let dir = scratch("cue");
        std::fs::write(dir.join("Game.bin"), b"x").unwrap();
        let cue = dir.join("Game.cue");
        std::fs::write(&cue, "FILE \"Game.bin\" BINARY\n  TRACK 01 MODE2/2352\n").unwrap();
        let members = scan_set_files(&cue);
        assert_eq!(members.len(), 2);
        assert!(members.iter().any(|m| m.ends_with("Game.cue")));
        assert!(members.iter().any(|m| m.ends_with("Game.bin")));
        // A bin referenced but absent isn't invented.
        let cue2 = dir.join("Ghost.cue");
        std::fs::write(dir.join("Ghost.bin"), b"x").unwrap();
        std::fs::write(
            &cue2,
            "FILE \"Ghost.bin\" BINARY\nFILE \"Missing.bin\" BINARY\n",
        )
        .unwrap();
        assert_eq!(scan_set_files(&cue2).len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn subfolder_references_decline_set_treatment() {
        let dir = scratch("sub");
        std::fs::create_dir_all(dir.join("tracks")).unwrap();
        std::fs::write(dir.join("tracks/t01.bin"), b"x").unwrap();
        let cue = dir.join("Game.cue");
        std::fs::write(&cue, "FILE \"tracks/t01.bin\" BINARY\n").unwrap();
        assert!(scan_set_files(&cue).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ccd_triples_come_from_the_sibling_sweep() {
        let dir = scratch("ccd");
        for name in ["Game.ccd", "Game.img", "Game.sub", "Other.img"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        let members = scan_set_files(&dir.join("Game.ccd"));
        assert_eq!(members.len(), 3);
        // A lone cue with no members and no siblings stays single.
        let lone = dir.join("Lone.cue");
        std::fs::write(&lone, "REM nothing here\n").unwrap();
        assert!(scan_set_files(&lone).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gdi_and_m3u_declarations_read() {
        let dir = scratch("gdi");
        std::fs::write(dir.join("track01.bin"), b"x").unwrap();
        std::fs::write(dir.join("track03.bin"), b"x").unwrap();
        let gdi = dir.join("Game.gdi");
        std::fs::write(
            &gdi,
            "2\n1 0 4 \"track01.bin\" 0\n3 45000 4 \"track03.bin\" 0\n",
        )
        .unwrap();
        assert_eq!(scan_set_files(&gdi).len(), 3);
        let m3u = dir.join("Saga.m3u");
        std::fs::write(dir.join("Saga (Disc 1).cue"), b"x").unwrap();
        std::fs::write(dir.join("Saga (Disc 2).cue"), b"x").unwrap();
        std::fs::write(&m3u, "#EXTM3U\nSaga (Disc 1).cue\nSaga (Disc 2).cue\n").unwrap();
        assert_eq!(scan_set_files(&m3u).len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cartridges_are_never_sets() {
        let dir = scratch("cart");
        let rom = dir.join("Game.gb");
        std::fs::write(&rom, b"x").unwrap();
        std::fs::write(dir.join("Game.bin"), b"x").unwrap();
        assert!(scan_set_files(&rom).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
