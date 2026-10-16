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
    /// Absolute root path when the backend knows it (Round 7.2: mounted
    /// volumes carry it so the shell can eject by path). None for a
    /// network server that isn't connected right now.
    pub path: Option<String>,
    /// Removable media (USB / SD card) — the sidebar offers eject.
    pub removable: bool,
    /// Network Locations: the normalized server URI for
    /// provider="network" rows (never carries a password).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// False only for a bookmarked network server that isn't
    /// currently connected. Unlike an unplugged volume (which leaves
    /// the list), a server bookmark stays listed offline — Library
    /// Memory semantics — with Connect as its action.
    pub online: bool,
}

// ---------------------------------------------------------------------------
// Mounted volumes (Round 7.2): the sidebar Locations list used to be only
// the persisted set — a freshly plugged drive never appeared. Volumes are
// now enumerated from lsblk on every list_locations call and merged in as
// provider="volume" runtime locations (registered in the browse maps so
// list_dir resolves them, never written to the catalog or the sidecar).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct MountedVolume {
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) removable: bool,
    /// Indexer v2: the filesystem UUID (lsblk UUID) — the volume's
    /// durable identity for Library Memory. None when lsblk reports
    /// none (rare; such volumes simply aren't remembered).
    pub(crate) uuid: Option<String>,
    /// The raw filesystem label, when the volume has one.
    pub(crate) label: Option<String>,
}

/// One mounted-filesystem candidate collected from the lsblk tree.
struct MountCandidate {
    device: String,
    mountpoint: String,
    label: Option<String>,
    /// GPT partition label — the device's own name when it has one.
    partlabel: Option<String>,
    /// Ancestor disk's model string (inherited down the tree, the
    /// way rm/tran are) — the human name of last resort before ids.
    model: Option<String>,
    removable: bool,
    /// Filesystem UUID of THIS node (never inherited — a partition's
    /// UUID is its own; the LUKS mapper node carries the fs UUID).
    uuid: Option<String>,
}

fn collect_mounts(
    node: &serde_json::Value,
    inherited_rm: bool,
    inherited_usb: bool,
    under_loop: bool,
    inherited_model: Option<String>,
    out: &mut Vec<MountCandidate>,
) {
    let ty = node.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let rm = node
        .get("rm")
        .and_then(|v| v.as_bool())
        .unwrap_or(inherited_rm);
    let usb = node.get("tran").and_then(|v| v.as_str()) == Some("usb") || inherited_usb;
    let model = node
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or(inherited_model);
    let loopish = under_loop || ty == "loop" || ty == "rom";
    let fstype = node.get("fstype").and_then(|v| v.as_str()).unwrap_or("");
    if !loopish && fstype != "squashfs" {
        if let Some(points) = node.get("mountpoints").and_then(|v| v.as_array()) {
            for mp in points.iter().filter_map(|v| v.as_str()) {
                out.push(MountCandidate {
                    device: node
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    mountpoint: mp.to_string(),
                    label: node
                        .get("label")
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty()),
                    partlabel: node
                        .get("partlabel")
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty()),
                    model: model.clone(),
                    removable: rm || usb,
                    uuid: node
                        .get("uuid")
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty()),
                });
            }
        }
    }
    if let Some(children) = node.get("children").and_then(|v| v.as_array()) {
        for child in children {
            collect_mounts(child, rm, usb, loopish, model.clone(), out);
        }
    }
}

/// Every mounted filesystem lsblk can see, one entry per device —
/// INCLUDING the system mounts ("/", "/boot") that the sidebar hides.
/// Library Memory (Indexer v2) works from this full set: the root
/// filesystem is a volume too (Home lives on it), it just never
/// appears in the sidebar. Pseudo mounts (/snap, /sys, /proc, /dev)
/// are excluded here for everyone.
fn collect_filesystems() -> Vec<MountCandidate> {
    let out = match std::process::Command::new("lsblk")
        .args([
            "-J",
            "-o",
            "NAME,TYPE,RM,TRAN,MOUNTPOINTS,LABEL,PARTLABEL,MODEL,FSTYPE,UUID",
        ])
        .output()
    {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    let parsed: serde_json::Value = match serde_json::from_slice(&out.stdout) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut candidates = Vec::new();
    if let Some(list) = parsed.get("blockdevices").and_then(|v| v.as_array()) {
        for d in list {
            collect_mounts(d, false, false, false, None, &mut candidates);
        }
    }
    candidates.retain(|c| {
        let mp = c.mountpoint.as_str();
        !mp.starts_with("/snap/")
            && !mp.starts_with("/var/lib/snapd")
            && !mp.starts_with("/sys")
            && !mp.starts_with("/proc")
            && !mp.starts_with("/dev")
    });
    // One entry per device: prefer the top-level (shortest) mountpoint,
    // which collapses btrfs subvolume duplicates of the same device.
    let mut by_device: std::collections::HashMap<String, MountCandidate> =
        std::collections::HashMap::new();
    for c in candidates {
        match by_device.get(&c.device) {
            Some(existing)
                if existing.mountpoint.matches('/').count()
                    <= c.mountpoint.matches('/').count() => {}
            _ => {
                by_device.insert(c.device.clone(), c);
            }
        }
    }
    by_device.into_values().collect()
}

/// Display-name chain (UI round, Dickles' ruling): the volume LABEL
/// first, then the device / partition name, then the disk's model,
/// and only as a last resort a SHORTENED id — a raw 36-char UUID in
/// the sidebar (his LUKS superdog) names nothing a human recognizes.
fn volume_display_name(c: &MountCandidate) -> String {
    c.label
        .clone()
        .or(c.partlabel.clone())
        .or(c.model.clone())
        .unwrap_or_else(|| {
            let base = std::path::Path::new(&c.mountpoint)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| c.mountpoint.clone());
            if base.chars().count() > 12 {
                format!("{}…", base.chars().take(8).collect::<String>())
            } else {
                base
            }
        })
}

fn mounted_volume_of(c: MountCandidate) -> MountedVolume {
    MountedVolume {
        name: volume_display_name(&c),
        path: c.mountpoint.clone(),
        removable: c.removable,
        uuid: c.uuid.clone(),
        label: c.label.clone(),
    }
}

/// All currently mounted filesystems (system mounts included) —
/// the live truth Library Memory badges and retention check against.
pub(crate) fn current_filesystems() -> Vec<MountedVolume> {
    collect_filesystems().into_iter().map(mounted_volume_of).collect()
}

fn enumerate_mounted_volumes() -> Vec<MountedVolume> {
    // The sidebar set: browsable volumes only, no system mounts.
    let mut volumes: Vec<MountedVolume> = collect_filesystems()
        .into_iter()
        .filter(|c| {
            let mp = c.mountpoint.as_str();
            mp != "/" && mp != "/boot" && mp != "/boot/efi"
        })
        .map(mounted_volume_of)
        .collect();
    volumes.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    volumes
}

// ---------------------------------------------------------------------------
// Library Memory (Indexer v2): volume sightings + item -> volume badges.
// A volume's catalog STAYS when it disconnects; these helpers are how
// the rest of the app knows which volume an item lives on and whether
// that volume is here right now.
// ---------------------------------------------------------------------------

/// Record every currently-mounted filesystem in the catalog's
/// volumes table (creating records on first sight). Returns the
/// mounted set so callers can resolve online state without a second
/// enumeration.
pub(crate) fn record_volume_sightings(catalog: &ufm_catalog::Catalog) -> Vec<MountedVolume> {
    let mounted = current_filesystems();
    let now = chrono::Utc::now().to_rfc3339();
    for v in &mounted {
        if let Some(uuid) = &v.uuid {
            let label = v.label.as_deref().unwrap_or(v.name.as_str());
            let _ = catalog.record_volume_sighting(uuid, Some(label), &v.path, &now);
        }
    }
    mounted
}

/// True when `path` is `prefix` itself or lives under it
/// (prefix-boundary aware; "/" contains every absolute path).
pub(crate) fn path_under(path: &str, prefix: &str) -> bool {
    if prefix == "/" {
        return path.starts_with('/');
    }
    path == prefix || path.starts_with(&format!("{prefix}/"))
}

/// Which volume an item path lives on, and whether it's connected.
#[derive(Debug, Clone)]
pub(crate) struct VolumeBadge {
    pub label: String,
    pub offline: bool,
    /// RFC 3339 — when the volume was last seen mounted. Set for
    /// offline badges (and online ones whose record exists).
    pub last_seen: Option<String>,
}

/// Resolve an item's absolute path to its volume: ONE contest
/// across live mounts AND remembered mount history — the longest
/// claim wins, and a live claim beats a history claim of the same
/// length. None = the path is on no known volume (cloud /
/// network locations, paths never indexed from a real volume).
///
/// The two-stage version this replaced asked live mounts FIRST and
/// only consulted history when no live mount claimed the path —
/// but "/" is always a live mount and claims every absolute path,
/// so an unplugged drive's items badged ONLINE wearing the
/// internal volume's identity, and the history stage never ran.
/// (Emulation v1.1 probe: the same "/" catch-all shape the v1
/// checkpoint flagged; `emu::home_volume` already carries this
/// one-contest discipline for the same reason.)
pub(crate) fn volume_badge_for_path<'a>(
    mounted: &'a [MountedVolume],
    volumes: &'a [ufm_catalog::VolumeRecord],
    path: &str,
) -> Option<VolumeBadge> {
    enum Claim<'a> {
        Live(&'a MountedVolume),
        History(&'a ufm_catalog::VolumeRecord),
    }
    let mut best: Option<(usize, Claim<'a>)> = None;
    let mut consider = |len: usize, claim: Claim<'a>| {
        let better = match &best {
            None => true,
            Some((bl, bc)) => {
                len > *bl
                    || (len == *bl
                        && matches!(claim, Claim::Live(_))
                        && matches!(bc, Claim::History(_)))
            }
        };
        if better {
            best = Some((len, claim));
        }
    };
    for v in mounted {
        if path_under(path, &v.path) {
            consider(v.path.len(), Claim::Live(v));
        }
    }
    for rec in volumes {
        for m in &rec.mount_history {
            if path_under(path, &m.path) {
                consider(m.path.len(), Claim::History(rec));
            }
        }
    }
    match best?.1 {
        Claim::Live(v) => {
            let record = v
                .uuid
                .as_ref()
                .and_then(|u| volumes.iter().find(|r| &r.fs_uuid == u));
            Some(VolumeBadge {
                label: record
                    .and_then(|r| r.label.clone())
                    .unwrap_or_else(|| v.name.clone()),
                offline: false,
                last_seen: record.map(|r| r.last_seen.clone()),
            })
        }
        Claim::History(rec) => Some(VolumeBadge {
            label: rec
                .label
                .clone()
                .unwrap_or_else(|| "Unnamed drive".to_string()),
            offline: true,
            last_seen: Some(rec.last_seen.clone()),
        }),
    }
}

#[cfg(test)]
mod badge_tests {
    use super::*;

    fn vol(path: &str, name: &str, uuid: Option<&str>) -> MountedVolume {
        MountedVolume {
            name: name.to_string(),
            path: path.to_string(),
            removable: true,
            uuid: uuid.map(|s| s.to_string()),
            label: None,
        }
    }

    fn record(uuid: &str, label: &str, mounts: &[&str]) -> ufm_catalog::VolumeRecord {
        ufm_catalog::VolumeRecord {
            fs_uuid: uuid.to_string(),
            label: Some(label.to_string()),
            first_seen: "2026-01-01T00:00:00Z".to_string(),
            last_seen: "2026-10-01T00:00:00Z".to_string(),
            last_indexed: None,
            mount_history: mounts
                .iter()
                .map(|p| ufm_catalog::MountSighting {
                    path: p.to_string(),
                    first_seen: "2026-01-01T00:00:00Z".to_string(),
                    last_seen: "2026-10-01T00:00:00Z".to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn unplugged_drive_items_badge_offline_with_their_own_name() {
        // The v1.1 probe case: Desk SSD unplugged, only "/" live.
        // Its files must NOT ride the root catch-all into an
        // online badge — the history claim is longer and wins.
        let mounted = vec![vol("/", "Internal", Some("root-uuid"))];
        let volumes = vec![
            record("root-uuid", "Internal", &["/"]),
            record("desk-uuid", "Desk SSD", &["/media/richard/Desk SSD"]),
        ];
        let badge = volume_badge_for_path(
            &mounted,
            &volumes,
            "/media/richard/Desk SSD/ROMS/game.nes",
        )
        .expect("badge");
        assert!(badge.offline, "unplugged drive's file must badge offline");
        assert_eq!(badge.label, "Desk SSD");
        // A genuinely internal file still badges online.
        let badge = volume_badge_for_path(&mounted, &volumes, "/home/richard/game.nes")
            .expect("badge");
        assert!(!badge.offline);
    }

    #[test]
    fn plugged_drive_items_badge_online_by_live_claim() {
        let mounted = vec![
            vol("/", "Internal", Some("root-uuid")),
            vol("/media/richard/Desk SSD", "Desk SSD", Some("desk-uuid")),
        ];
        let volumes = vec![record("desk-uuid", "Desk SSD", &["/media/richard/Desk SSD"])];
        let badge = volume_badge_for_path(
            &mounted,
            &volumes,
            "/media/richard/Desk SSD/ROMS/game.nes",
        )
        .expect("badge");
        assert!(!badge.offline);
        assert_eq!(badge.label, "Desk SSD");
    }

    #[test]
    fn unclaimed_paths_get_no_badge() {
        let mounted = vec![vol("/", "Internal", None)];
        assert!(volume_badge_for_path(&mounted, &[], "relative/path.nes").is_none());
    }
}

/// "Oct 9, 2026" for badge/error copy; falls back to the raw date
/// part when the timestamp doesn't parse.
pub(crate) fn human_date(rfc3339: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(rfc3339) {
        Ok(dt) => dt.format("%b %-d, %Y").to_string(),
        Err(_) => rfc3339.chars().take(10).collect(),
    }
}

/// Deterministic UUID for a volume seed string (FNV-1a 128-bit): the
/// same mount path must yield the same location / root-item ids on every
/// enumeration, or sidebar refresh would churn ids under open tabs.
fn volume_uuid(seed: &str) -> uuid::Uuid {
    let mut h1: u64 = 0xcbf2_9ce4_8422_2325;
    for b in seed.bytes() {
        h1 = (h1 ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    let mut h2: u64 = 0x8422_2325_cbf2_9ce4;
    for b in seed.bytes().rev() {
        h2 = (h2 ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&h1.to_be_bytes());
    bytes[8..].copy_from_slice(&h2.to_be_bytes());
    uuid::Uuid::from_bytes(bytes)
}

/// Disk space for the volume backing a path (inc-2 item 12: sidebar
/// "Disk Properties…"). Byte counts come from statvfs — the same
/// source `df` reports — so the dialog can be checked against df:
/// free_bytes is f_bavail (what an unprivileged user can actually
/// write), used_bytes mirrors df's Used ((f_blocks - f_bfree) blocks).
/// Mount point / source / fstype come from findmnt --target, the
/// tool the Disk Utility layer already shells out to.
#[derive(serde::Serialize)]
pub struct DiskSpaceInfo {
    pub path: String,
    pub mount_point: String,
    pub source: Option<String>,
    pub fstype: Option<String>,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub free_bytes: u64,
    pub percent_free: f64,
}

fn statvfs_totals(path: &str) -> std::result::Result<(u64, u64, u64), String> {
    let cpath = std::ffi::CString::new(path)
        .map_err(|_| "path contains a NUL byte".to_string())?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(cpath.as_ptr(), &mut st) };
    if rc != 0 {
        return Err(format!(
            "statvfs {path}: {}",
            std::io::Error::last_os_error()
        ));
    }
    let fr = st.f_frsize as u64;
    let total = (st.f_blocks as u64).saturating_mul(fr);
    let free_all = (st.f_bfree as u64).saturating_mul(fr);
    let avail = (st.f_bavail as u64).saturating_mul(fr);
    Ok((total, total.saturating_sub(free_all), avail))
}

#[tauri::command]
pub async fn get_disk_info(path: String) -> Cmd<DiskSpaceInfo> {
    let info = tauri::async_runtime::spawn_blocking(move || {
        let (total, used, free) = statvfs_totals(&path)?;
        // findmnt resolves the mount that actually contains the path.
        // TARGET goes last in the column list so a mount point with
        // spaces ("Desk SSD") survives the split intact.
        let (mut source, mut fstype, mut mount_point) =
            (None, None, path.clone());
        if let Ok(out) = std::process::Command::new("findmnt")
            .args(["-no", "SOURCE,FSTYPE,TARGET", "--target"])
            .arg(&path)
            .output()
        {
            let text = String::from_utf8_lossy(&out.stdout);
            let line = text.lines().next().unwrap_or("").trim_end();
            let toks: Vec<&str> = line.split_whitespace().collect();
            if toks.len() >= 3 {
                source = Some(toks[0].to_string());
                fstype = Some(toks[1].to_string());
                // Re-join the tail: the target may contain spaces.
                let head = format!("{} {}", toks[0], toks[1]);
                if let Some(rest) = line
                    .trim_start()
                    .strip_prefix(&head)
                    .map(|r| r.trim_start())
                {
                    if !rest.is_empty() {
                        mount_point = rest.to_string();
                    }
                }
            }
        }
        let denom = used.saturating_add(free);
        let percent_free = if denom > 0 {
            (free as f64 / denom as f64) * 100.0
        } else {
            0.0
        };
        Ok::<DiskSpaceInfo, String>(DiskSpaceInfo {
            path,
            mount_point,
            source,
            fstype,
            total_bytes: total,
            used_bytes: used,
            free_bytes: free,
            percent_free,
        })
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("disk info task failed: {e}"))))?
    .map_err(|e: String| CommandError::from(UfmError::Internal(e)))?;
    Ok(info)
}

#[tauri::command]
pub async fn list_locations(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Cmd<Vec<LocationSummary>> {
    let volumes = tauri::async_runtime::spawn_blocking(enumerate_mounted_volumes)
        .await
        .unwrap_or_default();
    // Library Memory: every location listing is also a sighting —
    // the volumes table learns what is mounted right now (records
    // are created on first sight and last_seen refreshes). The
    // catalog keeps OFFLINE volumes' rows regardless; this only
    // updates identity/history.
    {
        let catalog = state.catalog.lock().unwrap();
        record_volume_sightings(&catalog);
    }
    // Emulation alias keeper: when the mounted set changes (a drive
    // arrived or left), the unified view reconciles as a delta.
    {
        let mut now: Vec<String> = volumes.iter().map(|v| v.path.clone()).collect();
        now.sort();
        let changed = {
            let mut seen = state.emu_mounts_seen.lock().unwrap();
            let changed = *seen != now;
            if changed {
                *seen = now;
            }
            changed
        };
        if changed {
            crate::emu_farm::spawn_keeper(app.clone());
        }
    }
    let vol_paths: std::collections::HashSet<String> =
        volumes.iter().map(|v| v.path.clone()).collect();

    // Prune volume locations whose mount vanished (unplugged/unmounted).
    let stale: Vec<LocationId> = {
        let locs = state.locations.lock().unwrap();
        locs.iter()
            .filter(|l| {
                l.provider == "volume"
                    && l.path
                        .as_deref()
                        .map(|p| !vol_paths.contains(p))
                        .unwrap_or(true)
            })
            .map(|l| l.id)
            .collect()
    };
    for id in stale {
        state
            .locations
            .lock()
            .unwrap()
            .retain(|l| l.id != id);
        if let Some(root_item) = state.root_items.lock().unwrap().remove(&id) {
            state.item_paths.lock().unwrap().remove(&root_item);
        }
        if let Some(root) = state.local_roots.lock().unwrap().remove(&id) {
            state.path_ids.lock().unwrap().remove(&root);
        }
    }

    // Root paths already covered by a non-volume location win (dedupe).
    let volume_ids: std::collections::HashSet<LocationId> = {
        let locs = state.locations.lock().unwrap();
        locs.iter()
            .filter(|l| l.provider == "volume")
            .map(|l| l.id)
            .collect()
    };
    let covered: std::collections::HashSet<std::path::PathBuf> = {
        let roots = state.local_roots.lock().unwrap();
        roots
            .iter()
            .filter(|(id, _)| !volume_ids.contains(id))
            .map(|(_, p)| p.clone())
            .collect()
    };

    for v in &volumes {
        let root = std::path::PathBuf::from(&v.path);
        if covered.contains(&root) {
            continue;
        }
        let id = LocationId(volume_uuid(&format!("volume:{}", v.path)));
        let root_item = ItemId(volume_uuid(&format!("volume-root:{}", v.path)));
        let already = state
            .local_roots
            .lock()
            .unwrap()
            .get(&id)
            .map(|p| *p == root)
            .unwrap_or(false);
        if !already {
            state.local_roots.lock().unwrap().insert(id, root.clone());
            state.root_items.lock().unwrap().insert(id, root_item);
            state
                .item_paths
                .lock()
                .unwrap()
                .insert(root_item, root.clone());
            state.path_ids.lock().unwrap().insert(root.clone(), root_item);
        }
        let mut locs = state.locations.lock().unwrap();
        if let Some(existing) = locs.iter_mut().find(|l| l.id == id) {
            existing.display_name = v.name.clone();
            existing.removable = v.removable;
            existing.path = Some(v.path.clone());
        } else {
            locs.push(LocationSummary {
                id,
                provider: "volume".into(),
                display_name: v.name.clone(),
                root_item,
                path: Some(v.path.clone()),
                removable: v.removable,
                uri: None,
                online: true,
            });
        }
    }

    // Network Locations: merge server bookmarks against the live
    // GIO mount list (fresh every call, like volumes from lsblk).
    let live_net = tauri::async_runtime::spawn_blocking(crate::network::live_mounts)
        .await
        .unwrap_or_default();
    merge_network_locations(&state, &live_net);

    Ok(state.locations.lock().unwrap().clone())
}

// ---------------------------------------------------------------------------
// Network Locations — runtime locations like volumes (never written
// to locations.json; bookmarks live in network-servers.json, passwords
// only in the OS keyring). See network.rs and AUDIT.md.
// ---------------------------------------------------------------------------

pub(crate) fn network_location_id(uri: &str) -> LocationId {
    LocationId(volume_uuid(&format!("network:{uri}")))
}

fn network_root_item(uri: &str) -> ItemId {
    ItemId(volume_uuid(&format!("network-root:{uri}")))
}

fn register_browse_root(state: &AppState, id: LocationId, root_item: ItemId, root: &Path) {
    state
        .local_roots
        .lock()
        .unwrap()
        .insert(id, root.to_path_buf());
    state.root_items.lock().unwrap().insert(id, root_item);
    state
        .item_paths
        .lock()
        .unwrap()
        .insert(root_item, root.to_path_buf());
    state
        .path_ids
        .lock()
        .unwrap()
        .insert(root.to_path_buf(), root_item);
}

fn unregister_browse_root(state: &AppState, id: LocationId) {
    state.local_roots.lock().unwrap().remove(&id);
    if let Some(root_item) = state.root_items.lock().unwrap().remove(&id) {
        if let Some(root) = state.item_paths.lock().unwrap().remove(&root_item) {
            state.path_ids.lock().unwrap().remove(&root);
        }
    }
}

fn upsert_location_summary(state: &AppState, summary: LocationSummary) {
    let mut locs = state.locations.lock().unwrap();
    match locs.iter_mut().find(|l| l.id == summary.id) {
        Some(existing) => *existing = summary,
        None => locs.push(summary),
    }
}

/// Merge server bookmarks into the locations list against the live
/// GIO mounts: connected → registered for browsing + online; absent
/// → still listed, offline (Library Memory). Records a catalog
/// sighting for each connected server (the `volumes` mirror).
fn merge_network_locations(state: &AppState, live: &[crate::network::LiveMount]) {
    let bookmarks = crate::network::load_bookmarks(&state.data_base);
    let bookmarked: std::collections::HashSet<LocationId> = bookmarks
        .iter()
        .map(|b| network_location_id(&b.uri))
        .collect();
    // Prune network rows whose bookmark is gone (forgotten elsewhere
    // or the sidecar was edited) — volumes prune the same way.
    let stale: Vec<LocationId> = state
        .locations
        .lock()
        .unwrap()
        .iter()
        .filter(|l| l.provider == "network" && !bookmarked.contains(&l.id))
        .map(|l| l.id)
        .collect();
    for id in stale {
        unregister_browse_root(state, id);
        state.locations.lock().unwrap().retain(|l| l.id != id);
    }
    let now = chrono::Utc::now().to_rfc3339();
    for b in &bookmarks {
        let id = network_location_id(&b.uri);
        let root_item = network_root_item(&b.uri);
        let parsed = crate::network::normalize_server_uri(&b.uri).ok();
        let live_mount = parsed.as_ref().and_then(|p| {
            live.iter()
                .find(|m| crate::network::uri_matches(&m.root_uri, p))
                // Browse root = the requested share under the mount,
                // not the raw mount root (sftp home-root case).
                .map(|m| crate::network::effective_mount(m, p))
        });
        match live_mount {
            Some(m) => {
                {
                    let catalog = state.catalog.lock().unwrap();
                    let _ = catalog.record_network_sighting(
                        &b.uri,
                        Some(&b.display_name),
                        &m.fuse_path.to_string_lossy(),
                        &now,
                    );
                }
                register_browse_root(state, id, root_item, &m.fuse_path);
                upsert_location_summary(
                    state,
                    LocationSummary {
                        id,
                        provider: "network".into(),
                        display_name: b.display_name.clone(),
                        root_item,
                        path: Some(m.fuse_path.to_string_lossy().into_owned()),
                        removable: false,
                        uri: Some(b.uri.clone()),
                        online: true,
                    },
                );
            }
            None => {
                unregister_browse_root(state, id);
                upsert_location_summary(
                    state,
                    LocationSummary {
                        id,
                        provider: "network".into(),
                        display_name: b.display_name.clone(),
                        root_item,
                        path: None,
                        removable: false,
                        uri: Some(b.uri.clone()),
                        online: false,
                    },
                );
            }
        }
    }
}

/// The Network Locations sibling of `volume_badge_for_path`: resolve
/// an item path to a server badge. A path under a live mount's FUSE
/// prefix is online; a path under any remembered mount prefix is
/// offline with the server's last_seen — the unplugged-drive
/// semantics, for servers.
pub(crate) fn network_badge_for_path(
    records: &[ufm_catalog::NetworkLocationRecord],
    live: &[crate::network::LiveMount],
    path: &str,
) -> Option<VolumeBadge> {
    for m in live {
        if path_under(path, &m.fuse_path.to_string_lossy()) {
            if let Ok(parsed) = crate::network::normalize_server_uri(&m.root_uri) {
                if let Some(rec) = records.iter().find(|r| r.uri == parsed.uri) {
                    return Some(VolumeBadge {
                        label: rec
                            .display_name
                            .clone()
                            .unwrap_or_else(|| parsed.host.clone()),
                        offline: false,
                        last_seen: Some(rec.last_seen.clone()),
                    });
                }
            }
            // A live GVfs mount with no bookmark (mounted by another
            // app): still a server — badge it by its GIO name.
            return Some(VolumeBadge {
                label: m.name.clone(),
                offline: false,
                last_seen: None,
            });
        }
    }
    let mut best: Option<(&ufm_catalog::NetworkLocationRecord, usize)> = None;
    for rec in records {
        for sighting in &rec.mount_history {
            if path_under(path, &sighting.path) {
                let len = sighting.path.len();
                if best.is_none_or(|(_, l)| len > l) {
                    best = Some((rec, len));
                }
            }
        }
    }
    best.map(|(rec, _)| VolumeBadge {
        label: rec
            .display_name
            .clone()
            .unwrap_or_else(|| rec.uri.clone()),
        offline: true,
        last_seen: Some(rec.last_seen.clone()),
    })
}

/// Resolve a network location id back to its bookmark.
fn bookmark_for_location(
    state: &AppState,
    location: LocationId,
) -> Cmd<crate::network::ServerBookmark> {
    crate::network::load_bookmarks(&state.data_base)
        .into_iter()
        .find(|b| network_location_id(&b.uri) == location)
        .ok_or_else(|| {
            CommandError::from(UfmError::NotFound(
                "that server isn't in the saved servers list".into(),
            ))
        })
}

/// Connect to a server (smb/ftp/ftps/sftp/dav/davs/nfs/afp) via GIO.
/// Mounts it, saves the bookmark, and returns the now-online
/// Location. The password goes to the OS keyring ONLY when
/// `remember_password` is set and the mount succeeded.
#[tauri::command]
pub async fn net_connect(
    state: State<'_, AppState>,
    uri: String,
    username: Option<String>,
    password: Option<String>,
    domain: Option<String>,
    anonymous: bool,
    remember_password: bool,
) -> Cmd<LocationSummary> {
    let parsed = crate::network::normalize_server_uri(&uri)
        .map_err(|e| CommandError::from(UfmError::InvalidInput(e)))?;
    let effective_user = username
        .filter(|u| !u.trim().is_empty())
        .or_else(|| parsed.username.clone());
    let explicit_password = password.filter(|p| !p.is_empty());
    // A typed password wins; otherwise fall back to the keyring's
    // saved one (that's what Remember means at reconnect time).
    let effective_password =
        explicit_password.clone().or_else(|| crate::network::saved_password(&parsed.uri));
    let creds = crate::network::Credentials {
        username: effective_user.clone(),
        password: effective_password,
        domain: domain.filter(|d| !d.trim().is_empty()),
        anonymous,
    };
    // Gauntlet G1: GVfs presence is checked HERE, at the
    // Connect door — the daemons plus this scheme's backend are
    // ensured in context (deps.rs), in plain language, BEFORE
    // any mount attempt can fail cryptically inside GIO.
    let gvfs_scheme = parsed.scheme.clone();
    tauri::async_runtime::spawn_blocking(move || crate::deps::ensure_gvfs(&gvfs_scheme))
        .await
        .map_err(|e| CommandError::from(UfmError::Internal(format!("connect setup task: {e}"))))?
        .map_err(|e| CommandError::from(UfmError::InvalidInput(e)))?;
    let connect_uri = parsed.uri.clone();
    let mount = tauri::async_runtime::spawn_blocking(move || {
        crate::network::mount(&connect_uri, &creds)
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("connect task: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::InvalidInput(e)))?;
    // Mounted. Persist the bookmark (password to the keyring only if
    // the user asked us to remember it).
    let mut has_saved = crate::network::saved_password(&parsed.uri).is_some();
    if remember_password {
        if let Some(p) = &explicit_password {
            crate::network::store_password(&parsed.uri, p)
                .map_err(|e| CommandError::from(UfmError::Internal(e)))?;
            has_saved = true;
        }
    }
    let bookmark = crate::network::ServerBookmark {
        uri: parsed.uri.clone(),
        display_name: crate::network::display_name_for(&parsed),
        username: effective_user,
        has_saved_password: has_saved,
    };
    crate::network::upsert_bookmark(&state.data_base, bookmark)
        .map_err(|e| CommandError::from(UfmError::Internal(e)))?;
    // Sync state (registers the browse root, records the sighting).
    let mut live = tauri::async_runtime::spawn_blocking(crate::network::live_mounts)
        .await
        .unwrap_or_default();
    if !live.iter().any(|m| m.fuse_path == mount.fuse_path) {
        live.push(mount);
    }
    merge_network_locations(&state, &live);
    state
        .locations
        .lock()
        .unwrap()
        .iter()
        .find(|l| l.id == network_location_id(&parsed.uri))
        .cloned()
        .ok_or_else(|| {
            CommandError::from(UfmError::Internal(
                "connected, but the location didn't register".into(),
            ))
        })
}

/// Disconnect a server: GIO unmount. The bookmark stays — the
/// location flips to its offline state, files still badged in
/// search — until Forget Server removes it entirely.
#[tauri::command]
pub async fn net_disconnect(state: State<'_, AppState>, location: LocationId) -> Cmd<()> {
    let bookmark = bookmark_for_location(&state, location)?;
    let uri = bookmark.uri.clone();
    tauri::async_runtime::spawn_blocking(move || crate::network::unmount(&uri))
        .await
        .map_err(|e| CommandError::from(UfmError::Internal(format!("disconnect task: {e}"))))?
        .map_err(|e| CommandError::from(UfmError::Internal(e)))?;
    let live = tauri::async_runtime::spawn_blocking(crate::network::live_mounts)
        .await
        .unwrap_or_default();
    merge_network_locations(&state, &live);
    Ok(())
}

/// Forget a server entirely: unmount if connected, drop the
/// bookmark, the saved password, and the catalog record (its indexed
/// items are purged, like forgetting a drive).
#[tauri::command]
pub async fn net_forget(state: State<'_, AppState>, location: LocationId) -> Cmd<()> {
    let bookmark = bookmark_for_location(&state, location)?;
    let uri = bookmark.uri.clone();
    // Best-effort unmount first — a vanished server must not block
    // forgetting it.
    let u = uri.clone();
    let _ = tauri::async_runtime::spawn_blocking(move || crate::network::unmount(&u)).await;
    crate::network::forget_password(&uri);
    crate::network::remove_bookmark(&state.data_base, &uri)
        .map_err(|e| CommandError::from(UfmError::Internal(e)))?;
    {
        let catalog = state.catalog.lock().unwrap();
        if let Ok(Some(rec)) = catalog.network_location(&uri) {
            let prefixes: Vec<String> =
                rec.mount_history.iter().map(|m| m.path.clone()).collect();
            if !prefixes.is_empty() {
                let _ = catalog.purge_items_under_prefixes(&prefixes);
            }
            let _ = catalog.delete_network_location(&uri);
        }
    }
    let live = tauri::async_runtime::spawn_blocking(crate::network::live_mounts)
        .await
        .unwrap_or_default();
    merge_network_locations(&state, &live);
    Ok(())
}

/// Durable UI preferences (Round 7.2): a small JSON sidecar in the app
/// data dir, next to locations.json. Sidebar collapse state lives here
/// because WebKit localStorage proved unreliable across the AppImage's
/// launch contexts (state written in one session was missing in the
/// next); the shell keeps localStorage only as a pre-paint fast path.
#[tauri::command]
pub fn get_ui_prefs(state: State<'_, AppState>) -> Cmd<serde_json::Value> {
    let data = std::fs::read(&state.ui_prefs_path).unwrap_or_default();
    Ok(serde_json::from_slice(&data).unwrap_or_else(|_| serde_json::json!({})))
}

#[tauri::command]
pub fn set_ui_prefs(state: State<'_, AppState>, prefs: serde_json::Value) -> Cmd<()> {
    let json = serde_json::to_string_pretty(&prefs).map_err(|e| {
        CommandError::from(UfmError::Internal(format!("ui prefs encode: {e}")))
    })?;
    // Atomic write: temp file + rename, never truncate the good copy
    // (same discipline as the locations sidecar).
    let tmp = state.ui_prefs_path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| {
        CommandError::from(UfmError::Internal(format!("ui prefs write: {e}")))
    })?;
    std::fs::rename(&tmp, &state.ui_prefs_path).map_err(|e| {
        CommandError::from(UfmError::Internal(format!("ui prefs rename: {e}")))
    })?;
    Ok(())
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
    // Hydrate user metadata (labels + stars, both path-keyed in the
    // catalog). Stars come from one batch query for the whole listing.
    if let Ok(catalog) = ufm_catalog::Catalog::open(&state.catalog_path) {
        let keys: Vec<String> = items
            .iter()
            .filter_map(|i| i.provider_key.clone())
            .collect();
        let starred = catalog.starred_paths(&keys).unwrap_or_default();
        for item in items.iter_mut() {
            if let Some(pk) = &item.provider_key {
                if let Ok(labels) = catalog.labels_for_path(pk) {
                    item.labels = labels;
                }
                item.starred = starred.contains(pk);
            }
        }
    }
    Ok(items)
}

// ---------------------------------------------------------------------------
// Jobs — the transfer center
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyRequest {
    pub sources: Vec<ItemId>,
    pub dst_parent: ItemId,
}

/// Live control block for one job: the shared snapshot the executor
/// mutates, plus the pause/cancel flags the job commands flip. Mirrors
/// the `transcode_cancels` pattern, one entry per job. The catalog holds
/// the durable copy; this is the control surface.
#[derive(Clone)]
pub struct JobHandle {
    pub job: std::sync::Arc<Mutex<ufm_jobs::Job>>,
    pub pause: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// True while an executor task owns this job.
    pub live: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl JobHandle {
    pub fn new(job: ufm_jobs::Job, paused: bool) -> Self {
        use std::sync::atomic::AtomicBool;
        Self {
            job: std::sync::Arc::new(Mutex::new(job)),
            pause: std::sync::Arc::new(AtomicBool::new(paused)),
            cancel: std::sync::Arc::new(AtomicBool::new(false)),
            live: std::sync::Arc::new(AtomicBool::new(false)),
        }
    }

    fn snapshot(&self) -> ufm_jobs::Job {
        self.job.lock().unwrap().clone()
    }
}

fn flag(b: &std::sync::atomic::AtomicBool) -> bool {
    b.load(std::sync::atomic::Ordering::SeqCst)
}

pub(crate) fn job_state_name(state: ufm_jobs::JobState) -> &'static str {
    match state {
        ufm_jobs::JobState::Queued => "queued",
        ufm_jobs::JobState::Running => "running",
        ufm_jobs::JobState::Paused => "paused",
        ufm_jobs::JobState::Completed => "completed",
        ufm_jobs::JobState::Cancelled => "cancelled",
        ufm_jobs::JobState::Failed => "failed",
    }
}

/// Bytes attributable to finished steps (Done or verify-Skipped).
/// Recomputed from the snapshot so progress survives pause/resume.
/// Inc-2 (§2.5): Move/Delete steps count too — rename moves carry the
/// walked tree total on their src Item, permanent deletes carry
/// per-file sizes, and a trash step's target carries the walked total,
/// so move/delete progress bars advance instead of sitting at 0.
pub(crate) fn job_bytes_done(job: &ufm_jobs::Job) -> u64 {
    job.steps
        .values()
        .filter(|s| {
            matches!(
                s.state,
                ufm_jobs::StepState::Done | ufm_jobs::StepState::Skipped
            )
        })
        .filter_map(|s| match &s.op {
            ufm_jobs::StepOp::Copy { src, .. } => src.size_bytes,
            ufm_jobs::StepOp::Move { src, .. } => src.size_bytes,
            ufm_jobs::StepOp::Delete { target, .. } => target.size_bytes,
            _ => None,
        })
        .sum()
}

/// The `job-progress` event — the Transfer Center contract. Mirrors the
/// `index-progress` / `transcode-progress` payload style.
fn emit_job_progress(
    app: &tauri::AppHandle,
    job: &ufm_jobs::Job,
    bytes_done: u64,
    current: Option<&str>,
    error: Option<&str>,
) {
    let steps_done = job
        .steps
        .values()
        .filter(|s| {
            matches!(
                s.state,
                ufm_jobs::StepState::Done | ufm_jobs::StepState::Skipped
            )
        })
        .count();
    let _ = app.emit(
        "job-progress",
        serde_json::json!({
            "job_id": job.id.to_string(),
            "title": job.title,
            "state": job_state_name(job.state),
            "steps_done": steps_done,
            "steps_total": job.steps.len(),
            "bytes_done": bytes_done,
            "bytes_total": job.preview.bytes_to_move,
            "packed": job.packed,
            "current": current,
            "done": matches!(
                job.state,
                ufm_jobs::JobState::Completed
                    | ufm_jobs::JobState::Cancelled
                    | ufm_jobs::JobState::Failed
            ),
            "error": error,
        }),
    );
}

pub(crate) fn job_item(
    id: ItemId,
    location: LocationId,
    path: &Path,
    kind: ItemKind,
    size: Option<u64>,
) -> Item {
    Item {
        id,
        location,
        name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        kind,
        size_bytes: size,
        modified: None,
        provider_key: Some(path.to_string_lossy().into_owned()),
        labels: Vec::new(),
        starred: false,
    }
}

/// Resolve a destination file name inside `dst_dir`. On collision, record
/// a preview conflict and fall back to the app's paste idiom
/// (`name`, `name 2`, `name 3`, …) — a job never silently overwrites.
fn resolve_dest_name(
    dst_dir: &Path,
    name: &str,
    preview: &mut ufm_jobs::JobPreview,
) -> String {
    if !dst_dir.join(name).exists() {
        return name.to_string();
    }
    preview.conflicts.push(ufm_jobs::Conflict {
        path: dst_dir.join(name).to_string_lossy().into_owned(),
        kind: ufm_jobs::ConflictKind::NameCollision,
    });
    let mut n = 2;
    loop {
        let candidate = format!("{name} {n}");
        if !dst_dir.join(&candidate).exists() {
            return candidate;
        }
        n += 1;
    }
}

/// Builds the step DAG for a copy. Deterministic step ids (`s0001`, …)
/// keep journal rows stable and greppable.
struct CopyPlanner {
    job: ufm_jobs::Job,
    seq: u32,
    location: LocationId,
}

impl CopyPlanner {
    fn next_id(&mut self) -> String {
        self.seq += 1;
        format!("s{:04}", self.seq)
    }

    fn add_mkdir(
        &mut self,
        parent: &Path,
        name: &str,
        depends_on: Option<String>,
    ) -> String {
        let id = self.next_id();
        self.job.add_step(ufm_jobs::Step {
            id: id.clone(),
            label: format!("create folder {name}"),
            op: ufm_jobs::StepOp::Mkdir {
                parent: job_item(
                    ItemId::new(),
                    self.location,
                    parent,
                    ItemKind::Folder,
                    None,
                ),
                name: name.to_string(),
            },
            depends_on: depends_on.into_iter().collect(),
            state: ufm_jobs::StepState::Pending,
            compensation: None,
            outcome: false,
            outcome_path: None,
        });
        id
    }

    fn add_file(
        &mut self,
        src_path: &Path,
        dst_dir: &Path,
        new_name: &str,
        size: u64,
        mkdir_dep: Option<String>,
    ) {
        let src_item = job_item(
            ItemId::new(),
            self.location,
            src_path,
            ItemKind::File,
            Some(size),
        );
        let dst_item = job_item(
            ItemId::new(),
            self.location,
            &dst_dir.join(new_name),
            ItemKind::File,
            Some(size),
        );
        let verify_id = self.next_id();
        self.job.add_step(ufm_jobs::Step {
            id: verify_id.clone(),
            label: format!("verify {}", src_path.display()),
            op: ufm_jobs::StepOp::VerifyCopy {
                src: src_item.clone(),
                dst: dst_item,
            },
            depends_on: vec![],
            state: ufm_jobs::StepState::Pending,
            compensation: None,
            outcome: false,
            outcome_path: None,
        });
        let mut deps = vec![verify_id];
        if let Some(m) = mkdir_dep {
            deps.push(m);
        }
        let copy_id = self.next_id();
        self.job.add_step(ufm_jobs::Step {
            id: copy_id,
            label: format!("copy {}", src_path.display()),
            op: ufm_jobs::StepOp::Copy {
                src: src_item,
                dst_parent: job_item(
                    ItemId::new(),
                    self.location,
                    dst_dir,
                    ItemKind::Folder,
                    None,
                ),
                new_name: new_name.to_string(),
            },
            depends_on: deps,
            state: ufm_jobs::StepState::Pending,
            compensation: None,
            outcome: false,
            outcome_path: None,
        });
        self.job.preview.files_affected += 1;
        self.job.preview.bytes_to_move += size;
    }

    /// Plan the contents of `src_dir` into `dst_dir`. `mkdir_dep` is the
    /// step that creates `dst_dir` (None when `dst_dir` already exists).
    fn plan_dir(
        &mut self,
        src_dir: &Path,
        dst_dir: &Path,
        mkdir_dep: Option<String>,
    ) -> Cmd<()> {
        let io_err = |e: std::io::Error| CommandError::from(UfmError::Internal(e.to_string()));
        let mut entries: Vec<std::fs::DirEntry> = Vec::new();
        for entry in std::fs::read_dir(src_dir).map_err(io_err)? {
            entries.push(entry.map_err(io_err)?);
        }
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let ty = entry.file_type().map_err(io_err)?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if ty.is_dir() {
                let mid = self.add_mkdir(dst_dir, &name, mkdir_dep.clone());
                self.plan_dir(&path, &dst_dir.join(&name), Some(mid))?;
            } else {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                self.add_file(&path, dst_dir, &name, size, mkdir_dep.clone());
            }
        }
        Ok(())
    }
}

/// Build (but do not run) the copy job for a request. Ids resolve through
/// the live-browse caches: `item_paths` for paths, `item_locations` for
/// the location stamped onto step items.
fn build_copy_job(state: &AppState, req: &CopyRequest) -> Cmd<ufm_jobs::Job> {
    if req.sources.is_empty() {
        return Err(CommandError::from(UfmError::InvalidInput(
            "nothing to copy".into(),
        )));
    }
    let (dst_dir, dst_location, src_paths) = {
        let paths = state.item_paths.lock().unwrap();
        let locs = state.item_locations.lock().unwrap();
        let dst_dir = paths
            .get(&req.dst_parent)
            .cloned()
            .ok_or_else(|| {
                CommandError::from(UfmError::NotFound("unknown destination".into()))
            })?;
        let mut srcs = Vec::new();
        for id in &req.sources {
            let p = paths.get(id).cloned().ok_or_else(|| {
                CommandError::from(UfmError::NotFound("unknown source item".into()))
            })?;
            srcs.push(p);
        }
        (dst_dir, locs.get(&req.dst_parent).copied(), srcs)
    };
    let location = dst_location.unwrap_or_default();
    plan_copy_job_paths(&src_paths, &dst_dir, location)
}

/// The paths-based copy core (inc-2 §2.3): `build_copy_job` resolves
/// ids and delegates here; `start_copy_from_paths` calls it directly.
fn plan_copy_job_paths(
    srcs: &[PathBuf],
    dst_dir: &Path,
    location: LocationId,
) -> Cmd<ufm_jobs::Job> {
    if srcs.is_empty() {
        return Err(CommandError::from(UfmError::InvalidInput(
            "nothing to copy".into(),
        )));
    }
    if !dst_dir.is_dir() {
        return Err(CommandError::from(UfmError::InvalidInput(
            "destination is not a folder".into(),
        )));
    }
    let title = if srcs.len() == 1 {
        let name = srcs[0]
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "item".into());
        format!("Copy {name}")
    } else {
        format!("Copy {} items", srcs.len())
    };
    let mut planner = CopyPlanner {
        job: ufm_jobs::Job::new(title, ufm_jobs::JobPreview::default()),
        seq: 0,
        location,
    };
    for src_path in srcs {
        let meta = std::fs::metadata(src_path).map_err(|e| {
            CommandError::from(UfmError::Internal(format!(
                "source {}: {e}",
                src_path.display()
            )))
        })?;
        let name = src_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| {
                CommandError::from(UfmError::InvalidInput("source has no name".into()))
            })?;
        if meta.is_dir() {
            let resolved =
                resolve_dest_name(dst_dir, &name, &mut planner.job.preview);
            let mid = planner.add_mkdir(dst_dir, &resolved, None);
            planner.plan_dir(src_path, &dst_dir.join(&resolved), Some(mid))?;
        } else {
            let resolved =
                resolve_dest_name(dst_dir, &name, &mut planner.job.preview);
            planner.add_file(src_path, dst_dir, &resolved, meta.len(), None);
        }
    }
    Ok(planner.job)
}

/// Same-device test for move planning (inc-2 §2.3): `rename(2)`
/// succeeds iff both paths share an `st_dev` (btrfs subvolumes report
/// distinct `st_dev` and cross-subvolume rename returns EXDEV, so the
/// check agrees with reality there too). Any metadata error → false
/// (decompose — the safe direction).
fn same_device(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(ma), Ok(mb)) => ma.dev() == mb.dev(),
        _ => false,
    }
}

/// Metadata-only walk: (file count, total bytes). Symlinks are leaves
/// — never followed — matching the planners' kind rule (§2.3).
fn walk_totals(path: &Path) -> (u64, u64) {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return (0, 0),
    };
    if !meta.is_dir() {
        return (1, meta.len());
    }
    let mut files = 0;
    let mut bytes = 0;
    if let Ok(rd) = std::fs::read_dir(path) {
        for entry in rd.flatten() {
            let (f, b) = walk_totals(&entry.path());
            files += f;
            bytes += b;
        }
    }
    (files, bytes)
}

/// Planner for move/delete jobs (inc-2 §2.3): a sibling of
/// `CopyPlanner` — which stays untouched, shipped behavior — reusing
/// the same helpers and deterministic `s{:04}` step ids.
struct PathPlanner {
    job: ufm_jobs::Job,
    seq: u32,
    location: LocationId,
}

impl PathPlanner {
    fn next_id(&mut self) -> String {
        self.seq += 1;
        format!("s{:04}", self.seq)
    }

    fn add_step(
        &mut self,
        label: String,
        op: ufm_jobs::StepOp,
        depends_on: Vec<String>,
        outcome: bool,
        outcome_path: Option<String>,
    ) -> String {
        let id = self.next_id();
        self.job.add_step(ufm_jobs::Step {
            id: id.clone(),
            label,
            op,
            depends_on,
            state: ufm_jobs::StepState::Pending,
            compensation: None,
            outcome,
            outcome_path,
        });
        id
    }

    fn add_mkdir(
        &mut self,
        parent: &Path,
        name: &str,
        depends_on: Option<String>,
    ) -> String {
        self.add_step(
            format!("create folder {name}"),
            ufm_jobs::StepOp::Mkdir {
                parent: job_item(
                    ItemId::new(),
                    self.location,
                    parent,
                    ItemKind::Folder,
                    None,
                ),
                name: name.to_string(),
            },
            depends_on.into_iter().collect(),
            false,
            None,
        )
    }

    /// VerifyCopy → Copy → source Delete for one file of a
    /// cross-device move. The Delete depends only on its Copy —
    /// **a source is deleted only after its copy is journaled
    /// Done/Skipped** (the §2.4 invariant; if the Copy was
    /// verify-Skipped, the bytes are provably at the destination and
    /// the Delete still runs; if the Copy Failed, `run_job` stops the
    /// job before any dependent Delete executes). A crash anywhere in
    /// here leaves duplicates (src + dst), never loss.
    /// Returns (verify_id, copy_id, delete_id).
    fn add_move_file(
        &mut self,
        src_path: &Path,
        dst_dir: &Path,
        new_name: &str,
        size: u64,
        mkdir_dep: Option<String>,
    ) -> (String, String, String) {
        let src_item = job_item(
            ItemId::new(),
            self.location,
            src_path,
            ItemKind::File,
            Some(size),
        );
        let dst_item = job_item(
            ItemId::new(),
            self.location,
            &dst_dir.join(new_name),
            ItemKind::File,
            Some(size),
        );
        let verify_id = self.add_step(
            format!("verify {}", src_path.display()),
            ufm_jobs::StepOp::VerifyCopy {
                src: src_item.clone(),
                dst: dst_item,
            },
            vec![],
            false,
            None,
        );
        let mut deps = vec![verify_id.clone()];
        if let Some(m) = mkdir_dep {
            deps.push(m);
        }
        let copy_id = self.add_step(
            format!("copy {}", src_path.display()),
            ufm_jobs::StepOp::Copy {
                src: src_item.clone(),
                dst_parent: job_item(
                    ItemId::new(),
                    self.location,
                    dst_dir,
                    ItemKind::Folder,
                    None,
                ),
                new_name: new_name.to_string(),
            },
            deps,
            false,
            None,
        );
        let delete_id = self.add_step(
            format!("remove {}", src_path.display()),
            ufm_jobs::StepOp::Delete {
                target: src_item,
                use_trash: false,
            },
            vec![copy_id.clone()],
            false,
            None,
        );
        self.job.preview.files_affected += 1;
        self.job.preview.bytes_to_move += size;
        (verify_id, copy_id, delete_id)
    }

    /// Plan the contents of `src_dir` into `dst_dir` (created by
    /// `mkdir_dep`'s step) for a cross-device move. Returns every step
    /// id created in this subtree so the source dir's own Delete can
    /// depend on all of them — directories are removed deepest-first
    /// and always empty when their turn comes.
    fn plan_move_dir(
        &mut self,
        src_dir: &Path,
        dst_dir: &Path,
        mkdir_dep: &str,
    ) -> Cmd<Vec<String>> {
        let io_err =
            |e: std::io::Error| CommandError::from(UfmError::Internal(e.to_string()));
        let mut entries: Vec<std::fs::DirEntry> = Vec::new();
        for entry in std::fs::read_dir(src_dir).map_err(io_err)? {
            entries.push(entry.map_err(io_err)?);
        }
        entries.sort_by_key(|e| e.file_name());
        let mut ids = Vec::new();
        for entry in entries {
            let ty = entry.file_type().map_err(io_err)?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if ty.is_dir() {
                let child_mkdir =
                    self.add_mkdir(dst_dir, &name, Some(mkdir_dep.to_string()));
                let mut entry_ids = vec![child_mkdir.clone()];
                let mut sub =
                    self.plan_move_dir(&path, &dst_dir.join(&name), &child_mkdir)?;
                entry_ids.append(&mut sub);
                let del = self.add_step(
                    format!("remove {}", path.display()),
                    ufm_jobs::StepOp::Delete {
                        target: job_item(
                            ItemId::new(),
                            self.location,
                            &path,
                            ItemKind::Folder,
                            Some(0),
                        ),
                        use_trash: false,
                    },
                    entry_ids.clone(),
                    false,
                    None,
                );
                entry_ids.push(del);
                ids.extend(entry_ids);
            } else {
                // DirEntry::metadata follows symlinks — parity with
                // CopyPlanner (copies read through links).
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                let (verify_id, copy_id, delete_id) = self.add_move_file(
                    &path,
                    dst_dir,
                    &name,
                    size,
                    Some(mkdir_dep.to_string()),
                );
                ids.extend([verify_id, copy_id, delete_id]);
            }
        }
        Ok(ids)
    }

    /// Plan a permanent-delete walk for the contents of `src_dir`:
    /// one Delete per file, one per subdirectory (depending on
    /// everything inside it). Returns every Delete id in the subtree.
    fn plan_delete_dir(&mut self, src_dir: &Path) -> Cmd<Vec<String>> {
        let io_err =
            |e: std::io::Error| CommandError::from(UfmError::Internal(e.to_string()));
        let mut entries: Vec<std::fs::DirEntry> = Vec::new();
        for entry in std::fs::read_dir(src_dir).map_err(io_err)? {
            entries.push(entry.map_err(io_err)?);
        }
        entries.sort_by_key(|e| e.file_name());
        let mut ids = Vec::new();
        for entry in entries {
            let ty = entry.file_type().map_err(io_err)?;
            let path = entry.path();
            if ty.is_dir() {
                let mut sub = self.plan_delete_dir(&path)?;
                let del = self.add_step(
                    format!("remove {}", path.display()),
                    ufm_jobs::StepOp::Delete {
                        target: job_item(
                            ItemId::new(),
                            self.location,
                            &path,
                            ItemKind::Folder,
                            Some(0),
                        ),
                        use_trash: false,
                    },
                    sub.clone(),
                    false,
                    None,
                );
                sub.push(del);
                ids.extend(sub);
            } else {
                // Leaf (file or symlink — links are never walked).
                let size =
                    std::fs::symlink_metadata(&path).map(|m| m.len()).unwrap_or(0);
                let del = self.add_step(
                    format!("delete {}", path.display()),
                    ufm_jobs::StepOp::Delete {
                        target: job_item(
                            ItemId::new(),
                            self.location,
                            &path,
                            ItemKind::File,
                            Some(size),
                        ),
                        use_trash: false,
                    },
                    vec![],
                    false,
                    None,
                );
                ids.push(del);
            }
        }
        Ok(ids)
    }
}

/// Build (but do not run) a move job over resolved paths (§2.3).
fn plan_move_job(
    srcs: &[PathBuf],
    dst_dir: &Path,
    location: LocationId,
) -> Cmd<ufm_jobs::Job> {
    plan_move_job_with(srcs, dst_dir, location, &same_device)
}

/// `plan_move_job` with the device predicate injected, so tests can
/// force the cross-device decomposition on a single-filesystem host.
fn plan_move_job_with(
    srcs: &[PathBuf],
    dst_dir: &Path,
    location: LocationId,
    same_dev: &dyn Fn(&Path, &Path) -> bool,
) -> Cmd<ufm_jobs::Job> {
    if srcs.is_empty() {
        return Err(CommandError::from(UfmError::InvalidInput(
            "nothing to move".into(),
        )));
    }
    if !dst_dir.is_dir() {
        return Err(CommandError::from(UfmError::InvalidInput(
            "destination is not a folder".into(),
        )));
    }
    let title = if srcs.len() == 1 {
        let name = srcs[0]
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "item".into());
        format!("Move {name}")
    } else {
        format!("Move {} items", srcs.len())
    };
    let mut planner = PathPlanner {
        job: ufm_jobs::Job::new(title, ufm_jobs::JobPreview::default()),
        seq: 0,
        location,
    };
    for src in srcs {
        let name = src
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| {
                CommandError::from(UfmError::InvalidInput("source has no name".into()))
            })?;
        // Already in the destination → no steps for this source
        // (parity with move_item_to's no-op; checked BEFORE collision
        // resolution, exactly as today).
        if src.parent() == Some(dst_dir) {
            continue;
        }
        let resolved = resolve_dest_name(dst_dir, &name, &mut planner.job.preview);
        let is_dir = std::fs::symlink_metadata(src)
            .map(|m| m.is_dir())
            .unwrap_or(false);
        if same_dev(src, dst_dir) {
            // One rename step — a directory moves whole; the tree is
            // never decomposed. Preview totals come from a
            // metadata-only walk (rename moves complete near-instantly
            // but report the same totals as any other move).
            let (files, bytes) = walk_totals(src);
            planner.job.preview.files_affected += files;
            planner.job.preview.bytes_to_move += bytes;
            let kind = if is_dir {
                ItemKind::Folder
            } else {
                ItemKind::File
            };
            planner.add_step(
                format!("move {}", src.display()),
                ufm_jobs::StepOp::Move {
                    src: job_item(
                        ItemId::new(),
                        location,
                        src,
                        kind,
                        Some(bytes),
                    ),
                    dst_parent: job_item(
                        ItemId::new(),
                        location,
                        dst_dir,
                        ItemKind::Folder,
                        None,
                    ),
                    new_name: resolved,
                },
                vec![],
                true,
                None,
            );
        } else if is_dir {
            // Cross-device directory: copy walk + source cleanup.
            let mid = planner.add_mkdir(dst_dir, &resolved, None);
            let mut ids = vec![mid.clone()];
            let mut sub = planner.plan_move_dir(src, &dst_dir.join(&resolved), &mid)?;
            ids.append(&mut sub);
            // The top-level source dir's Delete depends on ALL steps
            // in its subtree and carries the source's outcome.
            planner.add_step(
                format!("remove {}", src.display()),
                ufm_jobs::StepOp::Delete {
                    target: job_item(
                        ItemId::new(),
                        location,
                        src,
                        ItemKind::Folder,
                        Some(0),
                    ),
                    use_trash: false,
                },
                ids,
                true,
                Some(dst_dir.join(&resolved).to_string_lossy().into_owned()),
            );
        } else {
            // Cross-device file: VerifyCopy → Copy → Delete.
            let size = std::fs::metadata(src).map_err(|e| {
                CommandError::from(UfmError::Internal(format!(
                    "source {}: {e}",
                    src.display()
                )))
            })?
            .len();
            let (_, _, del) = planner.add_move_file(src, dst_dir, &resolved, size, None);
            if let Some(s) = planner.job.steps.get_mut(&del) {
                s.outcome = true;
                s.outcome_path =
                    Some(dst_dir.join(&resolved).to_string_lossy().into_owned());
            }
        }
    }
    Ok(planner.job)
}

/// Build (but do not run) a delete job over resolved paths (§2.3).
fn plan_delete_job(
    srcs: &[PathBuf],
    use_trash: bool,
    location: LocationId,
) -> Cmd<ufm_jobs::Job> {
    if srcs.is_empty() {
        return Err(CommandError::from(UfmError::InvalidInput(
            "nothing to delete".into(),
        )));
    }
    let n = srcs.len();
    let plural = if n == 1 { "" } else { "s" };
    let title = if use_trash {
        format!("Move {n} item{plural} to Trash")
    } else {
        format!("Delete {n} item{plural}")
    };
    let mut planner = PathPlanner {
        job: ufm_jobs::Job::new(title, ufm_jobs::JobPreview::default()),
        seq: 0,
        location,
    };
    for src in srcs {
        // Preview totals: one metadata-only walk per source (spec §6
        // preview contract); the walked total is also stored on the
        // step's target Item so job_bytes_done can advance (§2.5).
        let (files, bytes) = walk_totals(src);
        planner.job.preview.files_affected += files;
        planner.job.preview.bytes_to_move += bytes;
        let is_dir = std::fs::symlink_metadata(src)
            .map(|m| m.is_dir())
            .unwrap_or(false);
        if use_trash {
            // ONE step per top-level source: trashing is a rename of
            // the whole item (tree intact, one Trash entry).
            // Decomposing it would shatter one trash entry into many
            // and break untrash_item semantics.
            let kind = if is_dir {
                ItemKind::Folder
            } else {
                ItemKind::File
            };
            planner.add_step(
                format!("trash {}", src.display()),
                ufm_jobs::StepOp::Delete {
                    target: job_item(
                        ItemId::new(),
                        location,
                        src,
                        kind,
                        Some(bytes),
                    ),
                    use_trash: true,
                },
                vec![],
                true,
                None,
            );
        } else if is_dir {
            // Permanent delete walks and decomposes — granularity is
            // the point of the destroy flavor.
            let ids = planner.plan_delete_dir(src)?;
            planner.add_step(
                format!("remove {}", src.display()),
                ufm_jobs::StepOp::Delete {
                    target: job_item(
                        ItemId::new(),
                        location,
                        src,
                        ItemKind::Folder,
                        Some(0),
                    ),
                    use_trash: false,
                },
                ids,
                true,
                None,
            );
        } else {
            let size = std::fs::symlink_metadata(src).map(|m| m.len()).unwrap_or(0);
            planner.add_step(
                format!("delete {}", src.display()),
                ufm_jobs::StepOp::Delete {
                    target: job_item(
                        ItemId::new(),
                        location,
                        src,
                        ItemKind::File,
                        Some(size),
                    ),
                    use_trash: false,
                },
                vec![],
                true,
                None,
            );
        }
    }
    Ok(planner.job)
}

/// Derive the `JobOutcome` for a finished outcome step (inc-2 §2.2) —
/// the record the frontend's undo stack is fed from at job completion.
/// `result_path` comes from the step's `outcome_path` when the planner
/// recorded one (decomposed moves), else is derived from the op. The
/// trash entry name arrives via the Done-transition journal note
/// (`trash_name=<name>`, written by the Delete executor arm).
fn build_outcome(step: &ufm_jobs::Step, note: Option<&str>) -> ufm_jobs::JobOutcome {
    let (src_path, result_path, trash_name) = match &step.op {
        ufm_jobs::StepOp::Copy {
            src,
            dst_parent,
            new_name,
        }
        | ufm_jobs::StepOp::Move {
            src,
            dst_parent,
            new_name,
        } => {
            let derived = dst_parent.provider_key.as_ref().map(|p| {
                Path::new(p)
                    .join(new_name)
                    .to_string_lossy()
                    .into_owned()
            });
            (
                src.provider_key.clone().unwrap_or_default(),
                step.outcome_path.clone().or(derived),
                None,
            )
        }
        ufm_jobs::StepOp::Delete { target, use_trash } => {
            let trash = if *use_trash {
                note.and_then(|n| n.strip_prefix("trash_name="))
                    .map(|s| s.to_string())
            } else {
                None
            };
            (
                target.provider_key.clone().unwrap_or_default(),
                None,
                trash,
            )
        }
        _ => (String::new(), None, None),
    };
    ufm_jobs::JobOutcome {
        step_id: step.id.clone(),
        src_path,
        result_path,
        trash_name,
    }
}

/// P0 (2026-10-14, "pause/cancel are dead"): how often the executor
/// persists the whole-job blob. The blob is a CHECKPOINT, not a
/// journal — the append-only journal rows are the per-event record,
/// and startup recovery replays them over the checkpoint. The old
/// design rewrote the blob on EVERY transition: for Dickles' 291k-
/// file copy the blob is 616 MB, so each file paid two 616 MB
/// serializations + writes, the journal's own inserts lost the
/// write-lock race (the live journal table was EMPTY), and pause /
/// cancel clicks paid the same 616 MB clone+write under the catalog
/// lock — the buttons weren't broken, the bookkeeping was the job.
/// The blob now lands on: this many transitions, this many seconds,
/// any outcome-step completion (undo records are load-bearing), and
/// every job-level transition (transition_job always writes).
struct SnapshotGate {
    since_write: u32,
    last: std::time::Instant,
}

impl SnapshotGate {
    fn new() -> Self {
        Self {
            since_write: 0,
            last: std::time::Instant::now(),
        }
    }

    fn should_write(&mut self, force: bool) -> bool {
        self.since_write += 1;
        let due = force
            || self.since_write >= 1000
            || self.last.elapsed() >= std::time::Duration::from_secs(10);
        if due {
            self.since_write = 0;
            self.last = std::time::Instant::now();
        }
        due
    }
}

/// What a step transition changed, as counts — the emit path works
/// from these plus a short lock, never from a cloned job (P0: one
/// clone of a 465k-step job is ~600 MB of allocation).
struct StepFx {
    bytes_done: u64,
}

/// Emit `job-progress` from the handle's live state under a short
/// lock — no job clone, no blob write (P0).
pub(crate) fn emit_from_handle(
    app: &tauri::AppHandle,
    handle: &JobHandle,
    bytes_done: u64,
    current: Option<&str>,
    error: Option<&str>,
) {
    let j = handle.job.lock().unwrap();
    emit_job_progress(app, &j, bytes_done, current, error);
}

/// Bytes attributable to finished steps, computed under the lock.
fn locked_bytes_done(handle: &JobHandle) -> u64 {
    let j = handle.job.lock().unwrap();
    job_bytes_done(&j)
}

/// Record one step transition everywhere it must land: the
/// in-memory state, the durable journal (append-only, per event),
/// and — gated by `gate` — the jobs-table checkpoint blob.
/// Inc-2: when an outcome step (§2.2) finishes, its `JobOutcome` is
/// appended under the same lock; an outcome completion also forces
/// a checkpoint, so the outcome is durable with the transition.
fn transition_step(
    handle: &JobHandle,
    catalog: &ufm_catalog::Catalog,
    journal: &mut ufm_jobs::JobJournal,
    gate: &mut SnapshotGate,
    step_id: &str,
    to: ufm_jobs::StepState,
    note: Option<String>,
) -> StepFx {
    let (fx, job_id, from, checkpoint) = {
        let mut j = handle.job.lock().unwrap();
        let from = j
            .steps
            .get(step_id)
            .map(|s| s.state)
            .unwrap_or(ufm_jobs::StepState::Pending);
        if let Some(s) = j.steps.get_mut(step_id) {
            s.state = to;
        }
        let mut outcome_completed = false;
        if matches!(
            to,
            ufm_jobs::StepState::Done | ufm_jobs::StepState::Skipped
        ) {
            let outcome = j.steps.get(step_id).and_then(|s| {
                if s.outcome && !j.outcomes.iter().any(|o| o.step_id == step_id) {
                    outcome_completed = true;
                    Some(build_outcome(s, note.as_deref()))
                } else {
                    None
                }
            });
            if let Some(o) = outcome {
                j.outcomes.push(o);
            }
        }
        let fx = StepFx {
            bytes_done: job_bytes_done(&j),
        };
        let checkpoint = if gate.should_write(outcome_completed) {
            Some(j.clone())
        } else {
            None
        };
        (fx, j.id, from, checkpoint)
    };
    journal.record_with_note(job_id, step_id, from, to, note);
    if let Some(snap) = checkpoint {
        let _ = catalog.upsert_job(&snap);
    }
    fx
}

/// Set the job-level state and persist the snapshot.
fn transition_job(
    handle: &JobHandle,
    catalog: &ufm_catalog::Catalog,
    to: ufm_jobs::JobState,
) -> ufm_jobs::Job {
    let snap = {
        let mut j = handle.job.lock().unwrap();
        j.state = to;
        j.clone()
    };
    let _ = catalog.upsert_job(&snap);
    snap
}

/// Block while the job is paused. Returns false if cancellation landed
/// meanwhile (the caller must finalize the job as Cancelled).
fn wait_while_paused(handle: &JobHandle) -> bool {
    while flag(&handle.pause) && !flag(&handle.cancel) {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    !flag(&handle.cancel)
}

enum StepOutcome {
    /// Finished. The payload is an optional journal note carried into
    /// the Done transition (inc-2: the trash executor returns
    /// `trash_name=<name>` so the entry name is durable in the journal
    /// and feeds `JobOutcome.trash_name`).
    Done(Option<String>),
    Skipped,
    Failed(String),
    Cancelled,
}

/// Filesystem core of the Move arm (inc-2 §2.4): a single rename,
/// guarded so a re-run after a crash can never overwrite or duplicate.
/// The planner decomposes cross-device moves; the executor never
/// falls back to a silent in-step copy.
fn exec_move_rename(src: &Path, dst: &Path) -> StepOutcome {
    let src_exists = src.symlink_metadata().is_ok();
    let dst_exists = dst.symlink_metadata().is_ok();
    match (src_exists, dst_exists) {
        // The rename landed just before a crash, before its journal
        // transition — the move is done.
        (false, true) => StepOutcome::Done(None),
        (false, false) => StepOutcome::Failed("source vanished".into()),
        // std::fs::rename would SILENTLY REPLACE an existing
        // destination file on Linux — this guard is the never-overwrite
        // rule. Do not simplify it away (§7.2).
        (true, true) => {
            StepOutcome::Failed("destination appeared since planning".into())
        }
        (true, false) => match std::fs::rename(src, dst) {
            Ok(()) => StepOutcome::Done(None),
            // EXDEV (raw OS error 18 on Linux): the planner's device
            // check was raced — fail loudly, never improvise a copy.
            Err(e) if e.raw_os_error() == Some(18) => StepOutcome::Failed(
                "cross-device move reached executor undecomposed".into(),
            ),
            Err(e) => StepOutcome::Failed(e.to_string()),
        },
    }
}

/// Filesystem core of the trash Delete arm (inc-2 §2.4): replicates
/// `trash_item` exactly, parameterized by trash root so tests can use
/// a temp root. Returns the trash entry name.
fn exec_trash(
    target: &Path,
    trash_root: &Path,
) -> std::result::Result<String, String> {
    let files_dir = trash_root.join("files");
    let info_dir = trash_root.join("info");
    std::fs::create_dir_all(&files_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&info_dir).map_err(|e| e.to_string())?;

    let name = target
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("invalid file name")?
        .to_string();

    // Collision-free destination name: name, "name 2", "name 3", ...
    // Checks BOTH dirs — an orphan .trashinfo left by a crash in the
    // write-then-rename window still reserves its name.
    let mut dest_name = name.clone();
    let mut n = 2;
    while files_dir.join(&dest_name).exists()
        || info_dir.join(format!("{dest_name}.trashinfo")).exists()
    {
        dest_name = format!("{name} {n}");
        n += 1;
    }
    let dest = files_dir.join(&dest_name);

    // Write the .trashinfo first, then move (spec order). Path= is the
    // RAW absolute path — untrash_item parses it raw.
    let now = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();
    let info = format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        target.to_string_lossy(),
        now
    );
    std::fs::write(info_dir.join(format!("{dest_name}.trashinfo")), info)
        .map_err(|e| e.to_string())?;

    // rename; fall back to copy+remove across filesystems (the Trash
    // lives under $HOME; sources on other devices are the normal case).
    if std::fs::rename(target, &dest).is_err() {
        let ty = std::fs::metadata(target).map_err(|e| e.to_string())?;
        if ty.is_dir() {
            copy_dir_recursive(target, &dest).map_err(|e| e.to_string())?;
            std::fs::remove_dir_all(target).map_err(|e| e.to_string())?;
        } else {
            std::fs::copy(target, &dest).map_err(|e| e.to_string())?;
            std::fs::remove_file(target).map_err(|e| e.to_string())?;
        }
    }
    Ok(dest_name)
}

/// Filesystem core of the permanent Delete arm (inc-2 §2.4).
/// Directories go with `remove_dir` — NEVER `remove_dir_all`: the
/// planner's dependency order guarantees the dir is empty by the time
/// its step runs, and a non-empty error is a real signal (something
/// is writing into the tree) that must surface as Failed.
fn exec_permanent_delete(target: &Path) -> StepOutcome {
    let meta = match std::fs::symlink_metadata(target) {
        Ok(m) => m,
        // A re-run after a kill: absence is the goal state.
        Err(_) => return StepOutcome::Skipped,
    };
    let result = if meta.is_dir() {
        std::fs::remove_dir(target)
    } else {
        std::fs::remove_file(target)
    };
    match result {
        Ok(()) => StepOutcome::Done(None),
        Err(e) => StepOutcome::Failed(e.to_string()),
    }
}

/// Execute one step. Copy is chunked (1 MiB) so pause/cancel land
/// between chunks, not just between files; progress emits are throttled.
fn execute_step(
    op: &ufm_jobs::StepOp,
    handle: &JobHandle,
    app: &tauri::AppHandle,
    bytes_done: &mut u64,
) -> StepOutcome {
    use std::io::{Read, Write};
    match op {
        ufm_jobs::StepOp::Mkdir { parent, name } => {
            let base = parent.provider_key.as_deref().unwrap_or_default();
            match std::fs::create_dir_all(Path::new(base).join(name)) {
                Ok(()) => StepOutcome::Done(None),
                Err(e) => StepOutcome::Failed(e.to_string()),
            }
        }
        ufm_jobs::StepOp::WriteFile { parent, name, contents } => {
            // Phase 6: the one plugin-originated mutation, always a
            // confirmed proposal by the time it lands here. Never
            // overwrites: an existing file fails the step loudly.
            let base = parent.provider_key.as_deref().unwrap_or_default();
            let path = Path::new(base).join(name);
            if path.exists() {
                StepOutcome::Failed(format!("{} already exists", path.display()))
            } else {
                match std::fs::write(&path, contents) {
                    Ok(()) => StepOutcome::Done(None),
                    Err(e) => StepOutcome::Failed(e.to_string()),
                }
            }
        }
        ufm_jobs::StepOp::VerifyCopy { src, dst } => {
            // Verify-then-skip: an equal-length destination file counts
            // as already copied (size-level in this increment; hash
            // verification is a follow-up).
            let src_path = Path::new(src.provider_key.as_deref().unwrap_or_default());
            let dst_path = Path::new(dst.provider_key.as_deref().unwrap_or_default());
            match std::fs::metadata(src_path) {
                Ok(sm) => match std::fs::metadata(dst_path) {
                    Ok(dm) if dm.is_file() && dm.len() == sm.len() => {
                        StepOutcome::Skipped
                    }
                    _ => StepOutcome::Done(None),
                },
                Err(e) => StepOutcome::Failed(e.to_string()),
            }
        }
        ufm_jobs::StepOp::Copy {
            src,
            dst_parent,
            new_name,
        } => {
            let src_path = Path::new(src.provider_key.as_deref().unwrap_or_default());
            let dst_path =
                Path::new(dst_parent.provider_key.as_deref().unwrap_or_default())
                    .join(new_name);
            let mut input = match std::fs::File::open(src_path) {
                Ok(f) => f,
                Err(e) => return StepOutcome::Failed(e.to_string()),
            };
            let mut output = match std::fs::File::create(&dst_path) {
                Ok(f) => f,
                Err(e) => return StepOutcome::Failed(e.to_string()),
            };
            let mut buf = vec![0u8; 1024 * 1024];
            let mut last_emit = std::time::Instant::now();
            let result: std::result::Result<(), StepOutcome> = loop {
                if flag(&handle.cancel) {
                    break Err(StepOutcome::Cancelled);
                }
                if flag(&handle.pause) && !wait_while_paused(handle) {
                    break Err(StepOutcome::Cancelled);
                }
                match input.read(&mut buf) {
                    Ok(0) => break Ok(()),
                    Ok(n) => {
                        if let Err(e) = output.write_all(&buf[..n]) {
                            break Err(StepOutcome::Failed(e.to_string()));
                        }
                        *bytes_done += n as u64;
                        if last_emit.elapsed() >= std::time::Duration::from_millis(150)
                        {
                            emit_from_handle(
                                app,
                                handle,
                                *bytes_done,
                                Some(&format!("copy {}", src_path.display())),
                                None,
                            );
                            last_emit = std::time::Instant::now();
                        }
                    }
                    Err(e) => break Err(StepOutcome::Failed(e.to_string())),
                }
            };
            match result {
                Ok(()) => {
                    // Match std::fs::copy: carry the source permissions.
                    if let Ok(meta) = std::fs::metadata(src_path) {
                        let _ = std::fs::set_permissions(
                            &dst_path,
                            meta.permissions(),
                        );
                    }
                    StepOutcome::Done(None)
                }
                Err(outcome) => {
                    // Compensating action: never leave a partial file
                    // behind to be mistaken for a finished copy.
                    drop(output);
                    let _ = std::fs::remove_file(&dst_path);
                    outcome
                }
            }
        }
        ufm_jobs::StepOp::Move {
            src,
            dst_parent,
            new_name,
        } => {
            let src_path =
                Path::new(src.provider_key.as_deref().unwrap_or_default());
            let dst_path =
                Path::new(dst_parent.provider_key.as_deref().unwrap_or_default())
                    .join(new_name);
            exec_move_rename(src_path, &dst_path)
        }
        ufm_jobs::StepOp::Delete { target, use_trash } => {
            let target_path =
                Path::new(target.provider_key.as_deref().unwrap_or_default());
            if *use_trash {
                // Target already gone → Skipped (idempotent re-run;
                // the entry is already in Trash — §2.6: its trash
                // name is unrecoverable for the undo stack, but the
                // item itself is safe and restorable from Trash view).
                if target_path.symlink_metadata().is_err() {
                    return StepOutcome::Skipped;
                }
                let home = std::env::var("HOME").unwrap_or_default();
                let trash_root =
                    PathBuf::from(format!("{home}/.local/share/Trash"));
                match exec_trash(target_path, &trash_root) {
                    Ok(name) => {
                        StepOutcome::Done(Some(format!("trash_name={name}")))
                    }
                    Err(e) => StepOutcome::Failed(e),
                }
            } else {
                exec_permanent_delete(target_path)
            }
        }
        ufm_jobs::StepOp::FetchMedia {
            item_id,
            system,
            dat_name,
            rom_stem,
            media_type,
            crc,
            md5,
            sha1,
            size,
        } => {
            // ROM media enrichment: the step spec is data only — the
            // cascade, the destination, and the credentials are all
            // resolved inside rommedia at execution time (settings
            // may have changed since the job was planned; secrets
            // never live in job snapshots).
            let spec = crate::rommedia::FetchSpec {
                item_id: item_id.clone(),
                system: system.clone(),
                dat_name: dat_name.clone(),
                rom_stem: rom_stem.clone(),
                media_type: media_type.clone(),
                crc: crc.clone(),
                md5: md5.clone(),
                sha1: sha1.clone(),
                size: *size,
            };
            let catalog_path = app.state::<AppState>().catalog_path.clone();
            let base = catalog_path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_default();
            let progress = |text: &str| {
                emit_from_handle(app, handle, *bytes_done, Some(text), None);
            };
            match crate::rommedia::run_fetch(
                &spec,
                &base,
                &catalog_path,
                &|| flag(&handle.cancel),
                &|| wait_while_paused(handle),
                &progress,
            ) {
                crate::rommedia::FetchVerdict::Done => StepOutcome::Done(None),
                crate::rommedia::FetchVerdict::Skipped => StepOutcome::Skipped,
                crate::rommedia::FetchVerdict::Failed(msg) => {
                    StepOutcome::Failed(msg)
                }
                crate::rommedia::FetchVerdict::Cancelled => {
                    StepOutcome::Cancelled
                }
            }
        }
        ufm_jobs::StepOp::FetchArtwork {
            video_path,
            media_type,
            tmdb_id,
            season,
            fanart,
        } => {
            // Media Completion artwork: data-only step — the TMDB
            // key is read from the keyring at execution time (never
            // stored in job snapshots). Placed sidecars are named in
            // the Done journal note (the durable placement record).
            let key = crate::mediares::tmdb_key_get();
            match crate::mediacomplete::fetch_artwork(
                std::path::Path::new(video_path),
                media_type,
                *tmdb_id,
                *season,
                *fanart,
                key,
                &|| flag(&handle.cancel),
            ) {
                crate::mediacomplete::FetchVerdict::Done(placed) => {
                    StepOutcome::Done(Some(format!(
                        "placed={}",
                        placed.join(",")
                    )))
                }
                crate::mediacomplete::FetchVerdict::Skipped(_) => {
                    StepOutcome::Skipped
                }
                crate::mediacomplete::FetchVerdict::Failed(msg) => {
                    StepOutcome::Failed(msg)
                }
            }
        }
        ufm_jobs::StepOp::FetchSubtitle {
            video_path,
            language,
            media_type,
            tmdb_id,
            season,
            episode,
        } => {
            // Media Completion subtitles: OpenSubtitles credentials
            // resolve inside the client from the keyring at
            // execution time. The Done note carries the API's own
            // quota report — the job card shows the real allowance
            // state, never an invented number.
            match crate::mediacomplete::fetch_subtitle(
                std::path::Path::new(video_path),
                language,
                media_type,
                *tmdb_id,
                *season,
                *episode,
            ) {
                crate::mediacomplete::SubVerdict::Done { path, quota } => {
                    let note = match quota.remaining {
                        Some(r) => format!(
                            "subtitle={path} opensubtitles_remaining={r}"
                        ),
                        None => format!("subtitle={path}"),
                    };
                    StepOutcome::Done(Some(note))
                }
                crate::mediacomplete::SubVerdict::Skipped(_) => {
                    StepOutcome::Skipped
                }
                crate::mediacomplete::SubVerdict::QuotaReached(q) => {
                    StepOutcome::Failed(format!(
                        "OpenSubtitles download allowance reached{}",
                        q.message
                            .as_deref()
                            .map(|m| format!(" — {m}"))
                            .unwrap_or_default()
                    ))
                }
                crate::mediacomplete::SubVerdict::Failed(msg) => {
                    StepOutcome::Failed(msg)
                }
            }
        }
        ufm_jobs::StepOp::ConvertMedia { src, dst_name, kind } => {
            // File Auditor conversion: ffmpeg beside the original,
            // never overwriting. The Done note records the TRUE
            // before/after byte counts (before=<n> after=<n>) so the
            // job card reports the real saving against the
            // Auditor's labeled estimate.
            let src_path = Path::new(src.provider_key.as_deref().unwrap_or_default());
            let Some(parent) = src_path.parent() else {
                return StepOutcome::Failed("source has no folder".into());
            };
            let dst_path = parent.join(dst_name);
            if dst_path.exists() {
                return StepOutcome::Failed(format!(
                    "{} already exists — nothing was overwritten",
                    dst_path.display()
                ));
            }
            match exec_convert(src_path, &dst_path, kind, handle) {
                Ok((before, after)) => {
                    *bytes_done += after;
                    StepOutcome::Done(Some(format!(
                        "converted={} before={before} after={after}",
                        dst_path.display()
                    )))
                }
                Err(e) => StepOutcome::Failed(e),
            }
        }
    }
}

/// File Auditor: run one ffmpeg conversion to completion,
/// honoring the job's cancel flag. Returns (before, after) byte
/// counts. A missing ffmpeg is SET UP on the spot via crate::deps
/// (the user just approved this conversion — the system prompt
/// belongs to that click) and the conversion then runs; only a
/// declined/failed setup lands as a plain-language failure.
fn exec_convert(
    src: &Path,
    dst: &Path,
    kind: &str,
    handle: &JobHandle,
) -> std::result::Result<(u64, u64), String> {
    use std::sync::atomic::Ordering;
    let src_s = src.to_string_lossy().into_owned();
    let dst_s = dst.to_string_lossy().into_owned();
    let args: Vec<&str> = match kind {
        "flac" => vec![
            "-hide_banner", "-loglevel", "error", "-nostdin", "-i", &src_s, "-c:a", "flac",
            &dst_s,
        ],
        "hevc" => vec![
            "-hide_banner", "-loglevel", "error", "-nostdin", "-i", &src_s, "-c:v",
            "libx265", "-crf", "24", "-preset", "medium", "-c:a", "copy", "-c:s", "copy",
            &dst_s,
        ],
        "png" => vec![
            "-hide_banner", "-loglevel", "error", "-nostdin", "-i", &src_s, &dst_s,
        ],
        other => return Err(format!("unknown conversion kind: {other}")),
    };
    let before = std::fs::metadata(src).map(|m| m.len()).unwrap_or(0);
    let spawn_ffmpeg = |args: &[&str]| {
        std::process::Command::new("ffmpeg")
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
    };
    let mut child = match spawn_ffmpeg(&args) {
        Ok(c) => c,
        Err(_) => {
            // Dependency law: a conversion the user just approved
            // sets ffmpeg up on the spot (system prompt) and then
            // runs — the plain failure below is only the receipt
            // when that setup was declined or couldn't complete.
            match crate::deps::ensure_tool("ffmpeg", "ffmpeg") {
                Ok(()) => match spawn_ffmpeg(&args) {
                    Ok(c) => c,
                    Err(_) => {
                        return Err(
                            "ffmpeg still couldn't start after setup, so the conversion didn't run. The original file was left untouched."
                                .into(),
                        )
                    }
                },
                Err(why) => {
                    return Err(format!(
                        "{why} The conversion didn't run, and the original file was left untouched."
                    ))
                }
            }
        }
    };
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    let _ = std::fs::remove_file(dst);
                    return Err(
                        "the conversion failed — the original file was left untouched."
                            .into(),
                    );
                }
                break;
            }
            Ok(None) => {
                if handle.cancel.load(Ordering::Relaxed) {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = std::fs::remove_file(dst);
                    return Err("cancelled; partial output removed".into());
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    let after = std::fs::metadata(dst).map(|m| m.len()).unwrap_or(0);
    Ok((before, after))
}

// ---------------------------------------------------------------------------
// Packed transfer (UI round — "migrate library magic", Dickles
// 2026-10-14, after watching Zorin Files copy 291,699 files at
// 21.8 MB/s): when a job's Copy steps form a many-small-files tree
// headed to a DIFFERENT device, per-file copying pays a metadata +
// open/close round trip per file on both filesystems; streaming the
// files back-to-back as a ustar (STORE — media does not compress
// and the N150's CPU is precious) pipe pays it once. The engine
// picks the mode itself at job start — no settings, no mode
// picker; the only UI trace is a small "Packed transfer" label on
// the job card (job.packed).
//
// Resume law (his standing order): a step is journaled Done only
// after its file is fully extracted at the destination and its
// extracted length verified. A kill mid-stream leaves extracted
// files in place; on resume their steps are Done and the rest
// stream again or fall to the per-file loop — the union of both
// sides is always complete. For moves the planner's Delete steps
// run in the main loop AFTER the copies, so a source is deleted
// only once its copy step is Done: a crash can duplicate, never
// lose.
//
// Fallback law: any packed-path failure stops the pass; every
// not-yet-committed step simply runs through the ordinary
// per-file loop. A job never dies because the fast path failed.
// ---------------------------------------------------------------------------

/// Packing engages when the eligible cross-device set has at least
/// this many files and their mean size is at most
/// PACKED_MAX_MEAN_BYTES. Starting hypotheses from the ruling;
/// confirmed by the on-box bench in the round report — encoded here
/// so the bench result is what ships, not vibes.
const PACKED_MIN_FILES: u64 = 500;
const PACKED_MAX_MEAN_BYTES: u64 = 1024 * 1024;
/// Files above this stay per-file even inside a packed job — the
/// stream is for the small-file mass; a giant streams fine alone.
const PACKED_MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;
/// In-flight cap between packer and unpacker: eight 256 KiB blocks
/// (the 2026-10-14 OOM lesson — the pipe stays a pipe, never a
/// buffer that grows with the tree).
const PACKED_BLOCK: usize = 256 * 1024;

/// One Copy step resolved for streaming.
struct PackedFile {
    step_id: String,
    src: PathBuf,
    dst: PathBuf,
    size: u64,
}

/// Nearest existing ancestor (device checks need a real path; the
/// destination tree's deeper parents are Mkdir steps that may not
/// have run yet when the pass is chosen).
fn existing_ancestor(p: &Path) -> PathBuf {
    let mut cur = p.to_path_buf();
    while !cur.exists() {
        if !cur.pop() {
            break;
        }
    }
    cur
}

/// The pure shape test, unit-tested separately (device identity is
/// un-fakeable in a unit test).
fn packed_shape(count: u64, total_bytes: u64) -> bool {
    count >= PACKED_MIN_FILES && total_bytes / count.max(1) <= PACKED_MAX_MEAN_BYTES
}

/// Choose the packed set for a job, or None when per-file is right.
/// Eligible Copy steps: Pending, no VerifyCopy dependency
/// (verify-then-skip keeps its own semantics), both paths present,
/// size at most PACKED_MAX_FILE_BYTES, and the source on a
/// different device than the destination parent. The SET must then
/// clear the count and mean-size thresholds.
fn packed_candidates(job: &ufm_jobs::Job) -> Option<Vec<PackedFile>> {
    let mut out: Vec<PackedFile> = Vec::new();
    for step in job.steps.values() {
        if !matches!(step.state, ufm_jobs::StepState::Pending) {
            continue;
        }
        let ufm_jobs::StepOp::Copy {
            src,
            dst_parent,
            new_name,
        } = &step.op
        else {
            continue;
        };
        let verify_gated = step.depends_on.iter().any(|d| {
            matches!(
                job.steps.get(d),
                Some(s) if matches!(s.op, ufm_jobs::StepOp::VerifyCopy { .. })
            )
        });
        if verify_gated {
            continue;
        }
        let (Some(src_key), Some(dst_key)) =
            (src.provider_key.as_ref(), dst_parent.provider_key.as_ref())
        else {
            continue;
        };
        let size = src.size_bytes.unwrap_or(0);
        if size > PACKED_MAX_FILE_BYTES {
            continue;
        }
        let src_path = Path::new(src_key);
        if same_device(src_path, &existing_ancestor(Path::new(dst_key))) {
            continue;
        }
        out.push(PackedFile {
            step_id: step.id.clone(),
            src: src_path.to_path_buf(),
            dst: Path::new(dst_key).join(new_name),
            size,
        });
    }
    let count = out.len() as u64;
    let total: u64 = out.iter().map(|f| f.size).sum();
    if !packed_shape(count, total) {
        return None;
    }
    // Deterministic stream order (steps live in a HashMap).
    out.sort_by(|a, b| a.step_id.cmp(&b.step_id));
    Some(out)
}

/// Write one ustar header for `name` — the step id: short, ASCII,
/// unique; the unpacker maps it back to the step's destination.
fn ustar_header(name: &str, size: u64, mode: u32) -> [u8; 512] {
    let mut h = [0u8; 512];
    let put_name = |h: &mut [u8; 512]| {
        let bytes = name.as_bytes();
        let n = bytes.len().min(100);
        h[..n].copy_from_slice(&bytes[..n]);
    };
    put_name(&mut h);
    let put_octal = |h: &mut [u8; 512], off: usize, width: usize, v: u64| {
        let s = format!("{:0width$o}", v, width = width - 1);
        h[off..off + width - 1].copy_from_slice(s.as_bytes());
        h[off + width - 1] = 0;
    };
    put_octal(&mut h, 100, 8, mode as u64);
    put_octal(&mut h, 108, 8, 0); // uid — not carried
    put_octal(&mut h, 116, 8, 0); // gid — not carried
    put_octal(&mut h, 124, 12, size);
    put_octal(&mut h, 136, 12, 0); // mtime — not carried; extraction is content-true
    h[156] = b'0'; // regular file
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    // Checksum over the block with the field treated as spaces.
    let sum: u64 = h
        .iter()
        .enumerate()
        .map(|(i, &b)| if (148..156).contains(&i) { b' ' as u64 } else { b as u64 })
        .sum();
    let chk = format!("{:06o}", sum);
    h[148..154].copy_from_slice(chk.as_bytes());
    h[154] = 0;
    h[155] = b' ';
    h
}

/// Parse one ustar header block. None = the end-of-archive zero
/// block (or a corrupt read — the pass then stops and the per-file
/// fallback takes the rest).
fn ustar_parse(block: &[u8; 512]) -> Option<(String, u64, u32)> {
    if block.iter().all(|&b| b == 0) {
        return None;
    }
    if &block[257..262] != b"ustar" {
        return None;
    }
    let field = |off: usize, len: usize| -> String {
        let raw = &block[off..off + len];
        let end = raw.iter().position(|&b| b == 0).unwrap_or(len);
        String::from_utf8_lossy(&raw[..end]).into_owned()
    };
    let octal = |off: usize, len: usize| -> u64 {
        let mut v: u64 = 0;
        for &b in &block[off..off + len] {
            match b {
                b'0'..=b'7' => v = v * 8 + (b - b'0') as u64,
                b' ' if v == 0 => {} // leading space padding
                _ => break,
            }
        }
        v
    };
    let name = field(0, 100);
    if name.is_empty() {
        return None;
    }
    Some((name, octal(124, 12), octal(100, 8) as u32))
}

/// The unpacker's view of the pipe: a byte stream reassembled from
/// bounded blocks.
struct BlockReader {
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    cur: Vec<u8>,
    pos: usize,
}

impl BlockReader {
    /// Fill `buf` completely; false on end of stream / broken pipe.
    fn read_exact(&mut self, buf: &mut [u8]) -> bool {
        let mut filled = 0;
        while filled < buf.len() {
            if self.pos >= self.cur.len() {
                match self.rx.recv() {
                    Ok(block) => {
                        self.cur = block;
                        self.pos = 0;
                    }
                    Err(_) => return false,
                }
                if self.cur.is_empty() {
                    continue;
                }
            }
            let n = (buf.len() - filled).min(self.cur.len() - self.pos);
            buf[filled..filled + n]
                .copy_from_slice(&self.cur[self.pos..self.pos + n]);
            self.pos += n;
            filled += n;
        }
        true
    }

    /// Stream exactly `n` bytes into `w`; None on a short stream.
    fn copy_n<W: std::io::Write>(&mut self, w: &mut W, mut n: u64) -> Option<u64> {
        let mut done = 0u64;
        while n > 0 {
            if self.pos >= self.cur.len() {
                match self.rx.recv() {
                    Ok(block) => {
                        self.cur = block;
                        self.pos = 0;
                    }
                    Err(_) => return None,
                }
                if self.cur.is_empty() {
                    continue;
                }
            }
            let take = n.min((self.cur.len() - self.pos) as u64) as usize;
            if w.write_all(&self.cur[self.pos..self.pos + take]).is_err() {
                return None;
            }
            self.pos += take;
            n -= take as u64;
            done += take as u64;
        }
        Some(done)
    }

    /// Discard `n` bytes (padding, or an unplaceable entry's body).
    fn skip_n(&mut self, mut n: u64) -> bool {
        while n > 0 {
            if self.pos >= self.cur.len() {
                match self.rx.recv() {
                    Ok(block) => {
                        self.cur = block;
                        self.pos = 0;
                    }
                    Err(_) => return false,
                }
                if self.cur.is_empty() {
                    continue;
                }
            }
            let take = n.min((self.cur.len() - self.pos) as u64) as usize;
            self.pos += take;
            n -= take as u64;
        }
        true
    }
}

/// The unpacker thread: consumes the ustar stream, extracts each
/// entry to the destination its step id maps to, and reports every
/// fully-extracted (length-verified) file back for journaling. Any
/// failure removes the in-flight partial file and ends the thread —
/// the caller treats unreported steps as not done (fallback law).
fn unpack_stream(
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    dests: std::sync::Arc<std::collections::HashMap<String, PathBuf>>,
    done_tx: std::sync::mpsc::SyncSender<String>,
) {
    use std::io::Write as _;
    let mut reader = BlockReader {
        rx,
        cur: Vec::new(),
        pos: 0,
    };
    let mut header = [0u8; 512];
    loop {
        if !reader.read_exact(&mut header) {
            return;
        }
        let Some((name, size, mode)) = ustar_parse(&header) else {
            return; // end-of-archive zero block, or corrupt header
        };
        let pad = (512 - size % 512) % 512;
        let Some(dst) = dests.get(&name) else {
            // Unknown entry — cannot place it; skip its body to stay
            // in frame and keep going.
            if !reader.skip_n(size + pad) {
                return;
            }
            continue;
        };
        let ok = (|| -> bool {
            if let Some(parent) = dst.parent() {
                if std::fs::create_dir_all(parent).is_err() {
                    return false;
                }
            }
            let mut out = match std::fs::File::create(dst) {
                Ok(f) => f,
                Err(_) => return false,
            };
            if reader.copy_n(&mut out, size) != Some(size) {
                return false;
            }
            if !reader.skip_n(pad) {
                return false;
            }
            let _ = out.flush();
            if mode != 0 {
                let _ = std::fs::set_permissions(
                    dst,
                    std::os::unix::fs::PermissionsExt::from_mode(mode),
                );
            }
            true
        })();
        if !ok {
            let _ = std::fs::remove_file(dst);
            return;
        }
        if done_tx.send(name).is_err() {
            return;
        }
    }
}

/// Run the packed pass over `files`: spawn the unpacker, stream the
/// files to it as ustar, and journal each reported extraction via
/// `transition_step` — the same commit the per-file loop uses, so
/// resume semantics are identical in both modes. Returns the number
/// of files committed; the rest stay Pending for the ordinary loop
/// (fallback law). Pause/cancel land between files.
fn run_packed_pass(
    app: &tauri::AppHandle,
    handle: &JobHandle,
    catalog: &ufm_catalog::Catalog,
    journal: &mut ufm_jobs::JobJournal,
    gate: &mut SnapshotGate,
    files: &[PackedFile],
    bytes_done: &mut u64,
) -> usize {
    use std::io::Read;
    let dests: std::sync::Arc<std::collections::HashMap<String, PathBuf>> =
        std::sync::Arc::new(
            files
                .iter()
                .map(|f| (f.step_id.clone(), f.dst.clone()))
                .collect(),
        );
    let (block_tx, block_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(8);
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<String>(64);
    let unpacker = {
        let dests = dests.clone();
        std::thread::spawn(move || unpack_stream(block_rx, dests, done_tx))
    };
    let mut committed = 0usize;
    let mut last_emit = std::time::Instant::now();
    // Journal every extraction the unpacker has reported so far.
    let mut drain = |journal: &mut ufm_jobs::JobJournal,
                     gate: &mut SnapshotGate,
                     bytes_done: &mut u64,
                     committed: &mut usize,
                     last_emit: &mut std::time::Instant,
                     force_emit: bool| {
        while let Ok(step_id) = done_rx.try_recv() {
            let fx = transition_step(
                handle,
                catalog,
                journal,
                gate,
                &step_id,
                ufm_jobs::StepState::Done,
                Some("packed".into()),
            );
            *bytes_done = fx.bytes_done;
            *committed += 1;
            if force_emit
                || last_emit.elapsed() >= std::time::Duration::from_millis(150)
            {
                emit_from_handle(app, handle, *bytes_done, Some("packed transfer"), None);
                *last_emit = std::time::Instant::now();
            }
        }
    };
    let mut send = |bytes: Vec<u8>| block_tx.send(bytes).is_ok();
    'stream: for f in files {
        if flag(&handle.cancel) {
            break 'stream;
        }
        if flag(&handle.pause) && !wait_while_paused(handle) {
            break 'stream; // cancelled while paused
        }
        // Stream the file's CURRENT length — a tree can grow between
        // planning and execution; the packed copy is content-true,
        // the planner's totals stay the progress denominator.
        let meta = match std::fs::metadata(&f.src) {
            Ok(m) => m,
            Err(_) => break 'stream, // vanished source — the per-file fallback owns its error
        };
        let size = meta.len();
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            meta.permissions().mode() & 0o7777
        };
        let mut input = match std::fs::File::open(&f.src) {
            Ok(x) => x,
            Err(_) => break 'stream,
        };
        if !send(ustar_header(&f.step_id, size, mode).to_vec()) {
            break 'stream;
        }
        let mut remaining = size;
        let mut buf = vec![0u8; PACKED_BLOCK];
        let mut short = false;
        while remaining > 0 {
            let want = (remaining as usize).min(PACKED_BLOCK);
            match input.read(&mut buf[..want]) {
                Ok(0) => {
                    short = true;
                    break;
                } // shrank mid-stream — stop the pass, stay honest
                Ok(n) => {
                    if !send(buf[..n].to_vec()) {
                        short = true;
                        break;
                    }
                    remaining -= n as u64;
                }
                Err(_) => {
                    short = true;
                    break;
                }
            }
        }
        if short {
            break 'stream;
        }
        let pad = (512 - size % 512) % 512;
        if pad > 0 && !send(vec![0u8; pad as usize]) {
            break 'stream;
        }
        drain(journal, gate, bytes_done, &mut committed, &mut last_emit, false);
    }
    drop(send);
    let _ = block_tx.send(vec![0u8; 1024]); // end-of-archive marker
    drop(block_tx);
    // Final sweep: everything the unpacker reported before exiting.
    let _ = unpacker.join();
    drain(journal, gate, bytes_done, &mut committed, &mut last_emit, true);
    committed
}

/// The executor: one blocking task per running job. Replays the durable
/// journal over the snapshot first (crash leftovers normalize Running ->
/// Pending), then runs every unfinished step in DAG order.
fn run_job(app: tauri::AppHandle, handle: JobHandle, catalog_path: PathBuf) {
    let job_id = handle.snapshot().id;
    let catalog = match ufm_catalog::Catalog::open(&catalog_path) {
        Ok(c) => c,
        Err(e) => {
            let snap = {
                let mut j = handle.job.lock().unwrap();
                j.state = ufm_jobs::JobState::Failed;
                j.clone()
            };
            emit_job_progress(
                &app,
                &snap,
                0,
                None,
                Some(&format!("catalog open failed: {e}")),
            );
            handle
                .live
                .store(false, std::sync::atomic::Ordering::SeqCst);
            return;
        }
    };
    let mut journal = ufm_jobs::JobJournal::with_sink(std::sync::Arc::new(
        ufm_catalog::CatalogJournalSink::new(catalog_path.clone()),
    ));
    if let Ok(entries) = catalog.journal_entries(&job_id) {
        let replay = ufm_jobs::JobJournal::from_entries(entries);
        let mut j = handle.job.lock().unwrap();
        replay.replay_into(&mut j);
    }
    // P0: the blob is checkpointed through this gate, never per
    // transition (see SnapshotGate).
    let mut gate = SnapshotGate::new();
    let mut bytes_done = job_bytes_done(&handle.snapshot());
    let snap = transition_job(&handle, &catalog, ufm_jobs::JobState::Running);
    emit_job_progress(&app, &snap, bytes_done, None, None);

    let order = match handle.snapshot().runnable_order() {
        Ok(o) => o,
        Err(e) => {
            let snap = transition_job(&handle, &catalog, ufm_jobs::JobState::Failed);
            emit_job_progress(&app, &snap, bytes_done, None, Some(&e.to_string()));
            handle
                .live
                .store(false, std::sync::atomic::Ordering::SeqCst);
            return;
        }
    };

    // Packed transfer (UI round): if this job's cross-device Copy
    // mass qualifies, stream it first. The loop below then simply
    // finds those steps Done and runs the rest — Mkdir steps,
    // Delete companions (a move's source dies only after its copy
    // committed), verify-gated and oversized copies.
    if let Some(files) = packed_candidates(&handle.snapshot()) {
        let snap = {
            let mut j = handle.job.lock().unwrap();
            j.packed = true;
            j.clone()
        };
        let _ = catalog.upsert_job(&snap);
        let snap = handle.snapshot();
        emit_job_progress(&app, &snap, bytes_done, Some("packed transfer"), None);
        run_packed_pass(
            &app,
            &handle,
            &catalog,
            &mut journal,
            &mut gate,
            &files,
            &mut bytes_done,
        );
    }

    for step_id in &order {
        let step = {
            let j = handle.job.lock().unwrap();
            j.steps.get(step_id).cloned()
        };
        let Some(step) = step else { continue };
        if matches!(
            step.state,
            ufm_jobs::StepState::Done
                | ufm_jobs::StepState::Skipped
                | ufm_jobs::StepState::Failed
        ) {
            continue;
        }
        if !wait_while_paused(&handle) {
            break; // cancelled while paused; finalize below
        }
        // A copy whose verify step was Skipped is itself Skipped:
        // the destination already holds the right bytes.
        if let ufm_jobs::StepOp::Copy { .. } = &step.op {
            let verify_skipped = {
                let j = handle.job.lock().unwrap();
                step.depends_on.iter().any(|d| {
                    matches!(
                        j.steps.get(d),
                        Some(s) if matches!(s.op, ufm_jobs::StepOp::VerifyCopy { .. })
                            && s.state == ufm_jobs::StepState::Skipped
                    )
                })
            };
            if verify_skipped {
                let fx = transition_step(
                    &handle,
                    &catalog,
                    &mut journal,
                    &mut gate,
                    step_id,
                    ufm_jobs::StepState::Skipped,
                    None,
                );
                bytes_done = fx.bytes_done;
                emit_from_handle(&app, &handle, bytes_done, None, None);
                continue;
            }
        }
        transition_step(
            &handle,
            &catalog,
            &mut journal,
            &mut gate,
            step_id,
            ufm_jobs::StepState::Running,
            None,
        );
        emit_from_handle(&app, &handle, bytes_done, Some(&step.label), None);
        let outcome = execute_step(&step.op, &handle, &app, &mut bytes_done);
        match outcome {
            StepOutcome::Done(note) => {
                let fx = transition_step(
                    &handle,
                    &catalog,
                    &mut journal,
                    &mut gate,
                    step_id,
                    ufm_jobs::StepState::Done,
                    note,
                );
                bytes_done = fx.bytes_done;
                emit_from_handle(&app, &handle, bytes_done, None, None);
            }
            StepOutcome::Skipped => {
                let fx = transition_step(
                    &handle,
                    &catalog,
                    &mut journal,
                    &mut gate,
                    step_id,
                    ufm_jobs::StepState::Skipped,
                    None,
                );
                bytes_done = fx.bytes_done;
                emit_from_handle(&app, &handle, bytes_done, None, None);
            }
            StepOutcome::Failed(msg) => {
                transition_step(
                    &handle,
                    &catalog,
                    &mut journal,
                    &mut gate,
                    step_id,
                    ufm_jobs::StepState::Failed,
                    Some(msg.clone()),
                );
                let snap =
                    transition_job(&handle, &catalog, ufm_jobs::JobState::Failed);
                emit_job_progress(&app, &snap, bytes_done, None, Some(&msg));
                handle
                    .live
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                return;
            }
            StepOutcome::Cancelled => {
                // The partial file is already removed by execute_step;
                // the step goes back to Pending so a later resume can
                // redo it cleanly.
                transition_step(
                    &handle,
                    &catalog,
                    &mut journal,
                    &mut gate,
                    step_id,
                    ufm_jobs::StepState::Pending,
                    Some("cancelled mid-step; partial output removed".into()),
                );
                break;
            }
        }
        if flag(&handle.cancel) {
            break;
        }
    }

    let final_state = if flag(&handle.cancel) {
        ufm_jobs::JobState::Cancelled
    } else {
        ufm_jobs::JobState::Completed
    };
    let snap = transition_job(&handle, &catalog, final_state);
    emit_job_progress(&app, &snap, job_bytes_done(&snap), None, None);
    handle
        .live
        .store(false, std::sync::atomic::Ordering::SeqCst);
}

/// Spawn the executor for a registered job, unless one is already live.
fn spawn_executor(app: tauri::AppHandle, state: &AppState, job: JobId) {
    let handle = {
        let jobs = state.jobs.lock().unwrap();
        jobs.get(&job).cloned()
    };
    let Some(handle) = handle else { return };
    if flag(&handle.live) {
        return;
    }
    handle
        .live
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let catalog_path = state.catalog_path.clone();
    tauri::async_runtime::spawn_blocking(move || run_job(app, handle, catalog_path));
}

fn job_handle(state: &AppState, job: JobId) -> Cmd<JobHandle> {
    state
        .jobs
        .lock()
        .unwrap()
        .get(&job)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound("unknown job".into())))
}

/// What a `start_*` command hands back (UI round): the job id, plus
/// whether the submission was folded into an identical job that is
/// already active — the double-submit guard below. Serialized
/// camelCase like the request structs (seam-pinned in the tests).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobStart {
    pub job_id: String,
    pub already_running: bool,
}

/// Identity of a submission: what makes two `start_*` calls the SAME
/// operation. Derived from the job's outcome steps (one per top-level
/// source): op kind + sorted source paths + destination. A decomposed
/// cross-device move plans Copy outcome steps plus Delete steps for
/// the originals — the Delete steps are what mark it as a move rather
/// than a copy. Jobs with no outcome sources (planner edge cases)
/// get a unique signature so the guard never fires on them.
fn job_signature(job: &ufm_jobs::Job) -> String {
    let key = |item: &Item| {
        item.provider_key
            .clone()
            .unwrap_or_else(|| item.name.clone())
    };
    let has_delete_step = job
        .steps
        .values()
        .any(|s| matches!(s.op, ufm_jobs::StepOp::Delete { .. }));
    let mut op = "job";
    let mut srcs: Vec<String> = Vec::new();
    let mut dst = String::new();
    for step in job.steps.values() {
        if !step.outcome {
            continue;
        }
        match &step.op {
            ufm_jobs::StepOp::Copy { src, dst_parent, .. } => {
                op = if has_delete_step { "move" } else { "copy" };
                srcs.push(key(src));
                dst = key(dst_parent);
            }
            ufm_jobs::StepOp::Move { src, dst_parent, .. } => {
                op = "move";
                srcs.push(key(src));
                dst = key(dst_parent);
            }
            ufm_jobs::StepOp::Delete { target, use_trash } => {
                op = if *use_trash { "trash" } else { "delete" };
                srcs.push(key(target));
            }
            ufm_jobs::StepOp::VerifyCopy { src, dst: dst_item } => {
                if op == "job" {
                    op = "copy";
                }
                srcs.push(key(src));
                if dst.is_empty() {
                    dst = key(dst_item);
                }
            }
            ufm_jobs::StepOp::Mkdir { .. } => {}
            ufm_jobs::StepOp::WriteFile { .. } => {}
            ufm_jobs::StepOp::FetchMedia { .. } => {}
            ufm_jobs::StepOp::FetchArtwork { .. } => {}
            ufm_jobs::StepOp::FetchSubtitle { .. } => {}
            ufm_jobs::StepOp::ConvertMedia { src, dst_name, kind } => {
                op = "convert";
                srcs.push(format!("{}>{dst_name}:{kind}", key(src)));
            }
        }
    }
    if srcs.is_empty() {
        return format!("unique:{}", job.id);
    }
    srcs.sort();
    srcs.dedup();
    format!("{op}|{}|{dst}", srcs.join("\u{1f}"))
}

/// The shared tail of every `start_*` command (inc-2 §2.1):
/// journal-first upsert → register the handle → spawn the executor.
/// The snapshot is durable before any step runs, so even a crash
/// mid-operation leaves a resumable record.
///
/// UI round — double-submit guard (Dickles' live incident,
/// 2026-10-14: a double-pressed copy shortcut spawned TWO identical
/// jobs that crawled, both writing the same destination tree). An
/// identical submission while a job is Queued/Running/Paused folds
/// into the existing job: no twin is planned into the registry, the
/// existing job's progress is re-emitted so the panel surfaces it,
/// and the caller learns it was a duplicate so it can say so.
pub(crate) fn start_job(
    app: tauri::AppHandle,
    state: &AppState,
    job: ufm_jobs::Job,
) -> Cmd<JobStart> {
    let sig = job_signature(&job);
    let mut jobs = state.jobs.lock().unwrap();
    for (id, handle) in jobs.iter() {
        let snap = handle.snapshot();
        if matches!(
            snap.state,
            ufm_jobs::JobState::Queued
                | ufm_jobs::JobState::Running
                | ufm_jobs::JobState::Paused
        ) && job_signature(&snap) == sig
        {
            let existing_id = *id;
            let bytes = job_bytes_done(&snap);
            drop(jobs);
            emit_job_progress(&app, &snap, bytes, None, None);
            return Ok(JobStart {
                job_id: existing_id.to_string(),
                already_running: true,
            });
        }
    }
    let id = job.id;
    state.catalog.lock().unwrap().upsert_job(&job)?;
    jobs.insert(id, JobHandle::new(job, false));
    drop(jobs);
    spawn_executor(app, state, id);
    Ok(JobStart {
        job_id: id.to_string(),
        already_running: false,
    })
}

/// Resolve id-based sources to paths through the live-browse caches
/// (mirrors `build_copy_job`; `NotFound` on unknown ids). The job's
/// location is the first source's location.
fn resolve_source_paths(
    state: &AppState,
    ids: &[ItemId],
) -> Cmd<(Vec<PathBuf>, LocationId)> {
    let paths = state.item_paths.lock().unwrap();
    let locs = state.item_locations.lock().unwrap();
    let mut srcs = Vec::new();
    let mut location = None;
    for id in ids {
        let p = paths.get(id).cloned().ok_or_else(|| {
            CommandError::from(UfmError::NotFound("unknown source item".into()))
        })?;
        if location.is_none() {
            location = locs.get(id).copied();
        }
        srcs.push(p);
    }
    Ok((srcs, location.unwrap_or_default()))
}

/// Best-effort location for a path-based request: the location of a
/// listed item at the same path, else the default location.
fn location_for_path(state: &AppState, path: &Path) -> LocationId {
    let paths = state.item_paths.lock().unwrap();
    let locs = state.item_locations.lock().unwrap();
    for (id, p) in paths.iter() {
        if p.as_path() == path {
            if let Some(l) = locs.get(id) {
                return *l;
            }
        }
    }
    LocationId::default()
}

#[tauri::command]
pub async fn start_copy(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    req: CopyRequest,
) -> Cmd<JobStart> {
    let job = build_copy_job(&state, &req)?;
    start_job(app, &state, job)
}

// Inc-2 (§2.1): move/delete requests. Note the deliberate asymmetry
// with CopyRequest — every move/delete call site in the UI already
// works with a destination PATH string, so these take `dst_dir`
// rather than a destination ItemId. Do not retrofit CopyRequest.

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveRequest {
    pub sources: Vec<ItemId>,
    pub dst_dir: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MovePathsRequest {
    pub sources: Vec<String>,
    pub dst_dir: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyPathsRequest {
    pub sources: Vec<String>,
    pub dst_dir: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteRequest {
    pub sources: Vec<ItemId>,
    pub use_trash: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletePathsRequest {
    pub paths: Vec<String>,
    pub use_trash: bool,
}

#[tauri::command]
pub async fn start_move(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    req: MoveRequest,
) -> Cmd<JobStart> {
    let (srcs, location) = resolve_source_paths(&state, &req.sources)?;
    let job = plan_move_job(&srcs, Path::new(&req.dst_dir), location)?;
    start_job(app, &state, job)
}

#[tauri::command]
pub async fn start_move_from_paths(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    req: MovePathsRequest,
) -> Cmd<JobStart> {
    let srcs: Vec<PathBuf> = req.sources.iter().map(PathBuf::from).collect();
    let location = location_for_path(&state, Path::new(&req.dst_dir));
    let job = plan_move_job(&srcs, Path::new(&req.dst_dir), location)?;
    start_job(app, &state, job)
}

#[tauri::command]
pub async fn start_copy_from_paths(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    req: CopyPathsRequest,
) -> Cmd<JobStart> {
    let srcs: Vec<PathBuf> = req.sources.iter().map(PathBuf::from).collect();
    let location = location_for_path(&state, Path::new(&req.dst_dir));
    let job = plan_copy_job_paths(&srcs, Path::new(&req.dst_dir), location)?;
    start_job(app, &state, job)
}

#[tauri::command]
pub async fn start_delete(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    req: DeleteRequest,
) -> Cmd<JobStart> {
    let (srcs, location) = resolve_source_paths(&state, &req.sources)?;
    let job = plan_delete_job(&srcs, req.use_trash, location)?;
    start_job(app, &state, job)
}

#[tauri::command]
pub async fn start_delete_from_paths(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    req: DeletePathsRequest,
) -> Cmd<JobStart> {
    let srcs: Vec<PathBuf> = req.paths.iter().map(PathBuf::from).collect();
    let location = srcs
        .first()
        .map(|p| location_for_path(&state, p))
        .unwrap_or_default();
    let job = plan_delete_job(&srcs, req.use_trash, location)?;
    start_job(app, &state, job)
}

/// Panel-facing summary of one registered job (§2.1).
#[derive(Debug, Serialize)]
pub struct JobSummary {
    pub job_id: String,
    pub title: String,
    pub state: String,
    pub steps_done: usize,
    pub steps_total: usize,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// UI round (packed transfer): the card's "Packed transfer" label.
    pub packed: bool,
}

#[tauri::command]
pub async fn list_jobs(state: State<'_, AppState>) -> Cmd<Vec<JobSummary>> {
    let mut snaps: Vec<ufm_jobs::Job> = {
        let jobs = state.jobs.lock().unwrap();
        jobs.values().map(|h| h.snapshot()).collect()
    };
    snaps.sort_by_key(|j| j.created);
    Ok(snaps
        .iter()
        .map(|j| {
            // Slimmed snapshots (Indexer v2 hygiene) carry tallies
            // instead of the steps map — report from those so the
            // card stays truthful after slimming.
            let (steps_done, steps_total) = if j.steps.is_empty() {
                match &j.step_counts {
                    Some(c) => ((c.done + c.skipped) as usize, c.total as usize),
                    None => (0, 0),
                }
            } else {
                (
                    j.steps
                        .values()
                        .filter(|s| {
                            matches!(
                                s.state,
                                ufm_jobs::StepState::Done | ufm_jobs::StepState::Skipped
                            )
                        })
                        .count(),
                    j.steps.len(),
                )
            };
            JobSummary {
                job_id: j.id.to_string(),
                title: j.title.clone(),
                state: job_state_name(j.state).to_string(),
                steps_done,
                steps_total,
                bytes_done: job_bytes_done(j),
                bytes_total: j.preview.bytes_to_move,
                packed: j.packed,
            }
        })
        .collect())
}

/// The per-source outcome records of a job (§2.2): the frontend's
/// undo feed at completion time.
#[tauri::command]
pub async fn get_job_outcomes(
    state: State<'_, AppState>,
    job: JobId,
) -> Cmd<Vec<ufm_jobs::JobOutcome>> {
    let handle = job_handle(&state, job)?;
    Ok(handle.snapshot().outcomes)
}

#[tauri::command]
pub async fn pause_job(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    job: JobId,
) -> Cmd<()> {
    let handle = job_handle(&state, job)?;
    // P0: pause is a flag + a state + an emit — all O(1)-ish, no job
    // clone, no blob write. The executor polls the flag (Copy checks
    // it every 1 MiB chunk and between steps), so the effect lands
    // within a second at ANY DAG size. Durability: the executor's
    // snapshot gate checkpoints within seconds while it runs, and
    // if the app dies first, startup recovery replays the journal
    // over the checkpoint and normalizes Running -> Paused anyway.
    handle
        .pause
        .store(true, std::sync::atomic::Ordering::SeqCst);
    {
        let mut j = handle.job.lock().unwrap();
        if matches!(
            j.state,
            ufm_jobs::JobState::Running | ufm_jobs::JobState::Queued
        ) {
            j.state = ufm_jobs::JobState::Paused;
        }
    }
    let bytes = locked_bytes_done(&handle);
    emit_from_handle(&app, &handle, bytes, None, None);
    Ok(())
}

#[tauri::command]
pub async fn resume_job(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    job: JobId,
) -> Cmd<()> {
    let handle = job_handle(&state, job)?;
    {
        let j = handle.job.lock().unwrap();
        if matches!(
            j.state,
            ufm_jobs::JobState::Completed
                | ufm_jobs::JobState::Cancelled
                | ufm_jobs::JobState::Failed
        ) {
            return Ok(()); // terminal jobs stay terminal
        }
    }
    handle
        .pause
        .store(false, std::sync::atomic::Ordering::SeqCst);
    // If an executor is live it sees the cleared flag; if not (paused
    // at shutdown, recovered at startup), spawn one — it skips every
    // step already Done, so resume is just "run what's left".
    spawn_executor(app, &state, job);
    Ok(())
}

/// One unconditional checkpoint write of the handle's job blob.
fn checkpoint_handle(handle: &JobHandle, catalog: &ufm_catalog::Catalog) {
    let snap = handle.snapshot();
    let _ = catalog.upsert_job(&snap);
}

/// Settle a job to Cancelled without an executor (P0): a recovered
/// or never-started job has nobody to consume the cancel flag, so
/// the command settles it directly — terminal state in memory plus
/// one checkpoint write.
fn settle_cancelled(handle: &JobHandle, catalog: &ufm_catalog::Catalog) {
    {
        let mut j = handle.job.lock().unwrap();
        j.state = ufm_jobs::JobState::Cancelled;
    }
    checkpoint_handle(handle, catalog);
}

#[tauri::command]
pub async fn cancel_job(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    job: JobId,
) -> Cmd<()> {
    let handle = job_handle(&state, job)?;
    handle
        .cancel
        .store(true, std::sync::atomic::Ordering::SeqCst);
    handle
        .pause
        .store(false, std::sync::atomic::Ordering::SeqCst);
    if !flag(&handle.live) {
        // No executor to finalize it (e.g. a recovered job): settle
        // in place — emit first so the card clears immediately.
        {
            let mut j = handle.job.lock().unwrap();
            j.state = ufm_jobs::JobState::Cancelled;
        }
        let bytes = locked_bytes_done(&handle);
        emit_from_handle(&app, &handle, bytes, None, None);
        checkpoint_handle(&handle, &state.catalog.lock().unwrap());
    } else {
        // Live executor: the flag lands at the next chunk/step
        // boundary (P0: boundaries arrive constantly now) and the
        // executor finalizes + emits the terminal state. Emit once
        // now so the click visibly registers.
        let bytes = locked_bytes_done(&handle);
        emit_from_handle(&app, &handle, bytes, None, None);
    }
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
    /// Library Memory: the volume this file lives on (its display
    /// label), when the path resolves to a known volume. Files on
    /// disconnected drives stay in the results — this + the two
    /// fields below are how the UI badges them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume_label: Option<String>,
    /// True when that volume is not currently connected.
    pub volume_offline: bool,
    /// RFC 3339: when that volume was last seen mounted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume_last_seen: Option<String>,
}

/// File-type categories for the advanced-search filter.
pub(crate) fn type_extensions(filter: &str) -> Option<&'static [&'static str]> {
    match filter {
        "image" => Some(&["png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "svg", "ico", "avif"][..]),
        "video" => Some(&["mp4", "mkv", "webm", "mov", "avi", "m4v", "mpg", "mpeg"][..]),
        "audio" => Some(&["mp3", "flac", "ogg", "oga", "wav", "m4a", "m4b", "opus", "aac"][..]),
        "document" => Some(&["pdf", "doc", "docx", "odt", "epub", "txt", "md", "rtf", "csv", "tsv"][..]),
        "archive" => Some(&["zip", "7z", "rar", "tar", "gz", "bz2", "xz", "tgz"][..]),
        // UI round (Dickles: "I'm old"): mail files are a first-class
        // search type — .eml messages, Outlook .msg, .mbox archives.
        "email" => Some(&["eml", "msg", "mbox"][..]),
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
    path_prefix: Option<String>,
    modified_after: Option<String>,
    modified_before: Option<String>,
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
            return finish_search(state, rows, scope, path_prefix.as_deref(), modified_after.as_deref(), modified_before.as_deref());
        }
        return Ok(Vec::new());
    }

    // A query that doesn't parse is not an error — it just matches nothing.
    let query = match ufm_search::parse(&query_text) {
        Ok(q) => q,
        Err(_) => return Ok(Vec::new()),
    };
    // User-metadata facets (label:, starred:) are NOT name text — they
    // execute against the catalog stores, either as a post-filter over
    // FTS rows or, when no name terms remain, as the candidate seed.
    let (label_facets, starred_facet) = ufm_search::metadata_facets(&query);
    let has_metadata = !label_facets.is_empty() || starred_facet.is_some();
    let fts_query = ufm_search::to_fts(&query);
    if fts_query.trim().is_empty() {
        if !has_metadata {
            return Ok(Vec::new());
        }
        // Metadata-only query: seed from the label/star stores, apply
        // the remaining facets as filters, then the shared tail.
        let seeded = {
            let catalog = state.catalog.lock().unwrap();
            let mut items = if let Some(first) = label_facets.first() {
                catalog
                    .items_by_label(first)
                    .map_err(CommandError::from)?
            } else {
                catalog.starred_items().map_err(CommandError::from)?
            };
            items.retain(|item| {
                metadata_match(&catalog, item, &label_facets, starred_facet)
            });
            if let Some(exts) = exts {
                items.retain(|item| name_in_type(&item.name, exts));
            }
            items.truncate(limit as usize);
            items
                .into_iter()
                .map(|item| (item, 0.0))
                .collect::<Vec<(Item, f64)>>()
        };
        return finish_search(state, seeded, scope, path_prefix.as_deref(), modified_after.as_deref(), modified_before.as_deref());
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
    if has_metadata {
        let catalog = state.catalog.lock().unwrap();
        rows.retain(|(item, _)| {
            metadata_match(&catalog, item, &label_facets, starred_facet)
        });
    }
    finish_search(state, rows, scope, path_prefix.as_deref(), modified_after.as_deref(), modified_before.as_deref())
}

/// Does an item satisfy the extracted metadata facets? Label facets
/// are ANDed (the item must carry every label, case-insensitively);
/// the starred facet compares against `path_stars`.
pub(crate) fn metadata_match(
    catalog: &ufm_catalog::Catalog,
    item: &Item,
    labels: &[String],
    starred: Option<bool>,
) -> bool {
    let Some(pk) = item.provider_key.as_deref() else {
        return labels.is_empty() && starred != Some(true);
    };
    for want in labels {
        match catalog.labels_for_path(pk) {
            Ok(have) => {
                if !have.iter().any(|h| h.eq_ignore_ascii_case(want)) {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
    if let Some(want) = starred {
        if catalog.is_starred_path(pk).unwrap_or(false) != want {
            return false;
        }
    }
    true
}

/// Shared tail of `search`: apply the location scope, cache result paths
/// so preview/list_dir can resolve the hits, and shape the response.
fn finish_search(
    state: State<'_, AppState>,
    rows: Vec<(Item, f64)>,
    scope: Option<LocationId>,
    path_prefix: Option<&str>,
    modified_after: Option<&str>,
    modified_before: Option<&str>,
) -> Cmd<Vec<SearchHit>> {
    let mut rows: Vec<(Item, f64)> = rows
        .into_iter()
        .filter(|(item, _)| {
            if let Some(p) = path_prefix {
                let key = item.provider_key.as_deref().unwrap_or("");
                if !key.starts_with(p) {
                    return false;
                }
            }
            // Item.modified is RFC 3339: its first 10 chars are the date,
            // which compares correctly against yyyy-mm-dd filter bounds.
            let day = item.modified.map(|m| m.format("%Y-%m-%d").to_string()).unwrap_or_default();
            if let Some(a) = modified_after {
                if !a.is_empty() && day.as_str() < a {
                    return false;
                }
            }
            if let Some(b) = modified_before {
                if !b.is_empty() && day.as_str() > b {
                    return false;
                }
            }
            true
        })
        .collect();
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

    // Hydrate user metadata (labels + stars) onto the hits — the
    // FTS rows carry neither, and the UI renders both.
    {
        let catalog = state.catalog.lock().unwrap();
        let keys: Vec<String> = rows
            .iter()
            .filter_map(|(i, _)| i.provider_key.clone())
            .collect();
        let starred = catalog.starred_paths(&keys).unwrap_or_default();
        for (item, _) in rows.iter_mut() {
            if let Some(pk) = &item.provider_key {
                item.labels = catalog.labels_for_path(pk).unwrap_or_default();
                item.starred = starred.contains(pk);
            }
        }
    }

    // Library Memory: resolve each hit's volume once per search
    // (one enumeration + one volumes read, not per hit).
    let mounted = current_filesystems();
    let (volumes, network_records) = {
        let catalog = state.catalog.lock().unwrap();
        (
            catalog.volumes().unwrap_or_default(),
            catalog.network_locations().unwrap_or_default(),
        )
    };
    let live_net = crate::network::live_mounts();
    Ok(rows
        .into_iter()
        .map(|(item, rank)| {
            let badge = item.provider_key.as_deref().and_then(|pk| {
                volume_badge_for_path(&mounted, &volumes, pk)
                    .or_else(|| network_badge_for_path(&network_records, &live_net, pk))
            });
            SearchHit {
                item,
                rank,
                volume_label: badge.as_ref().map(|b| b.label.clone()),
                volume_offline: badge.as_ref().is_some_and(|b| b.offline),
                volume_last_seen: badge.and_then(|b| b.last_seen),
            }
        })
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
    // never a duplicate walk. Re-runs are incremental (Indexer v2): the
    // walk diffs against the catalog manifest, so an unchanged library
    // costs a traversal, not a rewrite.
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
    let root_for_mark = root.clone();
    // The background task re-enters state through the owned AppHandle, so the
    // borrow of `state` never escapes this command.
    tauri::async_runtime::spawn(async move {
        let progress_app = app.clone();
        let indexed = tauri::async_runtime::spawn_blocking(move || {
            let mut indexer = ufm_search::Indexer::open(&catalog_path, &checkpoint_path)?;
            // Freeze fix (2026-10-16): the emit into the WebView is
            // coalesced to at most one per 400 ms. The callback itself
            // still updates index_state on every call (index_status
            // polls read the truth), but each emit crosses the glib
            // main-context channel and wakes the renderer — during the
            // 59k-dir Home walk the un-coalesced stream was part of
            // the saturation that froze the window. The completion
            // path below emits the final state unconditionally.
            let last_emit = std::sync::Arc::new(std::sync::Mutex::new(
                std::time::Instant::now() - std::time::Duration::from_secs(1),
            ));
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
                        index.files_unchanged = stats.files_unchanged;
                        index.files_updated = stats.files_updated;
                        index.files_removed = stats.files_removed;
                    }
                    index.clone()
                };
                {
                    let mut last = last_emit.lock().unwrap();
                    if last.elapsed() < std::time::Duration::from_millis(400) {
                        return;
                    }
                    *last = std::time::Instant::now();
                }
                let _ = progress_app.emit("index-progress", &payload);
            }));
            indexer.index_root(&root, &location_rec)
        })
        .await;
        let state = app.state::<AppState>();
        let mut run_ok = false;
        {
            let mut index = state.index_state.lock().unwrap();
            match indexed {
                Ok(Ok(stats)) => {
                    run_ok = true;
                    *index = IndexState {
                        running: false,
                        dirs_done: stats.dirs_done,
                        files_done: stats.files_done,
                        bytes_done: stats.bytes_done,
                        files_unchanged: stats.files_unchanged,
                        files_updated: stats.files_updated,
                        files_removed: stats.files_removed,
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
        if run_ok {
            // Library Memory: the volume this root lives on was just
            // indexed — refresh its sighting and stamp last_indexed.
            let catalog = state.catalog.lock().unwrap();
            let mounted = record_volume_sightings(&catalog);
            let root_s = root_for_mark.to_string_lossy().into_owned();
            let mut best: Option<&MountedVolume> = None;
            for v in &mounted {
                if path_under(&root_s, &v.path)
                    && best.is_none_or(|b| v.path.len() > b.path.len())
                {
                    best = Some(v);
                }
            }
            if let Some(uuid) = best.and_then(|v| v.uuid.clone()) {
                let _ = catalog
                    .mark_volume_indexed(&uuid, &chrono::Utc::now().to_rfc3339());
            }
            // Network Locations: the same completion mark when the
            // indexed root is a server's FUSE mount (its prefix is in
            // the record's mount history by construction).
            if let Ok(records) = catalog.network_locations() {
                if let Some(rec) = records.iter().find(|r| {
                    r.mount_history.iter().any(|h| path_under(&root_s, &h.path))
                }) {
                    let _ = catalog
                        .mark_network_indexed(&rec.uri, &chrono::Utc::now().to_rfc3339());
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

/// Resolve a UI item to its provider_key (the absolute path for
/// local files) — the identity every Phase 2 metadata store keys on.
fn item_provider_key(state: &AppState, item: ItemId) -> Cmd<String> {
    let paths = state.item_paths.lock().unwrap();
    paths
        .get(&item)
        .map(|p| p.to_string_lossy().into_owned())
        .ok_or_else(|| {
            CommandError::from(UfmError::NotFound(format!("unknown item {item}")))
        })
}

/// Reconcile catalog-resolved items with the shell's stable path-id
/// cache (the `list_items_by_label` pattern), so follow-up commands
/// (open, trash, star, …) resolve the same ids the UI already holds.
fn reconcile_item_ids(state: &AppState, found: Vec<Item>) -> Vec<Item> {
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
    out
}

fn open_catalog(state: &AppState) -> Cmd<ufm_catalog::Catalog> {
    ufm_catalog::Catalog::open(&state.catalog_path).map_err(CommandError::from)
}

/// Apply a label to an item. Labels live in the path-keyed catalog
/// store (the same one the sidebar Labels system writes), so a tag
/// applied here is the label seen everywhere.
#[tauri::command]
pub async fn tag_item(
    state: State<'_, AppState>,
    item: ItemId,
    label: String,
) -> Cmd<()> {
    let pk = item_provider_key(&state, item)?;
    open_catalog(&state)?
        .tag_path(&pk, &label)
        .map_err(CommandError::from)?;
    Ok(())
}

/// Remove a label from an item (the untag half of `tag_item`).
#[tauri::command]
pub async fn untag_item(
    state: State<'_, AppState>,
    item: ItemId,
    label: String,
) -> Cmd<()> {
    let pk = item_provider_key(&state, item)?;
    open_catalog(&state)?
        .untag_path(&pk, &label)
        .map_err(CommandError::from)?;
    Ok(())
}

/// Star/unstar an item; returns the NEW starred state. Stars persist
/// in the catalog (`path_stars`) and render as ★ in every list.
#[tauri::command]
pub async fn toggle_star(state: State<'_, AppState>, item: ItemId) -> Cmd<bool> {
    let pk = item_provider_key(&state, item)?;
    open_catalog(&state)?
        .toggle_star_path(&pk)
        .map_err(CommandError::from)
}

/// Every starred item in the catalog (the sidebar Starred view).
#[tauri::command]
pub async fn starred_items(state: State<'_, AppState>) -> Cmd<Vec<Item>> {
    let found = open_catalog(&state)?
        .starred_items()
        .map_err(CommandError::from)?;
    Ok(reconcile_item_ids(&state, found))
}

#[derive(Debug, Serialize)]
pub struct CollectionInfo {
    pub id: CollectionId,
    pub name: String,
    /// manual | saved_search | smart
    pub kind: String,
    pub member_count: u64,
    /// Members whose path no longer resolves to a catalog item.
    pub missing_count: u64,
}

fn collection_info(row: ufm_catalog::CollectionRow) -> Cmd<CollectionInfo> {
    let id = uuid::Uuid::parse_str(&row.id)
        .map(CollectionId)
        .map_err(|e| CommandError::from(UfmError::Internal(e.to_string())))?;
    Ok(CollectionInfo {
        id,
        name: row.name,
        kind: row.kind,
        member_count: row.member_count,
        missing_count: row.missing_count,
    })
}

/// All collections with live membership counts (sidebar list).
#[tauri::command]
pub async fn list_collections(
    state: State<'_, AppState>,
) -> Cmd<Vec<CollectionInfo>> {
    let rows = open_catalog(&state)?
        .list_collections()
        .map_err(CommandError::from)?;
    rows.into_iter().map(collection_info).collect()
}

#[tauri::command]
pub async fn get_collection(
    state: State<'_, AppState>,
    id: CollectionId,
) -> Cmd<CollectionInfo> {
    let rows = open_catalog(&state)?
        .list_collections()
        .map_err(CommandError::from)?;
    rows.into_iter()
        .find(|r| r.id == id.to_string())
        .ok_or_else(|| {
            CommandError::from(UfmError::NotFound(format!("unknown collection {id}")))
        })
        .and_then(collection_info)
}

/// Create a manual collection; returns its id.
#[tauri::command]
pub async fn create_collection(
    state: State<'_, AppState>,
    name: String,
) -> Cmd<CollectionId> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(CommandError::from(UfmError::InvalidInput(
            "collection name is empty".into(),
        )));
    }
    let id = CollectionId::new();
    open_catalog(&state)?
        .create_collection(&id, &name, "manual")
        .map_err(CommandError::from)?;
    Ok(id)
}

#[tauri::command]
pub async fn rename_collection(
    state: State<'_, AppState>,
    id: CollectionId,
    name: String,
) -> Cmd<()> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(CommandError::from(UfmError::InvalidInput(
            "collection name is empty".into(),
        )));
    }
    open_catalog(&state)?
        .rename_collection(&id, &name)
        .map_err(CommandError::from)?;
    Ok(())
}

/// Delete a collection. Member files are never touched — only the
/// membership list goes away.
#[tauri::command]
pub async fn delete_collection(
    state: State<'_, AppState>,
    id: CollectionId,
) -> Cmd<()> {
    open_catalog(&state)?
        .delete_collection(&id)
        .map_err(CommandError::from)?;
    Ok(())
}

#[tauri::command]
pub async fn add_to_collection(
    state: State<'_, AppState>,
    id: CollectionId,
    items: Vec<ItemId>,
) -> Cmd<()> {
    let catalog = open_catalog(&state)?;
    for item in items {
        let pk = item_provider_key(&state, item)?;
        catalog
            .add_collection_member(&id, &pk)
            .map_err(CommandError::from)?;
    }
    Ok(())
}

#[tauri::command]
pub async fn remove_from_collection(
    state: State<'_, AppState>,
    id: CollectionId,
    items: Vec<ItemId>,
) -> Cmd<()> {
    let catalog = open_catalog(&state)?;
    for item in items {
        let pk = item_provider_key(&state, item)?;
        catalog
            .remove_collection_member(&id, &pk)
            .map_err(CommandError::from)?;
    }
    Ok(())
}

/// Remove members by raw path — the way out for *missing* members,
/// whose items no longer exist to be resolved by id.
#[tauri::command]
pub async fn remove_collection_paths(
    state: State<'_, AppState>,
    id: CollectionId,
    paths: Vec<String>,
) -> Cmd<()> {
    let catalog = open_catalog(&state)?;
    for pk in paths {
        catalog
            .remove_collection_member(&id, &pk)
            .map_err(CommandError::from)?;
    }
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct CollectionContents {
    pub items: Vec<Item>,
    /// Member paths that no longer resolve (moved/deleted elsewhere).
    pub missing: Vec<String>,
}

/// A collection's contents: resolved items plus the honest list of
/// member paths that no longer resolve to a file.
#[tauri::command]
pub async fn collection_items(
    state: State<'_, AppState>,
    id: CollectionId,
) -> Cmd<CollectionContents> {
    let (items, missing) = open_catalog(&state)?
        .collection_items(&id)
        .map_err(CommandError::from)?;
    Ok(CollectionContents {
        items: reconcile_item_ids(&state, items),
        missing,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewProfileDto {
    /// grid | list | columns | gallery
    pub view_mode: String,
    /// name | size | modified
    pub sort_key: String,
    /// 1 ascending, -1 descending.
    pub sort_dir: i32,
    pub show_extensions: bool,
}

/// The remembered view profile for a location, if one was saved.
#[tauri::command]
pub async fn get_view_profile(
    state: State<'_, AppState>,
    location: LocationId,
) -> Cmd<Option<ViewProfileDto>> {
    let row = open_catalog(&state)?
        .view_profile(&location)
        .map_err(CommandError::from)?;
    Ok(row.map(|r| {
        let sort: serde_json::Value =
            serde_json::from_str(&r.sort_json).unwrap_or_default();
        ViewProfileDto {
            view_mode: r.view_mode,
            sort_key: sort
                .get("key")
                .and_then(|v| v.as_str())
                .unwrap_or("name")
                .to_string(),
            sort_dir: sort.get("dir").and_then(|v| v.as_i64()).unwrap_or(1) as i32,
            show_extensions: r.show_extensions,
        }
    }))
}

/// Remember how a location likes to be looked at (per-location view
/// memory: mode + sort, restored on the next visit).
#[tauri::command]
pub async fn set_view_profile(
    state: State<'_, AppState>,
    location: LocationId,
    profile: ViewProfileDto,
) -> Cmd<()> {
    let sort_json = serde_json::json!({ "key": profile.sort_key, "dir": profile.sort_dir })
        .to_string();
    open_catalog(&state)?
        .set_view_profile(
            &location,
            &ufm_catalog::ViewProfileRow {
                view_mode: profile.view_mode,
                sort_json,
                show_extensions: profile.show_extensions,
            },
        )
        .map_err(CommandError::from)?;
    Ok(())
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
    // Library Memory: a file on a disconnected drive gets a
    // plain-language answer, never a dead click or a raw OS error.
    if !path.exists() {
        let path_s = path.to_string_lossy().into_owned();
        let (mounted, volumes, network_records) = {
            let catalog = state.catalog.lock().unwrap();
            (
                current_filesystems(),
                catalog.volumes().unwrap_or_default(),
                catalog.network_locations().unwrap_or_default(),
            )
        };
        let badge = volume_badge_for_path(&mounted, &volumes, &path_s).or_else(|| {
            network_badge_for_path(&network_records, &crate::network::live_mounts(), &path_s)
        });
        if let Some(badge) = badge {
            if badge.offline {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path_s.clone());
                let seen = badge
                    .last_seen
                    .as_deref()
                    .map(human_date)
                    .unwrap_or_else(|| "a while ago".to_string());
                return Err(CommandError::from(UfmError::NotFound(format!(
                    "“{name}” is on “{label}”, which isn't connected right now (last seen {seen}). Connect it and try again.",
                    label = badge.label
                ))));
            }
        }
    }
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


/// Extract one member of a zip-based file (docx/odt/epub) as text,
/// via the bsdtar the app already shells to for archive browsing.
fn bsdtar_extract(path: &std::path::Path, member: &str) -> std::result::Result<String, String> {
    let out = run_with_timeout(
        "bsdtar",
        &["-xOf", &path.to_string_lossy(), member],
        20,
    )?;
    if out.is_empty() {
        return Err(format!("{member} not found"));
    }
    Ok(out)
}

/// Strip XML/HTML markup down to readable text: block-level closing
/// tags (paragraphs, headings, list items, table rows) become line
/// breaks, tab elements become tabs, entities are unescaped.
fn xml_to_text(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len() / 2);
    let mut in_tag = false;
    let mut tag = String::new();
    for ch in xml.chars() {
        match ch {
            '<' => {
                in_tag = true;
                tag.clear();
            }
            '>' => {
                in_tag = false;
                let t = tag.trim_start();
                if t.starts_with("/p")
                    || t.starts_with("/h")
                    || t.starts_with("/li")
                    || t.starts_with("/tr")
                    || t.starts_with("/w:p")
                    || t.starts_with("/text:p")
                    || t.starts_with("/text:h")
                    || t.starts_with("br")
                {
                    out.push('\n');
                } else if t.starts_with("w:tab") {
                    out.push('\t');
                }
            }
            _ => {
                if in_tag {
                    tag.push(ch);
                } else {
                    out.push(ch);
                }
            }
        }
    }
    // Unescape (&amp; last so one level survives: &amp;lt; → &lt;).
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Readable-text extraction for office / ebook formats (Round 7.1).
fn extract_document_text(path: &std::path::Path) -> std::result::Result<String, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "pdf" => run_with_timeout(
            "pdftotext",
            &["-layout", &path.to_string_lossy(), "-"],
            30,
        ),
        "docx" => Ok(xml_to_text(&bsdtar_extract(path, "word/document.xml")?)),
        // ODF family (Writer / Calc / Impress) all carry body text in
        // content.xml — one mechanism serves all three (Round 7.2).
        "odt" | "ods" | "odp" => Ok(xml_to_text(&bsdtar_extract(path, "content.xml")?)),
        "epub" => {
            // XHTML members in archive order (the spine order for
            // practical purposes); container metadata is skipped.
            let listing = run_with_timeout(
                "bsdtar",
                &["-tf", &path.to_string_lossy()],
                20,
            )?;
            let mut text = String::new();
            for member in listing.lines() {
                let m = member.trim();
                let lower = m.to_lowercase();
                if (lower.ends_with(".xhtml")
                    || lower.ends_with(".html")
                    || lower.ends_with(".htm"))
                    && !lower.starts_with("meta-inf/")
                {
                    if let Ok(x) = bsdtar_extract(path, m) {
                        text.push_str(&xml_to_text(&x));
                        text.push('\n');
                    }
                }
            }
            if text.trim().is_empty() {
                Err("no readable XHTML in epub".to_string())
            } else {
                Ok(text)
            }
        }
        _ => Err(format!("no text extractor for .{ext}")),
    }
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
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    // Round 7.1: formats with extractable text (docx/odt/epub/pdf) get
    // a real text preview instead of the metadata-only card.
    // Round 7.2: ods/odp join via the same ODF content.xml mechanism.
    if ["docx", "odt", "ods", "odp", "epub", "pdf"].contains(&ext.as_str()) {
        let doc_path = path.clone();
        let is_pdf = ext == "pdf";
        let extracted = tauri::async_runtime::spawn_blocking(move || {
            // Gauntlet G1: PDF text rides on pdftotext — ensure
            // Poppler at this user-invoked door first (session-
            // latched), so a missing-tools failure becomes a
            // setup, not a silent "no readable text".
            let receipt = if is_pdf {
                crate::deps::ensure_tool_session("pdfinfo", "poppler").err()
            } else {
                None
            };
            (extract_document_text(&doc_path), receipt)
        })
        .await;
        match extracted {
            Ok((Ok(text), _)) if !text.trim().is_empty() => {
                let mut lines: Vec<String> = Vec::new();
                let mut truncated = false;
                for raw in text.lines() {
                    if lines.len() >= max {
                        truncated = true;
                        break;
                    }
                    let l = raw.trim_end();
                    // XML spacing leaves blank runs — collapse them.
                    if l.trim().is_empty()
                        && (lines.is_empty()
                            || lines.last().map(|p: &String| p.trim().is_empty())
                                == Some(true))
                    {
                        continue;
                    }
                    let l = if l.chars().count() > 500 {
                        format!("{}...", l.chars().take(500).collect::<String>())
                    } else {
                        l.to_string()
                    };
                    lines.push(l);
                }
                return Ok(PreviewTextResult { lines, truncated });
            }
            Ok((_, receipt)) => {
                return Ok(PreviewTextResult {
                    lines: vec![receipt.unwrap_or_else(|| {
                        format!(
                            "No readable text could be extracted from this .{} file.",
                            ext
                        )
                    })],
                    truncated: false,
                });
            }
            _ => {
                return Ok(PreviewTextResult {
                    lines: vec![format!(
                        "No readable text could be extracted from this .{} file.",
                        ext
                    )],
                    truncated: false,
                });
            }
        }
    }
    // Binary office formats can't be previewed as text — return a friendly message
    let binary_office = ["xlsx", "xls", "doc", "pptx", "ppt", "odg"];
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

/// Split decoded document text into preview lines with the collapsing
/// rules `preview_text` applies (blank runs folded, lines clipped).
fn document_lines(text: &str, max: usize) -> (Vec<String>, bool) {
    let mut lines: Vec<String> = Vec::new();
    let mut truncated = false;
    for raw in text.lines() {
        if lines.len() >= max {
            truncated = true;
            break;
        }
        let l = raw.trim_end();
        if l.trim().is_empty()
            && (lines.is_empty()
                || lines.last().map(|p: &String| p.trim().is_empty()) == Some(true))
        {
            continue;
        }
        let l = if l.chars().count() > 500 {
            format!("{}...", l.chars().take(500).collect::<String>())
        } else {
            l.to_string()
        };
        lines.push(l);
    }
    (lines, truncated)
}

#[derive(Debug, serde::Serialize)]
pub struct DocumentPreview {
    /// "pdf" or "epub".
    pub format: String,
    pub title: Option<String>,
    pub author: Option<String>,
    /// "12 pages" / "24 chapters" — the document's extent, for the
    /// metadata rows.
    pub detail: Option<String>,
    /// Hero image data URL: the PDF's rendered first page or the
    /// EPUB's cover, when one could be produced.
    pub image: Option<String>,
    /// Opening text, line-split for the preview's text block.
    pub lines: Vec<String>,
    pub truncated: bool,
    /// Plain-language reason the preview is limited, when it is.
    pub note: Option<String>,
}

/// Rich document preview (Phase 4): a PDF's rendered first page or an
/// EPUB's cover, plus title/author/extent and the opening text —
/// decoded by ufm-preview (poppler-backed PDF, pure-Rust EPUB). The
/// hero image rides the freedesktop thumbnail cache (x-large tier,
/// md5(file-uri) naming, mtime freshness — the `get_thumbnail`
/// pattern), so repeat opens are instant and grid thumbnails share
/// the same render. Degradation is a note in the payload, never an
/// error: the Quick Look card always paints.
#[tauri::command]
pub async fn preview_document(
    state: State<'_, AppState>,
    item: ItemId,
) -> Cmd<DocumentPreview> {
    let path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if ext != "pdf" && ext != "epub" {
        return Err(CommandError::from(UfmError::InvalidInput(format!(
            "no document preview for .{ext}"
        ))));
    }
    tauri::async_runtime::spawn_blocking(move || build_document_preview(&path, &ext))
        .await
        .map_err(|e| CommandError::from(UfmError::Internal(format!("document task failed: {e}"))))?
        .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// When a door's tool setup didn't go through, the setup
/// receipt outranks the decoder's own "tools aren't installed"
/// note — the receipt says why, and what the user can do. Any
/// other decoder note (encrypted, unreadable) keeps its place.
/// (Gauntlet G1.)
pub(crate) fn prefer_setup_receipt(
    note: Option<String>,
    receipt: Option<String>,
) -> Option<String> {
    match (note, receipt) {
        (Some(n), Some(r)) if n.contains("aren't installed") => Some(r),
        (None, Some(r)) => Some(r),
        (n, _) => n,
    }
}

fn build_document_preview(
    path: &std::path::Path,
    ext: &str,
) -> std::result::Result<DocumentPreview, String> {
    use base64::Engine;
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    // The x-large hero cache: same digest + freshness law as
    // get_thumbnail, one tier up (freedesktop x-large = 512; the
    // document heroes render up to 1000 px wide).
    let home = std::env::var("HOME").map_err(|e| e.to_string())?;
    let cache_dir = std::path::PathBuf::from(format!("{home}/.cache/thumbnails/x-large"));
    let digest = format!("{:x}", md5::compute(file_uri(path).as_bytes()));
    let cache_file = cache_dir.join(format!("{digest}.png"));
    let src_mtime = meta.modified().ok();
    let cache_fresh = cache_file
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .zip(src_mtime)
        .map(|(cm, sm)| cm >= sm)
        .unwrap_or(false);
    let cached_image = if cache_fresh {
        std::fs::read(&cache_file).ok().map(|data| {
            format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(&data)
            )
        })
    } else {
        None
    };
    let cache_png = |png: &[u8]| {
        let _ = std::fs::create_dir_all(&cache_dir);
        let tmp = cache_dir.join(format!("{digest}.tmp.png"));
        if std::fs::write(&tmp, png).is_ok() {
            let _ = std::fs::rename(&tmp, &cache_file);
        }
    };
    let png_url = |png: &[u8]| {
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png)
        )
    };

    match ext {
        "pdf" => {
            // Gauntlet G1: Quick Look's document preview is a
            // user-invoked door, so Poppler is ENSURED here —
            // session-latched, so one declined prompt is
            // respected rather than re-asked on every preview —
            // and the decode below then re-runs with the tools
            // in place. The card still always paints: when setup
            // didn't go through, the receipt replaces the
            // decoder's missing-tools note, because the receipt
            // is the actionable truth. (Grid thumbnails stay
            // background and keep degrading quietly, by design.)
            let receipt =
                crate::deps::ensure_tool_session("pdfinfo", "poppler").err();
            // A fresh cached page makes the render unnecessary; the
            // metadata + text pass is the cheap remainder.
            let p = if cached_image.is_some() {
                ufm_preview::pdf::decode_pdf_metadata(path, meta.len())
            } else {
                ufm_preview::pdf::decode_pdf(path, meta.len(), 1000)
            };
            let image = cached_image.or_else(|| {
                p.page_png.as_ref().map(|png| {
                    cache_png(png);
                    png_url(png)
                })
            });
            let (lines, truncated) = document_lines(&p.text, 60);
            Ok(DocumentPreview {
                format: "pdf".into(),
                title: p.title,
                author: p.author,
                detail: p.page_count.map(|n| {
                    if n == 1 {
                        "1 page".to_string()
                    } else {
                        format!("{n} pages")
                    }
                }),
                image,
                lines,
                truncated,
                note: prefer_setup_receipt(p.note, receipt),
            })
        }
        _ => {
            // EPUB. Books are small; a runaway file gets a note, not a
            // multi-hundred-MB read.
            if meta.len() > 256 * 1024 * 1024 {
                return Ok(DocumentPreview {
                    format: "epub".into(),
                    title: None,
                    author: None,
                    detail: None,
                    image: None,
                    lines: vec![],
                    truncated: false,
                    note: Some(
                        "This book file is very large, so no preview was decoded — \
                         double-click opens it in your reader."
                            .into(),
                    ),
                });
            }
            let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
            let p = ufm_preview::epub::decode_epub(&bytes);
            let image = cached_image.or_else(|| {
                p.cover.as_ref().and_then(|cover| {
                    // Normalize the cover to PNG for the cache; a
                    // cover the image crate can't read still shows,
                    // served raw with its declared type (uncached).
                    match image::load_from_memory(&cover.data) {
                        Ok(img) => {
                            let mut buf = std::io::Cursor::new(Vec::new());
                            match img.write_to(&mut buf, image::ImageFormat::Png) {
                                Ok(()) => {
                                    let png = buf.into_inner();
                                    cache_png(&png);
                                    Some(png_url(&png))
                                }
                                Err(_) => None,
                            }
                        }
                        Err(_) => Some(format!(
                            "data:{};base64,{}",
                            cover.media_type,
                            base64::engine::general_purpose::STANDARD.encode(&cover.data)
                        )),
                    }
                })
            });
            let (lines, truncated) = document_lines(&p.excerpt, 60);
            Ok(DocumentPreview {
                format: "epub".into(),
                title: p.title,
                author: p.author,
                detail: if p.spine_count > 0 {
                    Some(if p.spine_count == 1 {
                        "1 chapter".to_string()
                    } else {
                        format!("{} chapters", p.spine_count)
                    })
                } else {
                    None
                },
                image,
                lines,
                truncated,
                note: p.note,
            })
        }
    }
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
        let mut items = 0usize;
        let mut files = 0usize;
        let mut errors = 0usize;
        // Delete files/
        if let Ok(entries) = std::fs::read_dir(&trash) {
            for entry in entries.flatten() {
                let p = entry.path();
                let n = count_tree_files(&p);
                let r = if p.is_dir() {
                    std::fs::remove_dir_all(&p)
                } else {
                    std::fs::remove_file(&p)
                };
                if r.is_ok() { items += 1; files += n; } else { errors += 1; }
            }
        }
        // Delete info/ (freedesktop trash metadata) — bookkeeping,
        // removed silently and never counted as user files.
        let info_dir = trash.replace("/files", "/info");
        if let Ok(entries) = std::fs::read_dir(&info_dir) {
            for entry in entries.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        Ok(TrashResult { deleted: items, items, files, errors })
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
        let mut errors = 0;

        // Collect all files (not dirs yet)
        let mut files = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&trash) {
            for entry in entries.flatten() {
                files.push(entry.path());
            }
        }

        let mut items = 0usize;
        let mut shredded = 0usize;
        for path in files {
            if path.is_dir() {
                // Recursively shred files in subdirs, then remove dir
                if let Ok(walker) = walkdir_files(&path) {
                    for f in walker {
                        shred_file(&f);
                        shredded += 1;
                    }
                }
                if std::fs::remove_dir_all(&path).is_ok() { items += 1; }
                else { errors += 1; }
            } else {
                shred_file(&path);
                if std::fs::remove_file(&path).is_ok() { items += 1; shredded += 1; }
                else { errors += 1; }
            }
        }
        // Clean info/ — bookkeeping, never counted (see TrashResult).
        let info_dir = trash.replace("/files", "/info");
        if let Ok(entries) = std::fs::read_dir(&info_dir) {
            for entry in entries.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        Ok(TrashResult { deleted: items, items, files: shredded, errors })
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
    /// Top-level trash entries removed (what the user calls "items").
    pub deleted: usize,
    /// Same as `deleted`, named for the UI.
    pub items: usize,
    /// Total real files removed, including files inside trashed
    /// folders. Trash bookkeeping (.trashinfo) is never counted —
    /// reporting it confused everyone (2 items read as "4 files").
    pub files: usize,
    pub errors: usize,
}

#[derive(serde::Serialize)]
pub struct TrashSummary {
    pub items: usize,
    pub files: usize,
}

/// Count real files under a path (1 for a file, recursive for a dir).
fn count_tree_files(p: &std::path::Path) -> usize {
    if p.is_dir() {
        walkdir_files(p).map(|v| v.len()).unwrap_or(0)
    } else {
        1
    }
}

/// Preview counts for the trash confirmation dialogs: how many items,
/// and how many files including folder contents.
#[tauri::command]
pub async fn trash_summary() -> Cmd<TrashSummary> {
    let trash = trash_path().await?;
    Ok(tauri::async_runtime::spawn_blocking(move || {
        let mut items = 0usize;
        let mut files = 0usize;
        if let Ok(entries) = std::fs::read_dir(&trash) {
            for entry in entries.flatten() {
                items += 1;
                files += count_tree_files(&entry.path());
            }
        }
        TrashSummary { items, files }
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("trash summary failed: {e}"))))?)
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
        let is_image = matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "ico");
        // Audio files thumbnail through their EMBEDDED cover art
        // (inc-2 item 9): Quick Look and the Music Player fall back
        // to this when the folder carries no art file, and until now
        // the gate below rejected every audio file outright, so the
        // fallback could never fire (Dickles' Technomancer m4b has a
        // covr atom and still showed the placeholder).
        let is_audio = matches!(
            ext.as_str(),
            "mp3" | "flac" | "ogg" | "oga" | "opus" | "m4a" | "m4b" | "aac" | "aif" | "aiff" | "wav" | "wma" | "dsf" | "dff"
        );
        // Documents (Phase 4): a PDF thumbnails through its rendered
        // first page, an EPUB through its cover — the ufm-preview
        // decoders — so grids and galleries show the book/page, not
        // a generic icon.
        let is_document = matches!(ext.as_str(), "pdf" | "epub");
        if !is_image && !is_audio && !is_document {
            return Err("not a thumbnail-able image".to_string());
        }
        let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
        // The size cap guards image decoding; an audiobook can be
        // hundreds of MB and still carry a small cover, and ffmpeg
        // streams it without loading the file.
        if is_image && meta.len() > 64 * 1024 * 1024 {
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
            let img = if is_image {
                image::open(&path).map_err(|e| format!("decode: {e}"))?
            } else if is_document && ext == "pdf" {
                let p = ufm_preview::pdf::decode_pdf_page(&path, meta.len(), max_px);
                let png = p
                    .page_png
                    .ok_or_else(|| "no page preview".to_string())?;
                image::load_from_memory(&png).map_err(|e| format!("decode page: {e}"))?
            } else if is_document {
                if meta.len() > 256 * 1024 * 1024 {
                    return Err("book too large to thumbnail".to_string());
                }
                let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
                let p = ufm_preview::epub::decode_epub(&bytes);
                let cover = p.cover.ok_or_else(|| "no cover image".to_string())?;
                image::load_from_memory(&cover.data)
                    .map_err(|e| format!("decode cover: {e}"))?
            } else {
                // Embedded cover art: ffmpeg exposes an MP3's APIC,
                // an MP4's covr and a FLAC/Vorbis picture as a video
                // stream — take one frame, piped back as PNG.
                let out = std::process::Command::new("ffmpeg")
                    .args(["-v", "error", "-i"])
                    .arg(&path)
                    .args([
                        "-map", "0:v:0", "-frames:v", "1", "-f", "image2pipe",
                        "-vcodec", "png", "-",
                    ])
                    .output()
                    .map_err(|e| format!("ffmpeg: {e}"))?;
                if !out.status.success() || out.stdout.is_empty() {
                    return Err("no embedded cover art".to_string());
                }
                image::load_from_memory(&out.stdout)
                    .map_err(|e| format!("decode art: {e}"))?
            };
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
    // Carry Phase 2 metadata (stars, collection memberships) to the
    // new path — the rename already succeeded, so this is best-effort.
    if let Ok(catalog) = ufm_catalog::Catalog::open(&state.catalog_path) {
        let _ = catalog.rekey_path(
            &path.to_string_lossy(),
            &dest.to_string_lossy(),
        );
    }
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
        // Gauntlet G1: `zip` is near-universal but not guaranteed
        // (minimal installs lack it) — Compress is the door, so
        // the tool is ensured at the door like every other.
        crate::deps::ensure_tool("zip", "zip")?;
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

/// Which clipboard world this session lives in (Gauntlet G1).
/// The session type the login session reports wins; the
/// display variables are the fallback — a session may lack the
/// type but a graphical one always has one of the displays.
/// Before G1 the clipboard only spoke wl-copy/wl-paste, so on
/// X11 sessions (Mint and MX default to them) file copy/paste
/// simply failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipboardBackend {
    Wayland,
    X11,
}

fn clipboard_backend() -> Result<ClipboardBackend, String> {
    match std::env::var("XDG_SESSION_TYPE").as_deref() {
        Ok("wayland") => return Ok(ClipboardBackend::Wayland),
        Ok("x11") => return Ok(ClipboardBackend::X11),
        _ => {}
    }
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        Ok(ClipboardBackend::Wayland)
    } else if std::env::var_os("DISPLAY").is_some() {
        Ok(ClipboardBackend::X11)
    } else {
        Err("Fable can't reach a clipboard here — this session doesn't report a Wayland or X11 display.".to_string())
    }
}

/// Write file paths to the session clipboard. Cut uses the
/// GNOME nautilus format so Nautilus honors the move; copy uses
/// text/uri-list, which everything reads. The helper is the
/// session's own (wl-copy on Wayland, xclip on X11 — the
/// nautilus format is native to both worlds) and is ensured at
/// this door, never assumed.
#[tauri::command]
pub async fn clipboard_write(paths: Vec<String>, cut: bool) -> Cmd<()> {
    tauri::async_runtime::spawn_blocking(move || {
        use std::io::{Read, Write};
        let backend = clipboard_backend()?;
        let tool = match backend {
            ClipboardBackend::Wayland => {
                crate::deps::ensure_tool("wl-copy", "wl-clipboard")?;
                "wl-copy"
            }
            ClipboardBackend::X11 => {
                crate::deps::ensure_tool("xclip", "xclip")?;
                "xclip"
            }
        };
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
        // AppImage hygiene (ROM-lists fix block): the bundle's
        // LD_LIBRARY_PATH must not leak into system tools — a bundled
        // libwayland/libffi can make wl-copy fail to start or to reach
        // the compositor, and the old code swallowed the exit status,
        // so the failure surfaced only as "[object Object]".
        let mut cmd = std::process::Command::new(tool);
        match backend {
            ClipboardBackend::Wayland => {
                cmd.arg("--type").arg(mime);
            }
            ClipboardBackend::X11 => {
                cmd.args(["-selection", "clipboard", "-t", mime, "-i"]);
            }
        }
        let mut child = cmd
            .env_remove("LD_LIBRARY_PATH")
            .stdin(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not start {tool}: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(data.as_bytes())
                .map_err(|e| format!("{tool} write: {e}"))?;
        }
        // Both helpers fork a clipboard owner and the parent exits
        // promptly; a nonzero exit is a REAL failure and must reach
        // the user.
        let status = child.wait().map_err(|e| format!("{tool} wait: {e}"))?;
        if !status.success() {
            let mut err_text = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                let _ = stderr.read_to_string(&mut err_text);
            }
            return Err(format!(
                "{tool} exited {status}: {}",
                err_text.trim()
            ));
        }
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

/// Read file paths from the session clipboard (wl-paste on
/// Wayland, xclip on X11 — ensured at the door, like the write).
#[tauri::command]
pub async fn clipboard_read() -> Cmd<ClipboardData> {
    tauri::async_runtime::spawn_blocking(move || {
        let backend = clipboard_backend()?;
        let tool = match backend {
            ClipboardBackend::Wayland => {
                crate::deps::ensure_tool("wl-paste", "wl-clipboard")?;
                "wl-paste"
            }
            ClipboardBackend::X11 => {
                crate::deps::ensure_tool("xclip", "xclip")?;
                "xclip"
            }
        };
        let read_type = |mime: &str| -> Option<String> {
            let mut cmd = std::process::Command::new(tool);
            match backend {
                ClipboardBackend::Wayland => {
                    cmd.arg("--type").arg(mime).arg("--no-newline");
                }
                ClipboardBackend::X11 => {
                    cmd.args(["-selection", "clipboard", "-t", mime, "-o"]);
                }
            }
            let out = cmd.env_remove("LD_LIBRARY_PATH").output().ok()?;
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

/// Pop the player out into its own window. UI round: this is now the
/// DETACH window — always-on-top mini player (Dickles: "same rule
/// about detaching" for video AND audio), sized like a mini, and it
/// receives the live playback state (`start` seconds + `autoplay`)
/// so playback continues from the exact position instead of
/// restarting. The window hands state back through `player-state` /
/// `player-return` events (see ui/src/main.ts + quicklook.ts).
#[tauri::command]
pub async fn open_player_window(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    path: String,
    name: String,
    kind: Option<String>,
    start: Option<f64>,
    autoplay: Option<bool>,
    art: Option<String>,
) -> Cmd<()> {
    use tauri::{Emitter, Manager};
    let label = player_label(&path);
    // Fix batch item 3: cover art rides the invoke channel (data
    // URLs are far too long for the window URL) and waits in the
    // stash for the mini window to pick up via `get_player_art`.
    if let Some(a) = art {
        state.player_art.lock().unwrap().insert(label.clone(), a);
    }
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.set_focus();
        return Ok(());
    }
    let audio = kind.as_deref() == Some("audio");
    let url = format!(
        "index.html?mode=player&path={}&name={}&kind={}&start={}&autoplay={}",
        query_encode(&path),
        query_encode(&name),
        if audio { "audio" } else { "video" },
        start.unwrap_or(0.0).max(0.0),
        if autoplay.unwrap_or(false) { "1" } else { "0" },
    );
    let (w, h, min_w, min_h) = if audio {
        (460.0, 208.0, 340.0, 170.0)
    } else {
        // Player round (Dickles' Brave screenshot): the detach
        // window used to open at a fixed 560x340 regardless of the
        // picture, so a 2.35:1 film left a slab of dead window
        // under the controls. The window now OPENS at the file's
        // true aspect (ffprobe dims): height is whatever the
        // picture needs at mini-player width — no more, no less.
        // Later resizes are the user's; the layout (styles.css
        // .vp-standalone) keeps any letterbox inside the picture.
        match probe_video_dims(std::path::Path::new(&path)) {
            Some((vw, vh)) => {
                let (fw, fh) = player_fit_size(vw, vh);
                (fw, fh, 320.0, 200.0)
            }
            None => (560.0, 340.0, 320.0, 200.0),
        }
    };
    let window = tauri::WebviewWindowBuilder::new(&app, &label, tauri::WebviewUrl::App(url.into()))
        .title(format!("{name} — Fable Files"))
        .inner_size(w, h)
        .min_inner_size(min_w, min_h)
        .always_on_top(true)
        .build()
        .map_err(|e| CommandError::from(UfmError::Internal(e.to_string())))?;
    let app2 = app.clone();
    let path2 = path.clone();
    let label2 = label.clone();
    window.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::Destroyed) {
            let st = app2.state::<AppState>();
            st.player_art.lock().unwrap().remove(&label2);
            let _ = app2.emit("player-closed", path2.clone());
        }
    });
    Ok(())
}

/// Whether a detach player window for `path` currently exists.
/// Fix batch item 4 (the tombstone guard): the "Playing in the
/// detached window" placeholder polls this and brings playback
/// home itself if the window vanished without its close event
/// landing — a placeholder must never outlive its window.
#[tauri::command]
pub fn player_window_open(app: tauri::AppHandle, path: String) -> bool {
    use tauri::Manager;
    app.get_webview_window(&player_label(&path)).is_some()
}

/// Artwork stashed for this path's detach window at open time
/// (fix batch item 3), or None when the caller had none.
#[tauri::command]
pub fn get_player_art(state: State<'_, AppState>, path: String) -> Option<String> {
    state
        .player_art
        .lock()
        .unwrap()
        .get(&player_label(&path))
        .cloned()
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
        crate::deps::ensure_tool("magick", "magick")?;
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
        crate::deps::ensure_tool("bsdtar", "bsdtar")?;
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
        crate::deps::ensure_tool("bsdtar", "bsdtar")?;
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
            crate::deps::ensure_tool("bsdtar", "bsdtar")?;
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


// ---------------------------------------------------------------------------
// Extension/duration rules (auto-labels + rule smart folders) and the
// loopback media URL. Durations are probed lazily with ffprobe and
// cached in the catalog; probe budgets keep every call short and the
// frontend loops while `remaining` > 0 (resume-able by construction).
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct LabelRule {
    pub label: String,
    pub extensions: Vec<String>,
    pub min_duration_secs: Option<f64>,
}

#[derive(serde::Serialize)]
pub struct RuleItemsResult {
    pub items: Vec<Item>,
    pub probed: u32,
    pub remaining: u32,
    /// Total members in the stored result (items may be truncated
    /// to the caller's limit).
    pub total_members: u32,
    /// Unprobed candidates on OFFLINE volumes — never probed; they
    /// join when the volume returns.
    pub pending_offline: u32,
    /// True when no online probing remains for this rule.
    pub sweep_complete: bool,
    /// Library Memory: volume badges for the returned items, keyed
    /// by (remapped) item id — smart folders badge offline files
    /// exactly like search results do.
    pub badges: Vec<ItemVolumeBadge>,
}

/// One item's volume badge (see SearchHit's volume fields).
#[derive(Debug, Clone, Serialize)]
pub struct ItemVolumeBadge {
    pub item_id: String,
    pub volume_label: String,
    pub offline: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<String>,
}

/// Badges for a set of items, resolved in one enumeration + one
/// volumes read (shared by the rule/smart-folder surfaces).
fn badges_for_items(catalog: &ufm_catalog::Catalog, items: &[Item]) -> Vec<ItemVolumeBadge> {
    let mounted = current_filesystems();
    let volumes = catalog.volumes().unwrap_or_default();
    let network_records = catalog.network_locations().unwrap_or_default();
    let live_net = crate::network::live_mounts();
    let mut out = Vec::new();
    for item in items {
        if let Some(pk) = item.provider_key.as_deref() {
            let badge = volume_badge_for_path(&mounted, &volumes, pk)
                .or_else(|| network_badge_for_path(&network_records, &live_net, pk));
            if let Some(b) = badge {
                out.push(ItemVolumeBadge {
                    item_id: item.id.to_string(),
                    volume_label: b.label,
                    offline: b.offline,
                    last_seen: b.last_seen,
                });
            }
        }
    }
    out
}

#[derive(serde::Serialize)]
pub struct RuleOutcome {
    pub label: String,
    pub matched: u32,
    pub added: u32,
    pub remaining: u32,
}

/// Probe one file's duration with ffprobe, in catalog ProbeOutcome
/// terms. The rule sweep guarantees the path sits on a mounted
/// volume; a path that vanished anyway reports Missing and is NEVER
/// settled as zero-length (the old probe recorded exactly that for
/// files on unmounted drives — ffprobe's exit status was ignored —
/// poisoning them out of duration rules forever), and ffprobe
/// failing to start reports Unavailable so the sweep stops honestly
/// instead of spinning.
fn probe_duration(path: &std::path::Path) -> ufm_catalog::ProbeOutcome {
    use ufm_catalog::ProbeOutcome;
    if !path.exists() {
        return ProbeOutcome::Missing;
    }
    let out = std::process::Command::new("ffprobe")
        .args([
            "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0",
        ])
        .arg(path)
        .output();
    match out {
        Ok(o) => {
            let text = String::from_utf8_lossy(&o.stdout);
            match text.trim().parse::<f64>().ok().filter(|d| *d > 0.0) {
                Some(d) => ProbeOutcome::Duration(d),
                None => ProbeOutcome::NoDuration,
            }
        }
        Err(e) => ProbeOutcome::Unavailable(format!("ffprobe could not start ({e})")),
    }
}

/// The probe-eligibility predicate for rule sweeps, built once per
/// command from Library Memory state: a path may be probed unless
/// the volume/server it lives on is currently OFFLINE — the same
/// resolution badges_for_items paints from. Offline items render
/// from catalog data and are never probed (no mount-waits, no
/// poisoned durations); they join when the volume returns.
fn rule_online_predicate(catalog: &ufm_catalog::Catalog) -> impl Fn(&str) -> bool + '_ {
    let mounted = current_filesystems();
    let volumes = catalog.volumes().unwrap_or_default();
    let network_records = catalog.network_locations().unwrap_or_default();
    let live_net = crate::network::live_mounts();
    move |pk: &str| {
        if let Some(b) = volume_badge_for_path(&mounted, &volumes, pk) {
            return !b.offline;
        }
        if let Some(b) = network_badge_for_path(&network_records, &live_net, pk) {
            return !b.offline;
        }
        true
    }
}

fn rule_spec(
    extensions: Vec<String>,
    min_duration_secs: Option<f64>,
) -> ufm_catalog::RuleSpec {
    ufm_catalog::RuleSpec {
        extensions,
        min_duration_secs,
    }
}

/// Re-anchor catalog items to session ids, exactly like
/// list_items_by_label does, so views can act on them.
fn remap_session_items(state: &AppState, items: Vec<Item>) -> Vec<Item> {
    let mut path_ids = state.path_ids.lock().unwrap();
    let mut item_paths = state.item_paths.lock().unwrap();
    let mut item_locations = state.item_locations.lock().unwrap();
    let mut out = Vec::new();
    for mut item in items {
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
    out
}

fn rule_result_from_status(
    state: &AppState,
    catalog: &ufm_catalog::Catalog,
    status: ufm_catalog::RuleStatus,
    limit: Option<u32>,
) -> RuleItemsResult {
    let total_members = status.member_count;
    let mut items = status.members;
    items.truncate(limit.unwrap_or(1000) as usize);
    let items = remap_session_items(state, items);
    let badges = badges_for_items(catalog, &items);
    RuleItemsResult {
        items,
        probed: 0,
        remaining: status.pending,
        total_members,
        pending_offline: status.pending_offline,
        sweep_complete: status.sweep_complete,
        badges,
    }
}

#[tauri::command]
pub async fn rule_items(
    state: State<'_, AppState>,
    extensions: Vec<String>,
    min_duration_secs: Option<f64>,
    limit: Option<u32>,
    probe_budget: Option<u32>,
) -> Cmd<RuleItemsResult> {
    // probe_budget is legacy and ignored: rule_items NEVER probes.
    // The remembered result (the catalog rule store) paints
    // instantly; the duration sweep runs in the background via
    // rule_probe, behind the visible list. A first-ever evaluation
    // runs one delta pass — stored durations only — to seed it.
    let _ = probe_budget;
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    let spec = rule_spec(extensions, min_duration_secs);
    let status = match catalog.rule_snapshot(&spec).map_err(CommandError::from)? {
        Some(s) => s,
        None => {
            let online = rule_online_predicate(&catalog);
            let now = chrono::Utc::now().to_rfc3339();
            catalog
                .rule_refresh(&spec, &online, &now)
                .map_err(CommandError::from)?
        }
    };
    Ok(rule_result_from_status(state.inner(), &catalog, status, limit))
}

/// Reconcile a rule with the catalog: files added, changed, or
/// removed since the remembered evaluation apply as deltas. The UI
/// calls this behind the instant snapshot paint on every revisit,
/// so the list never waits for it.
#[tauri::command]
pub async fn rule_refresh(
    state: State<'_, AppState>,
    extensions: Vec<String>,
    min_duration_secs: Option<f64>,
    limit: Option<u32>,
) -> Cmd<RuleItemsResult> {
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    let spec = rule_spec(extensions, min_duration_secs);
    let online = rule_online_predicate(&catalog);
    let now = chrono::Utc::now().to_rfc3339();
    let status = catalog
        .rule_refresh(&spec, &online, &now)
        .map_err(CommandError::from)?;
    Ok(rule_result_from_status(state.inner(), &catalog, status, limit))
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleProbeResult {
    /// Files probed in this batch.
    pub probed: u32,
    /// Unprobed candidates left on mounted volumes.
    pub remaining: u32,
    /// Unprobed candidates on offline volumes (they join when the
    /// volume returns; never probed while offline).
    pub remaining_offline: u32,
    /// Files this batch newly proved to be members — the UI merges
    /// them into the visible list live.
    pub new_members: Vec<Item>,
    /// Library Memory badges for `new_members`.
    pub badges: Vec<ItemVolumeBadge>,
    /// The batch stopped early: the user left the view. Everything
    /// probed so far is persisted; the sweep resumes next visit.
    pub cancelled: bool,
    /// Probing itself is unavailable (e.g. ffprobe missing): the
    /// sweep stopped without settling anything.
    pub probe_error: Option<String>,
}

/// One batch of the background duration sweep (default 250 files).
/// The ffprobe work runs on the blocking pool; every probe persists
/// immediately in the catalog, so leaving mid-sweep loses nothing.
#[tauri::command]
pub async fn rule_probe(
    state: State<'_, AppState>,
    extensions: Vec<String>,
    min_duration_secs: Option<f64>,
    budget: Option<u32>,
) -> Cmd<RuleProbeResult> {
    let spec = rule_spec(extensions, min_duration_secs);
    // A fresh batch is a new intent: clear a stale cancel left by a
    // previous view. Leaving mid-batch sets it again.
    let cancel = state.rule_probe_cancel.clone();
    cancel.store(false, std::sync::atomic::Ordering::SeqCst);
    // The catalog opens INSIDE the blocking task: the online
    // predicate borrows it, and spawn_blocking needs 'static.
    let catalog_path = state.catalog_path.clone();
    let batch = tauri::async_runtime::spawn_blocking(
        move || -> Cmd<ufm_catalog::RuleProbeBatch> {
            let catalog = ufm_catalog::Catalog::open(&catalog_path)
                .map_err(CommandError::from)?;
            let online = rule_online_predicate(&catalog);
            let mut probe = |p: &std::path::Path| probe_duration(p);
            catalog
                .rule_probe_batch(
                    &spec,
                    budget.unwrap_or(250),
                    &mut probe,
                    &online,
                    &cancel,
                )
                .map_err(CommandError::from)
        },
    )
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("rule probe task: {e}"))))??;
    let new_members = remap_session_items(state.inner(), batch.new_members);
    let badges_catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    let badges = badges_for_items(&badges_catalog, &new_members);
    Ok(RuleProbeResult {
        probed: batch.probed,
        remaining: batch.remaining,
        remaining_offline: batch.remaining_offline,
        new_members,
        badges,
        cancelled: batch.cancelled,
        probe_error: batch.probe_error,
    })
}

/// Cancel the running duration sweep (the user left the smart
/// folder). The batch checks the flag between files; partial
/// progress is already persisted in the catalog.
#[tauri::command]
pub fn rule_cancel_probe(state: State<'_, AppState>) {
    state
        .rule_probe_cancel
        .store(true, std::sync::atomic::Ordering::SeqCst);
}

#[tauri::command]
pub async fn apply_label_rules(
    state: State<'_, AppState>,
    rules: Vec<LabelRule>,
) -> Cmd<Vec<RuleOutcome>> {
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    let online = rule_online_predicate(&catalog);
    let now = chrono::Utc::now().to_rfc3339();
    let never_cancel = std::sync::atomic::AtomicBool::new(false);
    let mut outcomes = Vec::new();
    for rule in &rules {
        let spec = rule_spec(rule.extensions.clone(), rule.min_duration_secs);
        // Evaluate from stored durations, then spend the historical
        // per-rule probe budget (250) — the same reach as the old
        // inline eval, except every probe now persists, so the
        // budget buys permanent progress instead of a re-scan.
        let mut status = catalog
            .rule_refresh(&spec, &online, &now)
            .map_err(CommandError::from)?;
        let mut spent = 0u32;
        while status.pending > 0 && spent < 250 {
            let batch = catalog
                .rule_probe_batch(
                    &spec,
                    250 - spent,
                    &mut |p: &std::path::Path| probe_duration(p),
                    &online,
                    &never_cancel,
                )
                .map_err(CommandError::from)?;
            spent += batch.probed;
            if batch.probe_error.is_some() || batch.probed == 0 {
                break;
            }
            status = catalog
                .rule_snapshot(&spec)
                .map_err(CommandError::from)?
                .unwrap_or(status);
        }
        let mut added = 0u32;
        for item in &status.members {
            if let Some(pk) = &item.provider_key {
                if catalog
                    .tag_path_counted(pk, &rule.label)
                    .map_err(CommandError::from)?
                {
                    added += 1;
                }
            }
        }
        outcomes.push(RuleOutcome {
            label: rule.label.clone(),
            matched: status.member_count,
            added,
            remaining: status.pending,
        });
    }
    Ok(outcomes)
}

/// ---------------------------------------------------------------------------
/// ROM identification (ROM Lists): built-in ROM collection membership is
/// a DAT identification, never an extension guess. The dataset ships as
/// the rom-dats.bin bundle resource (see ROM-DATS.md); results cache in
/// the catalog's media_identities keyed by item id, validated against
/// (size, modified) so unchanged files are never rehashed.
/// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RomIdentifyStats {
    pub already_running: bool,
    pub candidates: u32,
    pub hashed: u32,
    pub matched: u32,
    pub fresh: u32,
    pub errors: u32,
    pub elapsed_ms: u64,
    pub files_per_sec: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RomIdentifyProgress {
    pub done: u32,
    pub total: u32,
    pub matched: u32,
}

fn rom_dataset(
    app: &tauri::AppHandle,
    state: &AppState,
) -> Result<std::sync::Arc<ufm_media::romdat::RomDataset>, CommandError> {
    if let Some(ds) = state.rom_dataset.lock().unwrap().clone() {
        return Ok(ds);
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(res) = app.path().resource_dir() {
        candidates.push(res.join("rom-dats.bin"));
        candidates.push(res.join("resources/rom-dats.bin"));
    }
    candidates.push(PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/resources/rom-dats.bin"
    )));
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("rom-dats.bin"));
            candidates.push(dir.join("resources/rom-dats.bin"));
        }
    }
    for c in &candidates {
        if let Ok(bytes) = std::fs::read(c) {
            let ds = ufm_media::romdat::RomDataset::load(bytes).map_err(|e| {
                CommandError::from(UfmError::InvalidInput(format!("rom dataset: {e}")))
            })?;
            let arc = std::sync::Arc::new(ds);
            *state.rom_dataset.lock().unwrap() = Some(arc.clone());
            return Ok(arc);
        }
    }
    Err(CommandError::from(UfmError::NotFound(
        "rom dataset (rom-dats.bin) not found in app resources".into(),
    )))
}

/// Which DAT project a system key's rows came from (evidence label).
/// The CD systems Phase 2 learned to identify (Neo Geo CD, Amiga
/// CD32, PC-FX) are Redump; "jaguar" merges No-Intro cart rows with
/// Redump's Jaguar CD rows under one key — the label names Redump,
/// the project this build's jaguar identifications come from.
fn rom_source_label(sys: &str) -> &'static str {
    match sys {
        "arcade" | "neogeo" => "mame",
        "3do" | "amigacd32" | "dreamcast" | "gamecube" | "jaguar" | "neogeocd" | "pcfx"
        | "psx" | "ps2" | "ps3" | "saturn" | "segacd" | "wii" | "xbox" | "xbox360" => {
            "redump"
        }
        _ => "no-intro",
    }
}

#[tauri::command]
pub async fn identify_roms(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Cmd<RomIdentifyStats> {
    use std::sync::atomic::Ordering;
    if state
        .rom_identify_running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Ok(RomIdentifyStats {
            already_running: true,
            candidates: 0,
            hashed: 0,
            matched: 0,
            fresh: 0,
            errors: 0,
            elapsed_ms: 0,
            files_per_sec: 0.0,
        });
    }
    let app2 = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let st = app2.state::<AppState>();
        run_rom_identify(&app2, &st)
    })
    .await;
    state
        .rom_identify_running
        .store(false, Ordering::SeqCst);
    match result {
        Ok(r) => {
            // A fresh identify changes catalog membership — the
            // unified view (alias farm + playlists) reconciles.
            if r.is_ok() {
                crate::emu_farm::spawn_keeper(app.clone());
                // v2.1: new systems may have joined the library
                // — the fleet prep pass tops itself up.
                crate::emu_prep::spawn_prep(app.clone());
            }
            r
        }
        Err(e) => Err(CommandError::from(UfmError::InvalidInput(format!(
            "identify task failed: {e}"
        )))),
    }
}

fn run_rom_identify(
    app: &tauri::AppHandle,
    state: &AppState,
) -> Cmd<RomIdentifyStats> {
    let started = std::time::Instant::now();
    // Archive identification shells to 7z (bsdtar fallback). Ensure
    // both once per pass, best effort: a declined/absent tool only
    // degrades archive reading (romdat falls back, then yields no
    // match) — it must never block identifying everything else.
    let _ = crate::deps::ensure_tool("7z", "7z");
    let _ = crate::deps::ensure_tool("bsdtar", "bsdtar");
    let ds = rom_dataset(app, state)?;
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    let exts: Vec<String> = ufm_media::romdat::candidate_extensions()
        .iter()
        .map(|s| s.to_string())
        .collect();
    let candidates = catalog
        .rule_candidates(&exts)
        .map_err(CommandError::from)?;
    let total = candidates.len() as u32;
    let mut stats = RomIdentifyStats {
        already_running: false,
        candidates: total,
        hashed: 0,
        matched: 0,
        fresh: 0,
        errors: 0,
        elapsed_ms: 0,
        files_per_sec: 0.0,
    };
    for (i, (item, _dur)) in candidates.iter().enumerate() {
        if i % 100 == 0 {
            let _ = app.emit(
                "rom-identify-progress",
                RomIdentifyProgress {
                    done: i as u32,
                    total,
                    matched: stats.matched,
                },
            );
        }
        let Some(pk) = item.provider_key.clone() else {
            continue;
        };
        let size = item.size_bytes.unwrap_or(0);
        let modified = item.modified.map(|m| m.to_rfc3339());
        // Freshness: a stored identity whose evidence matches the
        // file's current (size, modified) is still true — skip.
        if let Ok(Some(row)) = catalog.media_identity_for(&item.id) {
            if let Ok(ev) =
                serde_json::from_str::<serde_json::Value>(&row.evidence_json)
            {
                let ev_size = ev.get("size").and_then(|v| v.as_u64());
                let ev_mod = ev
                    .get("modified")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                if ev_size == Some(size) && ev_mod == modified {
                    stats.fresh += 1;
                    continue;
                }
            }
        }
        stats.hashed += 1;
        let path = std::path::PathBuf::from(&pk);
        // identify_file_all: archives can hold several games (one
        // per disc-set variant packed inside — TruRip's regional
        // editions). The row carries the BEST variant as the item's
        // identity; every variant is preserved in the evidence for
        // the Phase 3 grouping layer.
        let identified = ufm_media::romdat::identify_file_all(&ds, &path);
        let (title, platform, sha1, evidence) = match identified {
            Ok(ids) if !ids.is_empty() => {
                stats.matched += 1;
                let id = &ids[0];
                let variants: Vec<serde_json::Value> = ids
                    .iter()
                    .map(|v| {
                        serde_json::json!({
                            "sys": v.sys,
                            "also": v.also,
                            "datName": v.dat_name,
                            "crc": v.crc,
                            "md5": v.md5_hex(),
                            "sha1": v.sha1_hex(),
                            "container": v.container,
                            "member": v.member,
                        })
                    })
                    .collect();
                let ev = serde_json::json!({
                    "matched": true,
                    "crc": id.crc,
                    "md5": id.md5_hex(),
                    "size": size,
                    "modified": modified,
                    "source": rom_source_label(&id.sys),
                    "container": id.container,
                    "also": id.also,
                    "member": id.member,
                    "variants": variants,
                });
                (
                    Some(id.dat_name.clone()),
                    Some(id.sys.clone()),
                    Some(id.sha1_hex()),
                    ev,
                )
            }
            Ok(_) => {
                let ev = serde_json::json!({
                    "matched": false,
                    "size": size,
                    "modified": modified,
                    "also": [],
                });
                (None, None, None, ev)
            }
            Err(_) => {
                stats.errors += 1;
                continue;
            }
        };
        // Write with a small retry: the indexer may hold the WAL
        // writer briefly; a vanished item (FK) just counts as an error.
        let mut attempt = 0;
        loop {
            attempt += 1;
            match catalog.upsert_media_identity(
                &item.id,
                "rom",
                title.as_deref(),
                platform.as_deref(),
                sha1.as_deref(),
                &evidence.to_string(),
            ) {
                Ok(()) => break,
                Err(e) => {
                    if attempt < 3 && e.to_string().to_lowercase().contains("locked") {
                        std::thread::sleep(std::time::Duration::from_millis(120));
                        continue;
                    }
                    stats.errors += 1;
                    break;
                }
            }
        }
    }
    let _ = app.emit(
        "rom-identify-progress",
        RomIdentifyProgress {
            done: total,
            total,
            matched: stats.matched,
        },
    );
    stats.elapsed_ms = started.elapsed().as_millis() as u64;
    if stats.elapsed_ms > 0 && stats.hashed > 0 {
        stats.files_per_sec =
            stats.hashed as f64 / (stats.elapsed_ms as f64 / 1000.0);
    }
    Ok(stats)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RomScope {
    /// "all" | "boot" | "drives"
    pub mode: String,
    #[serde(default)]
    pub drives: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RomHit {
    pub item: Item,
    pub dat_name: String,
    pub system: String,
}

/// The members of one built-in ROM collection: catalog items with a
/// DAT identification for `system`, honoring the collection's scope.
/// Display names come from the DAT, not the filename.
#[tauri::command]
pub async fn rom_collection_items(
    state: State<'_, AppState>,
    system: String,
    scope: RomScope,
) -> Cmd<Vec<RomHit>> {
    let location_ids: Option<Vec<String>> = match scope.mode.as_str() {
        "boot" => Some(
            state
                .locations
                .lock()
                .unwrap()
                .iter()
                .filter(|l| l.provider != "volume")
                .map(|l| l.id.to_string())
                .collect(),
        ),
        "drives" => Some(scope.drives.clone()),
        _ => None,
    };
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    let rows = catalog
        .rom_collection_items(&system, location_ids.as_deref())
        .map_err(CommandError::from)?;
    let mut out = Vec::with_capacity(rows.len());
    for (item, title) in rows {
        let remapped = remap_session_items(state.inner(), vec![item]);
        if let Some(it) = remapped.into_iter().next() {
            out.push(RomHit {
                item: it,
                dat_name: title,
                system: system.clone(),
            });
        }
    }
    Ok(out)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RomIdentityHit {
    pub item_id: String,
    pub platform: String,
}

/// Identified platforms for a batch of items — the Files banner and
/// the "Open in ROM Home" door consult IDENTITIES first (Phase 2):
/// a folder may only be claimed for a system its identified games
/// actually belong to. Unidentified items are simply absent.
#[tauri::command]
pub async fn rom_identity_map(
    state: State<'_, AppState>,
    item_ids: Vec<String>,
) -> Cmd<Vec<RomIdentityHit>> {
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    let rows = catalog
        .rom_platforms_for_items(&item_ids)
        .map_err(CommandError::from)?;
    Ok(rows
        .into_iter()
        .map(|(item_id, platform)| RomIdentityHit { item_id, platform })
        .collect())
}

/// Is this path in ROM territory? Either its folder already holds an
/// identified game (data-driven), or some path segment names a ROM
/// collection outright. Keeps the Needs-ID shelf honest: PC-game
/// data files and documentation trees that merely share an
/// extension with a ROM format never qualify on their own.
fn rom_context_path(
    provider_key: &str,
    identified_folders: &std::collections::HashSet<String>,
) -> bool {
    if let Some((parent, _)) = provider_key.rsplit_once('/') {
        if identified_folders.contains(parent) {
            return true;
        }
    }
    provider_key.split('/').any(|seg| {
        matches!(
            seg.to_ascii_lowercase().as_str(),
            "roms" | "rom" | "games" | "game"
        )
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RomNeedsIdEntry {
    pub item: Item,
    /// Shelf group: the filename with its tag groups stripped
    /// (romdat::parsed_base_title) — Dickles Q3: grouped, not flat.
    pub group: String,
    /// Why this file is dark, in plain words.
    pub reason: String,
    /// True when an identification pass has already scanned (and
    /// failed to match) this file; false when it was never scanned.
    pub scanned: bool,
}

/// The ROM Home "Needs identification" shelf: ROM candidates in ROM
/// territory that no DAT claims — grouped by parsed filename in the
/// UI. Scoped hard (see rom_context_path): the raw matched-nothing
/// bucket is ~75% non-ROM noise and is NOT what this returns.
#[tauri::command]
pub async fn rom_home_needs_id(
    state: State<'_, AppState>,
) -> Cmd<Vec<RomNeedsIdEntry>> {
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    let exts: Vec<String> = ufm_media::romdat::candidate_extensions()
        .iter()
        .map(|s| s.to_string())
        .collect();
    let rows = catalog
        .rom_needs_id_candidates(&exts)
        .map_err(CommandError::from)?;
    let identified_folders = catalog
        .rom_identified_folders()
        .map_err(CommandError::from)?;
    let mut out = Vec::new();
    for (item, has_row) in rows {
        let Some(pk) = item.provider_key.clone() else {
            continue;
        };
        if !rom_context_path(&pk, &identified_folders) {
            continue;
        }
        let name = item.name.clone();
        let remapped = remap_session_items(state.inner(), vec![item]);
        let Some(it) = remapped.into_iter().next() else {
            continue;
        };
        let stem = std::path::Path::new(&name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&name)
            .to_string();
        let group = {
            let g = ufm_media::romdat::parsed_base_title(&stem);
            if g.is_empty() {
                stem.clone()
            } else {
                g
            }
        };
        let ext = std::path::Path::new(&name)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let reason = if !has_row {
            "Not scanned yet"
        } else if matches!(ext.as_str(), "7z" | "rar" | "zip") {
            "Scanned — no known game inside"
        } else if matches!(ext.as_str(), "cue" | "ccd" | "chd" | "gdi" | "mdf" | "cdi") {
            "Scanned — disc set not matched"
        } else {
            "Scanned — no DAT match"
        }
        .to_string();
        out.push(RomNeedsIdEntry {
            item: it,
            group,
            reason,
            scanned: has_row,
        });
    }
    Ok(out)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RomResetStats {
    pub identities_removed: u64,
    pub media_removed: u64,
}

/// Reset the ROM collection (Dickles, 2026-10-16): wipe every ROM
/// identity and cached ROM-media row so the next identification
/// pass starts clean — the on-ramp for a big new games drive.
/// Personal state (ratings, favorites, play history) is keyed by
/// file path in rom_game_state and is NOT touched; the UI's
/// confirmation copy says exactly this before the user commits.
#[tauri::command]
pub async fn rom_reset_collection(
    state: State<'_, AppState>,
) -> Cmd<RomResetStats> {
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    let (identities_removed, media_removed) = catalog
        .reset_rom_collection()
        .map_err(CommandError::from)?;
    Ok(RomResetStats {
        identities_removed,
        media_removed,
    })
}

// ---------------------------------------------------------------------------
// ROM Home (Dickles, 2026-10-15): the room's data doors. Shelf
// queries read the catalog alone (never the disk), so an unplugged
// drive's collection paints instantly with Library Memory badges;
// personal state writes are path-keyed in the catalog.
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RomHomeGameWire {
    pub item: Item,
    /// DAT-verified title (the display title is the state's
    /// title_override when set, else this).
    pub title: String,
    pub platform: String,
    pub also_platforms: Vec<String>,
    pub year: Option<i64>,
    pub sha1: Option<String>,
    pub region: Option<String>,
    pub media: Vec<ufm_catalog::RomGameMedia>,
    pub state: ufm_catalog::RomGameState,
    pub auditor_flagged: bool,
    /// Library Memory: the volume this game lives on, when known.
    pub volume_label: Option<String>,
    pub volume_offline: bool,
    /// RFC 3339 — when that volume was last seen mounted.
    pub volume_last_seen: Option<String>,
}

#[tauri::command]
pub async fn rom_home_games(
    state: State<'_, AppState>,
    platform: Option<String>,
) -> Cmd<Vec<RomHomeGameWire>> {
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    let rows = catalog
        .rom_home_games(platform.as_deref())
        .map_err(CommandError::from)?;
    let items: Vec<Item> = rows.iter().map(|r| r.item.clone()).collect();
    let remapped = remap_session_items(state.inner(), items);
    let badges = badges_for_items(&catalog, &remapped);
    let badge_by_id: std::collections::HashMap<String, ItemVolumeBadge> =
        badges.into_iter().map(|b| (b.item_id.clone(), b)).collect();
    let mut out = Vec::with_capacity(rows.len());
    for (row, item) in rows.into_iter().zip(remapped.into_iter()) {
        let badge = badge_by_id.get(&item.id.to_string());
        out.push(RomHomeGameWire {
            item,
            title: row.title,
            platform: row.platform,
            also_platforms: row.also_platforms,
            year: row.year,
            sha1: row.sha1,
            region: row.region,
            media: row.media,
            state: row.state,
            auditor_flagged: row.auditor_flagged,
            volume_label: badge.map(|b| b.volume_label.clone()),
            volume_offline: badge.map(|b| b.offline).unwrap_or(false),
            volume_last_seen: badge.and_then(|b| b.last_seen.clone()),
        });
    }
    Ok(out)
}

/// The sidebar door's numbers (total games · games missing media),
/// plus the per-system split the rail renders counts from. Catalog
/// only — this never touches the disk.
#[tauri::command]
pub fn rom_home_counts(
    state: State<'_, AppState>,
) -> Cmd<ufm_catalog::RomHomeCounts> {
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    catalog.rom_home_counts().map_err(CommandError::from)
}

#[tauri::command]
pub fn rom_home_set_rating(
    state: State<'_, AppState>,
    provider_key: String,
    rating: Option<i64>,
) -> Cmd<()> {
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    catalog
        .rom_game_set_rating(&provider_key, rating)
        .map_err(CommandError::from)
}

#[tauri::command]
pub fn rom_home_set_favorite(
    state: State<'_, AppState>,
    provider_key: String,
    on: bool,
) -> Cmd<()> {
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    catalog
        .rom_game_set_favorite(&provider_key, on)
        .map_err(CommandError::from)
}

#[tauri::command]
pub fn rom_home_set_completed(
    state: State<'_, AppState>,
    provider_key: String,
    on: bool,
) -> Cmd<()> {
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    catalog
        .rom_game_set_completed(&provider_key, on)
        .map_err(CommandError::from)
}

#[tauri::command]
pub fn rom_home_set_overrides(
    state: State<'_, AppState>,
    provider_key: String,
    title_override: Option<String>,
    genre: Option<String>,
    players: Option<String>,
    notes: Option<String>,
) -> Cmd<()> {
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    catalog
        .rom_game_set_overrides(
            &provider_key,
            title_override.as_deref(),
            genre.as_deref(),
            players.as_deref(),
            notes.as_deref(),
        )
        .map_err(CommandError::from)
}

/// One game's personal state by path — the read side for
/// extension-rule games, whose shelf rows don't come from the
/// identified-ROMs query.
#[tauri::command]
pub fn rom_home_state(
    state: State<'_, AppState>,
    provider_key: String,
) -> Cmd<ufm_catalog::RomGameState> {
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    catalog
        .rom_game_state_for(&provider_key)
        .map_err(CommandError::from)
}

/// Play was pressed in the room: the launch itself rides the
/// existing open path (the shell's openItem); this records the
/// relationship — count + last played — and returns the new state.
#[tauri::command]
pub fn rom_home_note_play(
    state: State<'_, AppState>,
    provider_key: String,
) -> Cmd<ufm_catalog::RomGameState> {
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    catalog
        .rom_game_note_play(&provider_key)
        .map_err(CommandError::from)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RomSmartList {
    pub id: String,
    pub name: String,
    /// The stored filter model (the room's own filter JSON).
    pub query_json: String,
}

/// Save an edited rule back onto the same Smart List.
#[tauri::command]
pub fn rom_home_update_smart_list(
    state: State<'_, AppState>,
    id: String,
    query_json: String,
) -> Cmd<()> {
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    catalog
        .update_smart_collection(&id, &query_json)
        .map_err(CommandError::from)
}

/// Every Smart List (a saved filter with its readable rule).
#[tauri::command]
pub fn rom_home_smart_lists(state: State<'_, AppState>) -> Cmd<Vec<RomSmartList>> {
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    let rows = catalog
        .smart_collections()
        .map_err(CommandError::from)?;
    Ok(rows
        .into_iter()
        .map(|(id, name, query_json)| RomSmartList {
            id,
            name,
            query_json,
        })
        .collect())
}

/// Keep the room's current filter as a Smart List. Returns the id.
#[tauri::command]
pub fn rom_home_save_smart_list(
    state: State<'_, AppState>,
    name: String,
    query_json: String,
) -> Cmd<String> {
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    let id = CollectionId::new();
    catalog
        .create_smart_collection(&id, &name, &query_json)
        .map_err(CommandError::from)?;
    Ok(id.to_string())
}

/// Register a local file with the built-in media server and return a
/// loopback URL. WebKit's media stack handles plain HTTP + Range
/// natively; the Tauri asset protocol is where video goes to die
/// (proven 2026-10-13: file:// and gst play everything, the asset
/// protocol plays nothing), so previews stream from here instead.
#[tauri::command]
pub async fn media_url_for_path(
    state: State<'_, AppState>,
    path: String,
) -> Cmd<String> {
    let port = media_server_port(state.inner())
        .map_err(|e| CommandError::from(UfmError::Internal(e)))?;
    let token = format!("{}", ItemId::new());
    state
        .media_files
        .lock()
        .unwrap()
        .insert(token.clone(), std::path::PathBuf::from(&path));
    Ok(format!("http://127.0.0.1:{port}/cast/{token}"))
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
        path: Some(path),
        removable: false,
        uri: None,
        online: true,
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
        if let Err(why) = crate::deps::ensure_tool("ffmpeg", "ffmpeg") {
            emit(0.0, true, None, Some(why));
            return;
        }
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
        crate::deps::ensure_tool("exiftool", "exiftool")?;
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
        crate::deps::ensure_tool("exiftool", "exiftool")?;
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
        crate::deps::ensure_tool("exiftool", "exiftool")?;
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
        crate::deps::ensure_tool("xorriso", "xorriso")?;
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
        crate::deps::ensure_tool("bsdtar", "bsdtar")?;
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
        crate::deps::ensure_tool("growisofs", "growisofs")?;
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
    // Dependency law: every rclone door funnels through here, so
    // a missing rclone is set up once, in context, and the call
    // the user asked for then runs.
    crate::deps::ensure_tool("rclone", "rclone")?;
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

/// Serve one media-server connection (extracted from
/// media_server_port so the Range / Content-Type behavior is
/// unit-testable against a real socket — Player round).
fn serve_media_conn(
    mut stream: std::net::TcpStream,
    files: std::sync::Arc<
        std::sync::Mutex<std::collections::HashMap<String, std::path::PathBuf>>,
    >,
) {
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
    let is_transcode = path_part.starts_with("/transcode/");
    let token_raw = if is_transcode {
        path_part.trim_start_matches("/transcode/")
    } else {
        path_part.trim_start_matches("/cast/")
    };
    let token = token_raw.split(['?', '&']).next().unwrap_or("");
    let file_path = files.lock().unwrap().get(token).cloned();
    let Some(file_path) = file_path else {
        let _ = stream.write_all(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n",
        );
        return;
    };
    if is_transcode {
        // Round 7.1: live H.264 transcode stream for formats
        // WebKitGTK can't render (HEVC 10-bit). ?t=<secs>
        // is the input-seek start offset.
        let start = path_part
            .split('?')
            .nth(1)
            .and_then(|q| {
                q.split('&').find_map(|kv| {
                    kv.strip_prefix("t=")
                        .and_then(|v| v.parse::<u64>().ok())
                })
            })
            .unwrap_or(0);
        if is_head {
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nContent-Length: 0\r\n\r\n",
            );
        } else {
            serve_transcode(stream, &file_path, start);
        }
        return;
    }
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
    // Real media type, not octet-stream: Chromecast and
    // DLNA renderers refuse (or mis-seek) files served as
    // application/octet-stream. Local playback didn't
    // care; receivers do. (Player round.)
    let mut headers = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {}\r\nContent-Length: {len}\r\nAccept-Ranges: bytes\r\n",
        guess_mime(&file_path)
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
            let Ok(stream) = stream else { continue };
            let files = files.clone();
            std::thread::spawn(move || serve_media_conn(stream, files));
        }
    });
    Ok(port)
}

/// Whether VA-API hardware transcode works on this machine (probed
/// once with a synthetic clip, then cached for the process lifetime).
fn vaapi_works() -> bool {
    static CACHE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        if !std::path::Path::new("/dev/dri/renderD128").exists() {
            return false;
        }
        std::process::Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-vaapi_device",
                "/dev/dri/renderD128",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=0.1:size=128x72:rate=1",
                "-vf",
                "format=nv12,hwupload",
                "-c:v",
                "h264_vaapi",
                "-f",
                "null",
                "-",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// Video dimensions via ffprobe (first stream that reports any).
fn probe_video_dims(path: &std::path::Path) -> Option<(u32, u32)> {
    let out = run_with_timeout(
        "ffprobe",
        &[
            "-v",
            "error",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=p=0",
            &path.to_string_lossy(),
        ],
        15,
    )
    .ok()?;
    for line in out.lines() {
        let mut it = line.trim().split(',');
        let w: u32 = it.next()?.trim().parse().ok()?;
        let h: u32 = it.next()?.trim().parse().ok()?;
        if w > 0 && h > 0 {
            return Some((w, h));
        }
    }
    None
}

/// Detach-player window size for a video of `vw`x`vh` (Player
/// round): width starts at the mini-player's 640, height is exactly
/// what the picture needs at that width. Absurd aspects clamp to a
/// sane band (height 200..=900, width at most 1280) by trading the
/// other dimension — the window still opens void-free, just not at
/// a postage-stamp or billboard size. Pure math; unit-tested.
pub fn player_fit_size(vw: u32, vh: u32) -> (f64, f64) {
    if vw == 0 || vh == 0 {
        return (560.0, 340.0);
    }
    let aspect = vw as f64 / vh as f64;
    let mut w = 640.0;
    let mut h = w / aspect;
    if h > 900.0 {
        h = 900.0;
        w = h * aspect;
    }
    if h < 200.0 {
        h = 200.0;
        w = h * aspect;
    }
    if w > 1280.0 {
        w = 1280.0;
        h = w / aspect;
    }
    (w.round(), h.round())
}

/// Stream a live H.264 transcode of `path` (from `start` seconds) to
/// the socket as fragmented MP4 (Round 7.1): the playback fallback
/// for formats WebKitGTK cannot render (HEVC 10-bit). VA-API hardware
/// pipeline when the probe passes, software libx264 otherwise. No
/// temp file; when the client goes away the copy loop breaks and
/// ffmpeg is killed.
fn serve_transcode(mut stream: std::net::TcpStream, path: &std::path::Path, start: u64) {
    use std::io::{Read, Write};
    let path_str = path.to_string_lossy().to_string();
    // Fit inside 1920x1080 preserving aspect, even dimensions, never
    // upscale. Unknown dimensions fall back to filter expressions.
    let (sw, sh) = match probe_video_dims(path) {
        Some((w, h)) => {
            let scale = ((1920.0 / w as f64).min(1080.0 / h as f64)).min(1.0);
            let tw = (((w as f64 * scale) as u32) & !1).max(2);
            let th = (((h as f64 * scale) as u32) & !1).max(2);
            (tw.to_string(), th.to_string())
        }
        None => ("min(1920,iw)".to_string(), "-2".to_string()),
    };
    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error"]);
    if start > 0 {
        cmd.args(["-ss", &start.to_string()]);
    }
    if vaapi_works() && sw != "min(1920,iw)" {
        cmd.args([
            "-hwaccel",
            "vaapi",
            "-hwaccel_device",
            "/dev/dri/renderD128",
            "-hwaccel_output_format",
            "vaapi",
            "-i",
            &path_str,
            "-vf",
            &format!("scale_vaapi=w={sw}:h={sh}:format=nv12"),
            "-c:v",
            "h264_vaapi",
            "-b:v",
            "8M",
            "-c:a",
            "aac",
            "-b:a",
            "160k",
        ]);
    } else {
        cmd.args([
            "-i",
            &path_str,
            "-vf",
            &format!("scale=w={sw}:h={sh},format=yuv420p"),
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-crf",
            "21",
            "-c:a",
            "aac",
            "-b:a",
            "160k",
        ]);
    }
    cmd.args([
        "-movflags",
        "frag_keyframe+empty_moov+default_base_moof",
        "-f",
        "mp4",
        "pipe:1",
    ]);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::null());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => {
            let _ = stream.write_all(
                b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n",
            );
            return;
        }
    };
    if stream
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
        )
        .is_err()
    {
        let _ = child.kill();
        return;
    }
    let mut stdout = child.stdout.take().unwrap();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        match stdout.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if stream.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
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

/// Parse `catt scan` output. REAL format (verified live
/// 2026-10-15, catt 0.13.3): "192.168.12.226 - Natalie Vizio -
/// Vizio VFD40M-0809" — IP FIRST, then name, then model. The
/// original parser assumed name-first and silently swapped name
/// and address (playback survived only because `catt -d` accepts
/// an IP too; the picker showed IPs as names). Robust version:
/// whichever field parses as an IPv4 address is the address; the
/// field after it (or the first field when the IP trails) is the
/// name. Pure; unit-tested.
pub fn parse_catt_scan(out: &str) -> Vec<CastDevice> {
    let mut devices = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() || !line.contains(" - ") {
            continue;
        }
        let parts: Vec<&str> = line.split(" - ").map(|p| p.trim()).collect();
        let ip_at = parts
            .iter()
            .position(|p| p.parse::<std::net::Ipv4Addr>().is_ok());
        let Some(ip_at) = ip_at else { continue };
        let address = parts[ip_at].to_string();
        let name = if ip_at + 1 < parts.len() && !parts[ip_at + 1].is_empty() {
            parts[ip_at + 1].to_string()
        } else if ip_at > 0 {
            parts[0].to_string()
        } else {
            address.clone()
        };
        devices.push(CastDevice {
            name,
            kind: "chromecast".into(),
            address,
        });
    }
    devices
}

/// Scan the LAN for cast targets: Chromecasts (catt), Apple TVs
/// (pyatv atvremote), Sonos speakers (soco), DLNA renderers (SSDP).
#[tauri::command]
pub async fn cast_scan() -> Cmd<Vec<CastDevice>> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut devices: Vec<CastDevice> = Vec::new();
        if let Ok(out) = run_with_timeout(&local_bin("catt"), &["scan"], 12) {
            devices.extend(parse_catt_scan(&out));
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
        // DLNA renderers: native SSDP, no helper needed (Player
        // round). A renderer's `address` is its AVTransport
        // control URL — the actionable endpoint, SOAP speaks to it
        // directly. Deduped against Sonos by name (a Sonos speaker
        // is also a DLNA renderer; soco is the richer driver, so
        // the DLNA twin stays hidden).
        for d in dlna_scan() {
            if !devices
                .iter()
                .any(|x| x.kind == "sonos" && x.name.eq_ignore_ascii_case(&d.name))
            {
                devices.push(d);
            }
        }
        Ok::<Vec<CastDevice>, String>(devices)
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("scan task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

fn soco_python_path() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    format!("{home}/.local/share/pipx/venvs/soco/bin/python")
}

fn cast_tool_found(bin: &str) -> bool {
    let home = std::env::var("HOME").unwrap_or_default();
    std::path::Path::new(&format!("{home}/.local/bin/{bin}")).exists()
        || crate::deps::tool_present(bin)
}

/// Which cast backends are NOT set up on this computer yet
/// ("catt" = Chromecast, "atvremote" = AirPlay, "soco" = Sonos)?
/// The cast dialog offers one-click setup when this is non-empty
/// — without it, a scan on a fresh machine found nothing and
/// blamed the network, which was a silent dependency dead-end.
#[tauri::command]
pub async fn cast_backends() -> Cmd<Vec<String>> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut missing: Vec<String> = Vec::new();
        if !cast_tool_found("catt") {
            missing.push("catt".to_string());
        }
        if !cast_tool_found("atvremote") {
            missing.push("atvremote".to_string());
        }
        if !std::path::Path::new(&soco_python_path()).exists() {
            missing.push("soco".to_string());
        }
        Ok::<Vec<String>, String>(missing)
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("cast task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Set up the missing cast backends: pipx itself installs
/// through the OS package machinery when absent (crate::deps);
/// the backends are user-level pipx installs (catt, pyatv, soco —
/// no admin password). Returns whatever is still missing.
#[tauri::command]
pub async fn cast_setup() -> Cmd<Vec<String>> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::deps::ensure_tool("pipx", "pipx")?;
        for (backend, package) in [("catt", "catt"), ("atvremote", "pyatv")] {
            if cast_tool_found(backend) {
                continue;
            }
            let out = std::process::Command::new("pipx")
                .args(["install", package])
                .output()
                .map_err(|e| format!("pipx: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "Setting up the {backend} helper didn't finish: {}. Nothing else is affected — you can try again from the same button.",
                    crate::deps::output_tail(&out)
                ));
            }
        }
        // soco is a LIBRARY, not an app: `pipx install soco` can
        // never succeed ("No apps associated with package soco").
        // The cast code only needs a venv whose python imports
        // soco, at the path soco_python_path() already points to —
        // so build exactly that venv with python3 -m venv + pip.
        // (Player round; proven by hand on the G9, soco 0.32.0.)
        if !std::path::Path::new(&soco_python_path()).exists() {
            ensure_soco_venv()?;
        }
        let mut missing: Vec<String> = Vec::new();
        if !cast_tool_found("catt") {
            missing.push("catt".to_string());
        }
        if !cast_tool_found("atvremote") {
            missing.push("atvremote".to_string());
        }
        if !std::path::Path::new(&soco_python_path()).exists() {
            missing.push("soco".to_string());
        }
        Ok::<Vec<String>, String>(missing)
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("cast setup task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Playback setup state (Gauntlet G1): is the host GStreamer
/// plugin set on this system, and — on the families whose full
/// codec set lives in a third-party source (Fedora/RPM Fusion,
/// openSUSE/Packman) — what is that source called, and is it
/// enabled yet? The player's setup strip renders from this;
/// probing is file checks only, cheap enough for a door.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackSetupStatus {
    pub plugins_present: bool,
    /// "RPM Fusion" / "Packman" on the families that have one.
    pub repo_name: Option<String>,
    pub repo_enabled: bool,
}

fn playback_setup_status_now() -> PlaybackSetupStatus {
    let fam = crate::deps::distro_family();
    let repo = crate::deps::third_party_repo(fam);
    PlaybackSetupStatus {
        plugins_present: crate::deps::gst_plugins_present(),
        repo_name: repo.map(|r| r.name.to_string()),
        repo_enabled: repo.map(|_| crate::deps::repo_enabled(fam)).unwrap_or(false),
    }
}

#[tauri::command]
pub async fn playback_setup_status() -> Cmd<PlaybackSetupStatus> {
    tauri::async_runtime::spawn_blocking(playback_setup_status_now)
        .await
        .map_err(|e| CommandError::from(UfmError::Internal(format!("playback status task: {e}"))))
}

/// The playback-setup door's one action, clicked BY the user on
/// the player's setup strip (never automatic): install the
/// GStreamer plugin set. With `with_repo` — a separate, explicit
/// click after the strip explained the source in words — the
/// family's third-party codec source is enabled first (the
/// pkexec prompt is the admin OK) and the FULL media set goes
/// in, ffmpeg included, so conversions light up from the same
/// consent. Returns the fresh status either way; an install
/// that didn't go through is the calm error (the receipt).
#[tauri::command]
pub async fn playback_setup_install(with_repo: bool) -> Cmd<PlaybackSetupStatus> {
    tauri::async_runtime::spawn_blocking(move || {
        let fam = crate::deps::distro_family();
        let repo = crate::deps::third_party_repo(fam);
        if with_repo {
            if let Some(r) = repo {
                if !crate::deps::repo_enabled(fam) {
                    crate::deps::enable_third_party_repo(fam).map_err(|e| {
                        format!(
                            "Fable couldn't add the {} source: {e}. Nothing was installed, and you can try again from the same button.",
                            r.name
                        )
                    })?;
                }
            }
        }
        let full = repo.is_some() && (with_repo || crate::deps::repo_enabled(fam));
        let pkgs = if full {
            crate::deps::media_full_packages(fam)
        } else {
            crate::deps::packages_for(fam, "gst-plugins")
                .map(|p| p.to_vec())
                .ok_or_else(|| {
                    "Fable doesn't know the playback packages for this system, so it can't set them up automatically."
                        .to_string()
                })?
        };
        crate::deps::install_packages(&pkgs)
            .map_err(|e| format!("Fable couldn't set up playback on this system: {e}. Nothing else is affected, and you can try again from the same button."))?;
        Ok::<PlaybackSetupStatus, String>(playback_setup_status_now())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("playback setup task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Start playback of a media URL on a cast device.
#[tauri::command]
pub async fn cast_play(
    state: State<'_, AppState>,
    device: CastDevice,
    url: String,
) -> Cmd<()> {
    // DLNA wants DIDL-Lite metadata (title + real MIME) with the
    // URI; reverse the media-server token back to its file to
    // build it. Other backends ignore this.
    let didl = {
        let token = url.rsplit('/').next().unwrap_or("");
        let path = state.media_files.lock().unwrap().get(token).cloned();
        match path {
            Some(p) => {
                let title = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Video".to_string());
                dlna_didl(&url, &title, &guess_mime(&p))
            }
            None => String::new(),
        }
    };
    tauri::async_runtime::spawn_blocking(move || {
        match device.kind.as_str() {
            "dlna" => {
                let args = format!(
                    "<InstanceID>0</InstanceID><CurrentURI>{}</CurrentURI><CurrentURIMetaData>{}</CurrentURIMetaData>",
                    xml_escape(&url),
                    xml_escape(&didl)
                );
                dlna_soap(&device.address, "SetAVTransportURI", &args)?;
                dlna_soap(
                    &device.address,
                    "Play",
                    "<InstanceID>0</InstanceID><Speed>1</Speed>",
                )?;
                Ok::<(), String>(())
            }
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
            "dlna" => {
                dlna_soap(&device.address, "Stop", "<InstanceID>0</InstanceID>")?;
            }
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

/// Create the soco venv directly (see cast_setup): pipx refuses
/// library packages, but the cast code only needs a python that
/// imports soco at soco_python_path().
fn ensure_soco_venv() -> std::result::Result<(), String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let venv = format!("{home}/.local/share/pipx/venvs/soco");
    if std::path::Path::new(&soco_python_path()).exists() {
        return Ok(());
    }
    let out = std::process::Command::new("python3")
        .args(["-m", "venv", &venv])
        .output()
        .map_err(|e| format!("python3: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "Setting up the Sonos helper didn't finish: {}. Nothing else is affected — you can try again from the same button.",
            crate::deps::output_tail(&out)
        ));
    }
    let pip = format!("{venv}/bin/pip");
    let out = std::process::Command::new(&pip)
        .args(["install", "--quiet", "soco"])
        .output()
        .map_err(|e| format!("pip: {e}"))?;
    if !out.status.success() || !std::path::Path::new(&soco_python_path()).exists() {
        return Err(format!(
            "Setting up the Sonos helper didn't finish: {}. Nothing else is affected — you can try again from the same button.",
            crate::deps::output_tail(&out)
        ));
    }
    Ok(())
}

/// Register a file by PATH with the media server (Player round):
/// the video player is path-based in both its hosts (Quick Look
/// and the detach window), so its cast flow cannot go through the
/// item-id registration the file browser uses.
#[tauri::command]
pub async fn cast_register_path(
    state: State<'_, AppState>,
    path: String,
) -> Cmd<String> {
    let p = std::path::PathBuf::from(&path);
    if !p.is_file() {
        return Err(CommandError::from(UfmError::NotFound(format!(
            "no file at {path}"
        ))));
    }
    let port = media_server_port(state.inner())
        .map_err(|e| CommandError::from(UfmError::Internal(e)))?;
    let token = format!("{}", ItemId::new());
    state.media_files.lock().unwrap().insert(token.clone(), p);
    Ok(format!("http://{}:{}/cast/{}", lan_ip(), port, token))
}

// ---------------------------------------------------------------------------
// Player round — cast transport + DLNA.
//
// The picker used to be fire-and-forget (play / stop, nothing in
// between). These are the verbs that let the player's own
// transport drive the receiver — pause, resume, seek — plus a
// status poll the scrubber syncs to. DLNA renderers join the
// picker through native SSDP discovery + SOAP AVTransport, the
// one family that needs no helper download at all.
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
pub struct CastStatus {
    /// playing | paused | idle | unknown
    pub state: String,
    /// Seconds into the media (0 when unknown).
    pub position: f64,
    /// Seconds total (0 when unknown).
    pub duration: f64,
}

impl CastStatus {
    fn unknown() -> Self {
        CastStatus {
            state: "unknown".into(),
            position: 0.0,
            duration: 0.0,
        }
    }
}

/// Parse `catt info -j` JSON (player_state / current_time /
/// duration — shape verified against catt 0.13.3's cast_info).
pub fn parse_catt_status(json: &str) -> CastStatus {
    let root: serde_json::Value =
        serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
    let v = root.get("info").unwrap_or(&root);
    let num = |k: &str| v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0);
    let state = match v.get("player_state").and_then(|x| x.as_str()).unwrap_or("") {
        "PLAYING" | "BUFFERING" => "playing",
        "PAUSED" => "paused",
        "IDLE" => "idle",
        _ => "unknown",
    };
    CastStatus {
        state: state.into(),
        position: num("current_time"),
        duration: num("duration"),
    }
}

/// Parse `atvscript playing` JSON (device_state / position /
/// total_time). pyatv states arrive lowercase.
pub fn parse_atv_status(json: &str) -> CastStatus {
    let v: serde_json::Value =
        serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
    let num = |k: &str| v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0);
    let state = match v
        .get("device_state")
        .and_then(|x| x.as_str())
        .unwrap_or("")
    {
        "playing" | "loading" | "seeking" => "playing",
        "paused" => "paused",
        "idle" | "stopped" => "idle",
        _ => "unknown",
    };
    CastStatus {
        state: state.into(),
        position: num("position"),
        duration: num("total_time"),
    }
}

/// "1:02:03" / "02:03" / "90" → seconds. soco, DLNA and friends
/// all speak H:MM:SS; junk ("NOT_IMPLEMENTED", "--:--") is 0.
pub fn parse_hms(s: &str) -> f64 {
    let s = s.trim();
    if s.is_empty() || !s.chars().next().unwrap().is_ascii_digit() {
        return 0.0;
    }
    let parts: Vec<f64> = s
        .split(':')
        .map(|p| p.trim().parse().unwrap_or(0.0))
        .collect();
    match parts.len() {
        1 => parts[0],
        2 => parts[0] * 60.0 + parts[1],
        _ => parts[0] * 3600.0 + parts[1] * 60.0 + parts[2],
    }
}

/// Seconds → "H:MM:SS" (DLNA REL_TIME / soco seek format).
pub fn hms_of(secs: f64) -> String {
    let t = secs.max(0.0) as u64;
    format!("{}:{:02}:{:02}", t / 3600, (t % 3600) / 60, t % 60)
}

/// Parse the soco status one-liner this module prints:
/// "STATE|position|duration" with H:MM:SS times.
pub fn parse_soco_status(line: &str) -> CastStatus {
    let mut it = line.trim().splitn(3, '|');
    let raw = it.next().unwrap_or("");
    let state = match raw {
        "PLAYING" | "TRANSITIONING" => "playing",
        "PAUSED_PLAYBACK" => "paused",
        "STOPPED" | "NO_MEDIA_PRESENT" => "idle",
        _ => "unknown",
    };
    CastStatus {
        state: state.into(),
        position: it.next().map(parse_hms).unwrap_or(0.0),
        duration: it.next().map(parse_hms).unwrap_or(0.0),
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Text of the first `<tag>…</tag>` in a small XML payload (SSDP /
/// SOAP / device descriptions are regular enough that a full XML
/// parser would be overkill — and the crate has none).
pub fn xml_tag_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    let raw = xml[start..end].trim();
    Some(
        raw.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&"),
    )
}

/// Resolve a possibly-relative UPnP URL against its base
/// (description LOCATION or URLBase). Pure; unit-tested.
pub fn resolve_http_url(base: &str, rel: &str) -> String {
    if rel.starts_with("http://") || rel.starts_with("https://") {
        return rel.to_string();
    }
    let after_scheme = base.find("://").map(|i| i + 3).unwrap_or(0);
    let origin_end = base[after_scheme..]
        .find('/')
        .map(|j| after_scheme + j)
        .unwrap_or(base.len());
    let origin = &base[..origin_end];
    if let Some(stripped) = rel.strip_prefix('/') {
        return format!("{origin}/{stripped}");
    }
    if origin_end >= base.len() {
        return format!("{origin}/{rel}");
    }
    let dir = &base[..=base.rfind('/').unwrap_or(origin_end)];
    format!("{dir}{rel}")
}

/// Parse a UPnP device description: (friendlyName, AVTransport
/// control URL resolved absolute). None when the device offers no
/// AVTransport service (not a renderer we can drive).
pub fn parse_dlna_description(xml: &str, location: &str) -> Option<(String, String)> {
    let name = xml_tag_text(xml, "friendlyName").filter(|s| !s.is_empty())?;
    let avt = xml.find("urn:schemas-upnp-org:service:AVTransport")?;
    let control = xml_tag_text(&xml[avt..], "controlURL")?;
    let base = xml_tag_text(xml, "URLBase").unwrap_or_else(|| location.to_string());
    Some((name, resolve_http_url(&base, &control)))
}

/// The LOCATION header of an SSDP response (headers are
/// case-insensitive; values may carry a trailing \r).
pub fn parse_ssdp_location(resp: &str) -> Option<String> {
    for line in resp.lines() {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("location") {
                let v = v.trim();
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

/// SSDP discovery of DLNA MediaRenderers (multicast M-SEARCH,
/// ~2.6 s listen window), then a description fetch per unique
/// LOCATION. Runs inside cast_scan's blocking task.
fn dlna_scan() -> Vec<CastDevice> {
    use std::net::UdpSocket;
    let mut locations: Vec<String> = Vec::new();
    if let Ok(sock) = UdpSocket::bind("0.0.0.0:0") {
        let _ = sock.set_read_timeout(Some(std::time::Duration::from_millis(500)));
        let search = "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\n\
                      MAN: \"ssdp:discover\"\r\nMX: 2\r\n\
                      ST: urn:schemas-upnp-org:device:MediaRenderer:1\r\n\r\n";
        for _ in 0..2 {
            let _ = sock.send_to(search.as_bytes(), "239.255.255.250:1900");
        }
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_millis(2600);
        let mut buf = [0u8; 4096];
        while std::time::Instant::now() < deadline {
            if let Ok((n, _)) = sock.recv_from(&mut buf) {
                let text = String::from_utf8_lossy(&buf[..n]);
                if let Some(loc) = parse_ssdp_location(&text) {
                    if !locations.contains(&loc) {
                        locations.push(loc);
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    for loc in locations {
        let xml = ureq::AgentBuilder::new()
            .build()
            .get(&loc)
            .timeout(std::time::Duration::from_secs(3))
            .call()
            .ok()
            .and_then(|r| r.into_string().ok());
        if let Some(xml) = xml {
            if let Some((name, control)) = parse_dlna_description(&xml, &loc) {
                out.push(CastDevice {
                    name,
                    kind: "dlna".into(),
                    address: control,
                });
            }
        }
    }
    out
}

/// DIDL-Lite metadata for SetAVTransportURI. Many renderers show
/// the title from here and gate playback on the res MIME.
pub fn dlna_didl(url: &str, title: &str, mime: &str) -> String {
    format!(
        "<DIDL-Lite xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\" \
         xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
         xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\">\
         <item id=\"0\" parentID=\"0\" restricted=\"1\">\
         <dc:title>{}</dc:title>\
         <upnp:class>object.item.videoItem</upnp:class>\
         <res protocolInfo=\"http-get:*:{}:*\">{}</res>\
         </item></DIDL-Lite>",
        xml_escape(title),
        mime,
        xml_escape(url)
    )
}

/// One AVTransport SOAP call; returns the response body. SOAP
/// faults arrive as HTTP 500 WITH an XML body — for Get* actions
/// the body may still carry the data, so a status error hands its
/// body back instead of failing (callers parse defensively).
fn dlna_soap(
    control_url: &str,
    action: &str,
    args: &str,
) -> std::result::Result<String, String> {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\">\
         <s:Body><u:{action} xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\">\
         {args}</u:{action}></s:Body></s:Envelope>"
    );
    // Fresh agent per call: renderers close sockets freely, and a
    // pooled stale connection fails harder than a fresh dial.
    let req = ureq::AgentBuilder::new()
        .build()
        .post(control_url)
        .set("Content-Type", "text/xml; charset=\"utf-8\"")
        .set(
            "SOAPACTION",
            &format!("\"urn:schemas-upnp-org:service:AVTransport:1#{action}\""),
        )
        .timeout(std::time::Duration::from_secs(8));
    match req.send_string(&body) {
        Ok(resp) => resp
            .into_string()
            .map_err(|e| format!("DLNA {action}: {e}")),
        Err(ureq::Error::Status(_, resp)) => resp
            .into_string()
            .map_err(|e| format!("DLNA {action}: {e}")),
        Err(e) => Err(format!("DLNA {action}: {e}")),
    }
}

fn dlna_status(control_url: &str) -> std::result::Result<CastStatus, String> {
    let info = dlna_soap(control_url, "GetTransportInfo", "<InstanceID>0</InstanceID>")?;
    let raw = xml_tag_text(&info, "CurrentTransportState").unwrap_or_default();
    let state = match raw.as_str() {
        "PLAYING" | "TRANSITIONING" => "playing",
        "PAUSED_PLAYBACK" | "PAUSED" => "paused",
        "STOPPED" | "NO_MEDIA_PRESENT" => "idle",
        _ => "unknown",
    };
    let (mut position, mut duration) = (0.0, 0.0);
    if let Ok(p) = dlna_soap(control_url, "GetPositionInfo", "<InstanceID>0</InstanceID>") {
        position = xml_tag_text(&p, "RelTime").map(|s| parse_hms(&s)).unwrap_or(0.0);
        duration = xml_tag_text(&p, "TrackDuration")
            .map(|s| parse_hms(&s))
            .unwrap_or(0.0);
    }
    Ok(CastStatus {
        state: state.into(),
        position,
        duration,
    })
}

fn soco_py(script: &str, secs: u64) -> std::result::Result<String, String> {
    run_with_timeout(&soco_python_path(), &["-c", script], secs)
}

fn cast_transport_sync(
    device: &CastDevice,
    action: &str,
    seconds: f64,
) -> std::result::Result<(), String> {
    match device.kind.as_str() {
        "chromecast" => {
            let args: Vec<String> = match action {
                "pause" => vec!["-d".into(), device.name.clone(), "pause".into()],
                "resume" => vec!["-d".into(), device.name.clone(), "play".into()],
                "seek" => vec![
                    "-d".into(),
                    device.name.clone(),
                    "seek".into(),
                    format!("{}", seconds.max(0.0) as u64),
                ],
                _ => return Err(format!("bad cast action {action}")),
            };
            let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            run_with_timeout(&local_bin("catt"), &refs, 15)?;
        }
        "airplay" => {
            let arg = match action {
                "pause" => "pause".to_string(),
                "resume" => "play".to_string(),
                "seek" => format!("set_position={}", seconds.max(0.0) as u64),
                _ => return Err(format!("bad cast action {action}")),
            };
            run_with_timeout(
                &local_bin("atvremote"),
                &["-s", &device.address, &arg],
                15,
            )?;
        }
        "sonos" => {
            let addr = device.address.replace('\'', "");
            let script = match action {
                "pause" => format!("from soco import SoCo\nSoCo('{addr}').pause()"),
                "resume" => format!("from soco import SoCo\nSoCo('{addr}').play()"),
                "seek" => format!(
                    "from soco import SoCo\nSoCo('{addr}').seek('{}')",
                    hms_of(seconds)
                ),
                _ => return Err(format!("bad cast action {action}")),
            };
            soco_py(&script, 15)?;
        }
        "dlna" => {
            let args = match action {
                "pause" => ("Pause", "<InstanceID>0</InstanceID>".to_string()),
                "resume" => (
                    "Play",
                    "<InstanceID>0</InstanceID><Speed>1</Speed>".to_string(),
                ),
                "seek" => (
                    "Seek",
                    format!(
                        "<InstanceID>0</InstanceID><Unit>REL_TIME</Unit><Target>{}</Target>",
                        hms_of(seconds)
                    ),
                ),
                _ => return Err(format!("bad cast action {action}")),
            };
            dlna_soap(&device.address, args.0, &args.1)?;
        }
        other => return Err(format!("unknown device kind {other}")),
    }
    Ok(())
}

fn cast_status_sync(device: &CastDevice) -> std::result::Result<CastStatus, String> {
    match device.kind.as_str() {
        "chromecast" => {
            // catt errors when nothing is loaded — that's an idle
            // receiver, not a failure the poll should surface.
            match run_with_timeout(&local_bin("catt"), &["-d", &device.name, "info", "-j"], 12) {
                Ok(out) => Ok(parse_catt_status(&out)),
                Err(_) => Ok(CastStatus {
                    state: "idle".into(),
                    position: 0.0,
                    duration: 0.0,
                }),
            }
        }
        "airplay" => {
            match run_with_timeout(&local_bin("atvscript"), &["-s", &device.address, "playing"], 12) {
                Ok(out) => Ok(parse_atv_status(&out)),
                Err(_) => Ok(CastStatus::unknown()),
            }
        }
        "sonos" => {
            let addr = device.address.replace('\'', "");
            let script = format!(
                "from soco import SoCo\nd = SoCo('{addr}')\nti = d.get_transport_info()\ninfo = d.get_current_track_info()\nprint(ti.get('current_transport_state', '') + '|' + (info.get('position') or '') + '|' + (info.get('duration') or ''))"
            );
            match soco_py(&script, 12) {
                Ok(out) => Ok(parse_soco_status(&out)),
                Err(_) => Ok(CastStatus::unknown()),
            }
        }
        "dlna" => dlna_status(&device.address),
        other => Err(format!("unknown device kind {other}")),
    }
}

/// Pause the receiver (the player's transport drives it now).
#[tauri::command]
pub async fn cast_pause(device: CastDevice) -> Cmd<()> {
    tauri::async_runtime::spawn_blocking(move || {
        cast_transport_sync(&device, "pause", 0.0)
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("cast task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Resume the receiver.
#[tauri::command]
pub async fn cast_resume(device: CastDevice) -> Cmd<()> {
    tauri::async_runtime::spawn_blocking(move || {
        cast_transport_sync(&device, "resume", 0.0)
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("cast task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Seek the receiver to `seconds`.
#[tauri::command]
pub async fn cast_seek(device: CastDevice, seconds: f64) -> Cmd<()> {
    tauri::async_runtime::spawn_blocking(move || {
        cast_transport_sync(&device, "seek", seconds)
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("cast task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Receiver state + position for the player's scrubber sync.
/// Helper-backed kinds report idle/unknown rather than erroring
/// when the receiver has nothing loaded (their CLIs exit nonzero
/// for an idle device — a state, not a failure).
#[tauri::command]
pub async fn cast_status(device: CastDevice) -> Cmd<CastStatus> {
    tauri::async_runtime::spawn_blocking(move || cast_status_sync(&device))
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

/// Uninstall an app (flatpak remove, a local-launcher delete,
/// or the family's own package removal for system apps —
/// Gauntlet G1 de-pacman'd this: owner query + removal speak
/// dpkg/apt, pacman, or rpm/dnf/zypper per deps.rs) and
/// optionally trash the selected leftovers.
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
                // System app: find the owning package and remove
                // it the family's way (deps.rs — this branch used
                // to shell pacman unconditionally, so it errored
                // on every apt/dnf/zypper system).
                let pkg = crate::deps::app_owner_query(&app.desktop_path)?
                    .ok_or("could not find the owning package")?;
                crate::deps::remove_app_package(&pkg)?;
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


// ---------------------------------------------------------------------------
// Disk Utility: inspect, format, erase, resize — with boot safety
// ---------------------------------------------------------------------------

#[derive(serde::Serialize, Clone)]
pub struct DiskPart {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    pub fstype: Option<String>,
    pub label: Option<String>,
    pub mountpoints: Vec<String>,
    pub locked: bool,
    pub lock_reason: Option<String>,
}

#[derive(serde::Serialize)]
pub struct DiskInfo {
    pub path: String,
    pub model: String,
    pub size_bytes: u64,
    pub removable: bool,
    pub transport: String,
    pub partitions: Vec<DiskPart>,
    pub locked: bool,
    pub lock_reason: Option<String>,
}

fn lsblk_json() -> std::result::Result<serde_json::Value, String> {
    let out = std::process::Command::new("lsblk")
        .args([
            "-J", "-b", "-o",
            "NAME,PATH,SIZE,TYPE,FSTYPE,LABEL,MOUNTPOINT,MODEL,RM,TRAN",
        ])
        .output()
        .map_err(|e| format!("lsblk: {e}"))?;
    serde_json::from_slice(&out.stdout).map_err(|e| format!("lsblk json: {e}"))
}

/// Mount sources of the running system ("/dev/mapper/root" style
/// paths), subvolume suffixes stripped.
fn system_mount_sources() -> Vec<String> {
    let mut sources = Vec::new();
    for mp in ["/", "/boot", "/boot/efi", "/home"] {
        if let Ok(out) = std::process::Command::new("findmnt")
            .args(["-no", "SOURCE", mp])
            .output()
        {
            let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let base = raw.split('[').next().unwrap_or("").trim().to_string();
            if base.starts_with("/dev/") {
                sources.push(base);
            }
        }
    }
    sources
}

fn active_swap_devices() -> Vec<String> {
    std::fs::read_to_string("/proc/swaps")
        .map(|content| {
            content
                .lines()
                .skip(1)
                .filter_map(|l| l.split_whitespace().next())
                .filter(|p| p.starts_with("/dev/"))
                .map(|p| p.to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn subtree_mountpoints(node: &serde_json::Value, out: &mut Vec<String>) {
    if let Some(mp) = node.get("mountpoint").and_then(|v| v.as_str()) {
        if !mp.is_empty() {
            out.push(mp.to_string());
        }
    }
    // lsblk -o MOUNTPOINT is single; MOUNTPOINTS (plural) unused here.
    if let Some(children) = node.get("children").and_then(|v| v.as_array()) {
        for child in children {
            subtree_mountpoints(child, out);
        }
    }
}

fn subtree_has_path(node: &serde_json::Value, paths: &[String]) -> bool {
    if let Some(p) = node.get("path").and_then(|v| v.as_str()) {
        if paths.iter().any(|x| x == p) {
            return true;
        }
    }
    if let Some(children) = node.get("children").and_then(|v| v.as_array()) {
        return children.iter().any(|c| subtree_has_path(c, paths));
    }
    false
}

const SYSTEM_MOUNTS: [&str; 4] = ["/", "/boot", "/boot/efi", "/home"];

/// Full disk inventory with safety locks computed fresh.
fn disk_inventory() -> std::result::Result<Vec<DiskInfo>, String> {
    let parsed = lsblk_json()?;
    let sys_sources = system_mount_sources();
    let swaps = active_swap_devices();
    let mut disks = Vec::new();
    let Some(list) = parsed.get("blockdevices").and_then(|v| v.as_array()) else {
        return Ok(disks);
    };
    for node in list {
        if node.get("type").and_then(|v| v.as_str()) != Some("disk") {
            continue;
        }
        let disk_path = node
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let mut partitions = Vec::new();
        let mut disk_locked = false;
        let mut disk_reason: Option<String> = None;
        if let Some(children) = node.get("children").and_then(|v| v.as_array()) {
            for part in children {
                if part.get("type").and_then(|v| v.as_str()) != Some("part") {
                    continue;
                }
                let mut mps = Vec::new();
                subtree_mountpoints(part, &mut mps);
                let ppath = part
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let mut locked = false;
                let mut reason: Option<String> = None;
                if mps.iter().any(|m| SYSTEM_MOUNTS.contains(&m.as_str()))
                    || subtree_has_path(part, &sys_sources)
                {
                    locked = true;
                    reason = Some(
                        "This is part of the system you're running right now —                          formatting it isn't possible (or wise) from in here."
                            .into(),
                    );
                } else if swaps.iter().any(|d| d == &ppath) {
                    locked = true;
                    reason = Some("This partition is active swap right now.".into());
                }
                if locked {
                    disk_locked = true;
                    disk_reason = Some(
                        "This disk holds the system you're running — it can't be erased as a whole.                          Its system partitions are protected; unrelated data partitions on it can still be formatted."
                            .into(),
                    );
                }
                partitions.push(DiskPart {
                    name: part
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    path: ppath,
                    size_bytes: part.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
                    fstype: part
                        .get("fstype")
                        .and_then(|v| v.as_str())
                        .map(|x| x.to_string()),
                    label: part
                        .get("label")
                        .and_then(|v| v.as_str())
                        .map(|x| x.to_string()),
                    mountpoints: mps,
                    locked,
                    lock_reason: reason,
                });
            }
        }
        disks.push(DiskInfo {
            path: disk_path,
            model: node
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("Disk")
                .trim()
                .to_string(),
            size_bytes: node.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
            removable: node.get("rm").and_then(|v| v.as_bool()).unwrap_or(false),
            transport: node
                .get("tran")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            partitions,
            locked: disk_locked,
            lock_reason: disk_reason,
        });
    }
    Ok(disks)
}

#[tauri::command]
pub async fn list_disks() -> Cmd<Vec<DiskInfo>> {
    tauri::async_runtime::spawn_blocking(move || disk_inventory())
        .await
        .map_err(|e| CommandError::from(UfmError::Internal(format!("disk task failed: {e}"))))?
        .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Which filesystems this machine can actually create.
#[tauri::command]
pub async fn available_filesystems() -> Cmd<Vec<String>> {
    Ok([
        "ext4", "btrfs", "exfat", "vfat", "ntfs", "xfs", "f2fs",
    ]
    .iter()
    .filter(|fs| {
        std::path::Path::new(&format!("/sbin/mkfs.{fs}")).exists()
            || std::path::Path::new(&format!("/usr/sbin/mkfs.{fs}")).exists()
    })
    .map(|fs| fs.to_string())
    .collect())
}

fn check_device_operable(
    device: &str,
    whole_disk: bool,
) -> std::result::Result<DiskPart, String> {
    let inv = disk_inventory()?;
    for disk in &inv {
        if whole_disk && disk.path == device {
            if disk.locked {
                return Err(disk
                    .lock_reason
                    .clone()
                    .unwrap_or_else(|| "disk is protected".into()));
            }
            return Ok(DiskPart {
                name: disk.path.clone(),
                path: disk.path.clone(),
                size_bytes: disk.size_bytes,
                fstype: None,
                label: None,
                mountpoints: vec![],
                locked: false,
                lock_reason: None,
            });
        }
        for part in &disk.partitions {
            if part.path == device {
                if part.locked {
                    return Err(part
                        .lock_reason
                        .clone()
                        .unwrap_or_else(|| "partition is protected".into()));
                }
                if !part.mountpoints.is_empty() {
                    return Err(format!(
                        "still mounted at {} — unmount it first, so nothing is writing while we work",
                        part.mountpoints.join(", ")
                    ));
                }
                return Ok(part.clone());
            }
        }
    }
    Err(format!("{device} is not a known disk or partition"))
}

fn sanitize_label(label: &str) -> String {
    label
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-' || *c == '_')
        .take(32)
        .collect::<String>()
        .trim()
        .to_string()
}

fn mkfs_command(fstype: &str, label: &str, device: &str) -> std::result::Result<String, String> {
    let label = sanitize_label(label);
    let lflag = if label.is_empty() {
        String::new()
    } else {
        match fstype {
            "exfat" => format!(" -n '{label}'"),
            "vfat" => format!(" -n '{}'", label.to_uppercase()),
            "f2fs" => format!(" -l '{label}'"),
            _ => format!(" -L '{label}'"),
        }
    };
    Ok(match fstype {
        "ext4" => format!("mkfs.ext4 -F{lflag} '{device}'"),
        "btrfs" => format!("mkfs.btrfs -f{lflag} '{device}'"),
        "exfat" => format!("mkfs.exfat{lflag} '{device}'"),
        "vfat" => format!("mkfs.vfat -F 32{lflag} '{device}'"),
        "ntfs" => format!("mkfs.ntfs -f{lflag} '{device}'"),
        "xfs" => format!("mkfs.xfs -f{lflag} '{device}'"),
        "f2fs" => format!("mkfs.f2fs -f{lflag} '{device}'"),
        other => return Err(format!("unsupported filesystem {other}")),
    })
}

fn pkexec_sh(script: &str) -> std::result::Result<(), String> {
    let out = std::process::Command::new("pkexec")
        .arg("sh")
        .arg("-c")
        .arg(script)
        .output()
        .map_err(|e| format!("pkexec: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        let last = err.lines().last().unwrap_or("").trim().to_string();
        return Err(if last.is_empty() {
            "the operation failed (or the password prompt was cancelled)".into()
        } else {
            last
        });
    }
    Ok(())
}

/// Format one partition. Safety is re-verified server-side at call
/// time: protected (system/swap) or mounted partitions are refused.
#[tauri::command]
pub async fn format_partition(
    device: String,
    fstype: String,
    label: String,
) -> Cmd<()> {
    tauri::async_runtime::spawn_blocking(move || {
        check_device_operable(&device, false)?;
        let cmd = mkfs_command(&fstype, &label, &device)?;
        pkexec_sh(&cmd)?;
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("format task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Erase a whole disk: wipe signatures, fresh GPT, one partition
/// filling the disk, formatted. Refused for the boot disk.
#[tauri::command]
pub async fn erase_disk(device: String, fstype: String, label: String) -> Cmd<()> {
    tauri::async_runtime::spawn_blocking(move || {
        check_device_operable(&device, true)?;
        let part_dev = if device
            .chars()
            .last()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
        {
            format!("{device}p1")
        } else {
            format!("{device}1")
        };
        let mkfs = mkfs_command(&fstype, &label, &part_dev)?;
        let script = format!(
            "wipefs -a '{device}' && parted -s '{device}' mklabel gpt mkpart primary 1MiB 100% && partprobe '{device}' && udevadm settle && {mkfs}"
        );
        pkexec_sh(&script)?;
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("erase task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

#[derive(serde::Serialize)]
pub struct ResizeInfo {
    pub size_bytes: u64,
    pub free_after_bytes: u64,
    pub can_grow: bool,
    pub can_shrink: bool,
    pub note: String,
}

/// Resize feasibility for a partition: contiguous free space after
/// it (parted), plus per-filesystem rules.
#[tauri::command]
pub async fn partition_resize_info(device: String) -> Cmd<ResizeInfo> {
    tauri::async_runtime::spawn_blocking(move || {
        let inv = disk_inventory()?;
        let mut found: Option<(DiskPart, String)> = None;
        for disk in &inv {
            for part in &disk.partitions {
                if part.path == device {
                    found = Some((part.clone(), disk.path.clone()));
                }
            }
        }
        let (part, disk_path) = found.ok_or("unknown partition")?;
        crate::deps::ensure_tool("parted", "parted")?;
        // parted machine-readable free-space scan
        let out = std::process::Command::new("parted")
            .args(["-m", &disk_path, "unit", "B", "print", "free"])
            .output()
            .map_err(|e| format!("parted: {e}"))?;
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let part_num: String = device
            .trim_start_matches(&disk_path)
            .trim_start_matches('p')
            .chars()
            .filter(|c| c.is_ascii_digit())
            .collect();
        let mut part_end: u64 = 0;
        let mut free_after: u64 = 0;
        for line in text.lines() {
            let fields: Vec<&str> = line.split(':').collect();
            if fields.len() < 5 {
                continue;
            }
            let parse_b = |v: &str| v.trim_end_matches('B').parse::<u64>().unwrap_or(0);
            if fields[0] == part_num {
                part_end = parse_b(fields[2]);
            }
        }
        for line in text.lines() {
            let fields: Vec<&str> = line.split(':').collect();
            if fields.len() < 5 {
                continue;
            }
            let parse_b = |v: &str| v.trim_end_matches('B').parse::<u64>().unwrap_or(0);
            let is_free = fields[4].contains("free");
            if is_free && part_end > 0 {
                let start = parse_b(fields[1]);
                if start >= part_end && start.saturating_sub(part_end) < 4 * 1024 * 1024 {
                    free_after = parse_b(fields[3]);
                }
            }
        }
        let fs = part.fstype.clone().unwrap_or_default();
        let mounted = !part.mountpoints.is_empty();
        let (can_grow, can_shrink, note) = match fs.as_str() {
            "ext4" => (
                free_after > 0 && !part.locked,
                !mounted && !part.locked,
                if mounted {
                    "ext4 can grow while mounted; shrinking needs the partition unmounted."
                } else {
                    "ext4 can grow into the free space after it, or shrink (the filesystem is shrunk first, then the partition)."
                }
                .to_string(),
            ),
            "btrfs" => (
                free_after > 0 && mounted && !part.locked,
                false,
                "btrfs grows online while mounted. Shrinking btrfs from here isn't offered — it's the riskiest resize there is."
                    .to_string(),
            ),
            "xfs" => (
                free_after > 0 && mounted && !part.locked,
                false,
                "XFS can only grow, and only while mounted.".to_string(),
            ),
            _ => (
                false,
                false,
                format!("Resizing {fs} isn't supported here — formatting is the honest option for this one."),
            ),
        };
        Ok::<ResizeInfo, String>(ResizeInfo {
            size_bytes: part.size_bytes,
            free_after_bytes: free_after,
            can_grow,
            can_shrink,
            note,
        })
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("resize info failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Resize a partition (+ its filesystem) to `new_bytes`.
/// Grow: parted first, then the filesystem. Shrink (ext4,
/// unmounted only): filesystem first, then parted.
#[tauri::command]
pub async fn resize_partition(device: String, new_bytes: u64) -> Cmd<()> {
    tauri::async_runtime::spawn_blocking(move || {
        let inv = disk_inventory()?;
        let mut found: Option<(DiskPart, String, u32)> = None;
        for disk in &inv {
            for part in &disk.partitions {
                if part.path == device {
                    let num: u32 = device
                        .trim_start_matches(&disk.path)
                        .trim_start_matches('p')
                        .chars()
                        .filter(|c| c.is_ascii_digit())
                        .collect::<String>()
                        .parse()
                        .unwrap_or(0);
                    found = Some((part.clone(), disk.path.clone(), num));
                }
            }
        }
        let (part, disk_path, part_num) = found.ok_or("unknown partition")?;
        if part.locked {
            return Err(part
                .lock_reason
                .clone()
                .unwrap_or_else(|| "partition is protected".into()));
        }
        if part_num == 0 {
            return Err("could not determine the partition number".into());
        }
        let fs = part.fstype.clone().unwrap_or_default();
        let mounted = !part.mountpoints.is_empty();
        let mp = part.mountpoints.first().cloned().unwrap_or_default();
        let growing = new_bytes > part.size_bytes;
        let script = if growing {
            match fs.as_str() {
                "ext4" => format!(
                    "yes | parted -s '{disk}' resizepart {num} {new}B && resize2fs '{dev}'",
                    disk = disk_path, num = part_num, new = new_bytes, dev = device
                ),
                "btrfs" => {
                    if !mounted {
                        return Err("mount the btrfs filesystem first — it resizes online".into());
                    }
                    format!(
                        "yes | parted -s '{disk}' resizepart {num} {new}B && btrfs filesystem resize {new} '{mp}'",
                        disk = disk_path, num = part_num, new = new_bytes, mp = mp
                    )
                }
                "xfs" => {
                    if !mounted {
                        return Err("mount the XFS filesystem first — it grows online".into());
                    }
                    format!(
                        "yes | parted -s '{disk}' resizepart {num} {new}B && xfs_growfs '{mp}'",
                        disk = disk_path, num = part_num, new = new_bytes, mp = mp
                    )
                }
                _ => return Err(format!("growing {fs} isn't supported")),
            }
        } else {
            // shrink
            if fs != "ext4" {
                return Err(format!("shrinking {fs} isn't supported here"));
            }
            if mounted {
                return Err("unmount the partition first — ext4 can't shrink while mounted".into());
            }
            format!(
                "resize2fs '{dev}' {new}B && yes | parted -s '{disk}' resizepart {num} {new}B",
                dev = device, new = new_bytes, disk = disk_path, num = part_num
            )
        };
        // parted asks before shrinking a partition; the `yes |`
        // prefixes inside the script answer just that prompt.
        pkexec_sh(&script)?;
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("resize task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

// ---------------------------------------------------------------------------
// New files (right-click → New) + folders
// ---------------------------------------------------------------------------

/// Create a folder in a directory item ("Untitled Folder", numbered
/// on collision). Returns the new folder's path.
#[tauri::command]
pub async fn create_folder(state: State<'_, AppState>, dir: ItemId) -> Cmd<String> {
    let dir_path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&dir)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown dir {dir}"))))?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut dest = dir_path.join("Untitled Folder");
        let mut n = 2;
        while dest.exists() {
            dest = dir_path.join(format!("Untitled Folder {n}"));
            n += 1;
        }
        std::fs::create_dir(&dest).map_err(|e| format!("mkdir: {e}"))?;
        Ok::<String, String>(dest.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("mkdir task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Where generated office templates are cached (per-user data dir).
fn template_dir() -> std::result::Result<std::path::PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "no HOME".to_string())?;
    let dir = std::path::PathBuf::from(home)
        .join(".local/share/fable-files/templates");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Ensure a blank office template of `kind` exists in the cache,
/// generated with LibreOffice from an empty seed file.
fn office_template(kind: &str) -> std::result::Result<std::path::PathBuf, String> {
    crate::deps::ensure_tool("soffice", "soffice")?;
    let dir = template_dir()?;
    let ext = kind;
    let template = dir.join(format!("blank.{ext}"));
    if template.exists()
        && std::fs::metadata(&template).map(|m| m.len() > 500).unwrap_or(false)
    {
        return Ok(template);
    }
    let staging = dir.join(format!("stage-{kind}"));
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    // Seed: documents/spreadsheets come from a text/csv seed;
    // presentations/impress formats from an empty text seed.
    let (seed_name, seed_content) = match kind {
        "xlsx" | "ods" => ("seed.csv", ""),
        _ => ("seed.txt", ""),
    };
    let seed = staging.join(seed_name);
    std::fs::write(&seed, seed_content).map_err(|e| e.to_string())?;
    let out = std::process::Command::new("soffice")
        .args(["--headless", "--norestore", "--convert-to", kind, "--outdir"])
        .arg(&staging)
        .arg(&seed)
        .output()
        .map_err(|e| format!("soffice: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "LibreOffice could not create a blank .{kind}: {}",
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .last()
                .unwrap_or("")
                .trim()
        ));
    }
    let produced = staging.join(format!("seed.{kind}"));
    if !produced.exists() {
        return Err(format!("LibreOffice did not produce a blank .{kind}"));
    }
    std::fs::rename(&produced, &template).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(&staging);
    Ok(template)
}

/// Create a new blank file of `kind` in a directory. Text kinds get
/// honest starter content; office kinds come from real LibreOffice
/// templates. Returns the new file's path.
#[tauri::command]
pub async fn create_new_file(
    state: State<'_, AppState>,
    dir: ItemId,
    kind: String,
) -> Cmd<String> {
    let dir_path = state
        .item_paths
        .lock()
        .unwrap()
        .get(&dir)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown dir {dir}"))))?;
    tauri::async_runtime::spawn_blocking(move || {
        let (base_name, text_content): (&str, Option<&str>) = match kind.as_str() {
            "txt" => ("Untitled.txt", Some("")),
            "md" => ("Untitled.md", Some("# Untitled\n\n")),
            "csv" => ("Untitled.csv", Some("")),
            "json" => ("Untitled.json", Some("{}\n")),
            "html" => (
                "Untitled.html",
                Some("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>Untitled</title>\n</head>\n<body>\n\n</body>\n</html>\n"),
            ),
            "sh" => ("Untitled.sh", Some("#!/bin/bash\n")),
            "py" => ("Untitled.py", Some("#!/usr/bin/env python3\n\n")),
            "docx" | "xlsx" | "pptx" | "odt" | "ods" | "odp" => {
                let template = office_template(&kind)?;
                let file_name = format!("Untitled.{kind}");
                let mut dest = dir_path.join(&file_name);
                let mut n = 2;
                while dest.exists() {
                    dest = dir_path.join(format!("Untitled {n}.{kind}"));
                    n += 1;
                }
                std::fs::copy(&template, &dest).map_err(|e| format!("copy: {e}"))?;
                return Ok::<String, String>(dest.to_string_lossy().to_string());
            }
            other => return Err(format!("unknown file kind {other}")),
        };
        let mut dest = dir_path.join(base_name);
        let mut n = 2;
        while dest.exists() {
            let stem = base_name.rsplit_once('.').map(|(s, _)| s).unwrap_or(base_name);
            let ext = base_name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
            dest = dir_path.join(format!("{stem} {n}.{ext}"));
            n += 1;
        }
        std::fs::write(&dest, text_content.unwrap_or("")).map_err(|e| format!("write: {e}"))?;
        if kind == "sh" || kind == "py" {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(
                &dest,
                std::fs::Permissions::from_mode(0o755),
            );
        }
        Ok::<String, String>(dest.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("create task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

// ---------------------------------------------------------------------------
// Printing (CUPS lp; office docs converted to PDF first)
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
pub struct Printer {
    pub name: String,
    pub is_default: bool,
    pub state: String,
}

#[tauri::command]
pub async fn list_printers() -> Cmd<Vec<Printer>> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::deps::ensure_tool("lpstat", "cups")?;
        let default_name = std::process::Command::new("lpstat")
            .arg("-d")
            .output()
            .ok()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .split(':')
                    .nth(1)
                    .unwrap_or("")
                    .trim()
                    .to_string()
            })
            .unwrap_or_default();
        let out = std::process::Command::new("lpstat")
            .arg("-p")
            .output()
            .map_err(|e| format!("lpstat: {e}"))?;
        let mut printers = Vec::new();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            // "printer NAME is idle.  enabled since ..."
            if let Some(rest) = line.strip_prefix("printer ") {
                let mut parts = rest.splitn(2, char::is_whitespace);
                let name = parts.next().unwrap_or("").to_string();
                let state = if line.contains("is idle") {
                    "idle"
                } else if line.contains("disabled") {
                    "disabled"
                } else if line.contains("now printing") {
                    "printing"
                } else {
                    "unknown"
                }
                .to_string();
                if !name.is_empty() {
                    printers.push(Printer {
                        is_default: name == default_name,
                        name,
                        state,
                    });
                }
            }
        }
        Ok::<Vec<Printer>, String>(printers)
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("printers task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Print a file. Office documents go through LibreOffice to a temp
/// PDF (CUPS speaks PDF natively); everything else prints as-is.
/// Returns the CUPS request id line.
#[tauri::command]
pub async fn print_item(
    state: State<'_, AppState>,
    item: ItemId,
    printer: Option<String>,
    copies: u32,
) -> Cmd<String> {
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
        let office = [
            "doc", "docx", "odt", "rtf", "xls", "xlsx", "ods", "ppt", "pptx", "odp",
        ];
        crate::deps::ensure_tool("lpstat", "cups")?;
        let printable_path;
        let _temp_dir;
        if office.contains(&ext.as_str()) {
            crate::deps::ensure_tool("soffice", "soffice")?;
            let td = std::env::temp_dir().join(format!("fable-print-{}", ItemId::new()));
            std::fs::create_dir_all(&td).map_err(|e| e.to_string())?;
            let out = std::process::Command::new("soffice")
                .args(["--headless", "--norestore", "--convert-to", "pdf", "--outdir"])
                .arg(&td)
                .arg(&path)
                .output()
                .map_err(|e| format!("soffice: {e}"))?;
            if !out.status.success() {
                return Err("LibreOffice could not convert the document for printing".into());
            }
            let stem = path
                .file_stem()
                .and_then(|n| n.to_str())
                .unwrap_or("document")
                .to_string();
            printable_path = td.join(format!("{stem}.pdf"));
            if !printable_path.exists() {
                return Err("the print conversion produced no PDF".into());
            }
            _temp_dir = td;
        } else {
            printable_path = path.clone();
        }
        let mut cmd = std::process::Command::new("lp");
        if let Some(p) = &printer {
            cmd.arg("-d").arg(p);
        }
        if copies > 1 {
            cmd.arg("-n").arg(copies.to_string());
        }
        cmd.arg(&printable_path);
        let out = cmd.output().map_err(|e| format!("lp: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "printing failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok::<String, String>(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        )
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("print task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}


// ---------------------------------------------------------------------------
// Torrent creation — thin wrappers over the ufm-torrent engine
// ---------------------------------------------------------------------------

/// Is mktorrent available? (The dialog offers a one-click install
/// when it isn't — no silent degraded modes.)
#[tauri::command]
pub async fn mktorrent_status() -> Cmd<bool> {
    Ok(ufm_torrent::mktorrent_available())
}

/// Install mktorrent through the OS package machinery (the
/// dialog's one-click offer; per-family packages via crate::deps
/// — this used to hardcode pacman, which silently broke the
/// offer on every apt/dnf/zypper machine).
#[tauri::command]
pub async fn install_mktorrent() -> Cmd<()> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::deps::ensure_tool("mktorrent", "mktorrent")
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("install task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// Create a .torrent for a file or folder (see ufm-torrent).
#[tauri::command]
pub async fn create_torrent(
    state: State<'_, AppState>,
    item: ItemId,
    wash: bool,
    clean_images: bool,
    trackers: Vec<String>,
    private_flag: bool,
) -> Cmd<ufm_torrent::TorrentOutcome> {
    let src = state
        .item_paths
        .lock()
        .unwrap()
        .get(&item)
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound(format!("unknown item {item}"))))?;
    tauri::async_runtime::spawn_blocking(move || {
        let opts = ufm_torrent::TorrentOptions {
            wash,
            clean_images,
            trackers,
            private_flag,
        };
        ufm_torrent::create_torrent(&src, &opts)
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("torrent task failed: {e}"))))?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}


// ---------------------------------------------------------------------------
// ROM media enrichment (rommedia.rs): settings + secrets + validation,
// the journaled download job, the missing-media report, ambiguity
// confirmation, and the art lookup for art-forward collections.
// ---------------------------------------------------------------------------

fn rom_media_base(state: &AppState) -> std::path::PathBuf {
    state
        .catalog_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Library Memory (Indexer v2) — remembered volumes + retention.
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_library_settings(
    state: State<'_, AppState>,
) -> Cmd<crate::library::LibrarySettings> {
    Ok(crate::library::load_settings(&rom_media_base(&state)))
}

#[tauri::command]
pub fn set_library_settings(
    state: State<'_, AppState>,
    settings: crate::library::LibrarySettings,
) -> Cmd<crate::library::LibrarySettings> {
    crate::library::save_settings(&rom_media_base(&state), &settings)
        .map_err(|e| CommandError::from(UfmError::InvalidInput(e)))?;
    // A shorter window may make volumes prunable right now — run
    // the same pass startup runs.
    prune_now(&state);
    Ok(settings)
}

/// The retention pass: record sightings, then prune expired
/// OFFLINE volumes. An empty enumeration skips pruning entirely —
/// "can't see anything" is never read as "everything is gone".
fn prune_now(state: &AppState) {
    let catalog = state.catalog.lock().unwrap();
    let mounted = record_volume_sightings(&catalog);
    if mounted.is_empty() {
        return;
    }
    let mounted_uuids: std::collections::HashSet<String> =
        mounted.iter().filter_map(|v| v.uuid.clone()).collect();
    let settings = crate::library::load_settings(&rom_media_base(state));
    let pruned = crate::library::prune_expired_volumes(
        &catalog,
        &settings,
        &mounted_uuids,
        chrono::Utc::now(),
    );
    if !pruned.is_empty() {
        let _ = catalog.vacuum();
    }
}

#[tauri::command]
pub async fn list_volumes(
    state: State<'_, AppState>,
) -> Cmd<Vec<crate::library::VolumeSummary>> {
    let settings = crate::library::load_settings(&rom_media_base(&state));
    let catalog = state.catalog.lock().unwrap();
    let mounted = record_volume_sightings(&catalog);
    let records = catalog.volumes().map_err(CommandError::from)?;
    Ok(records
        .iter()
        .map(|rec| {
            let online_mount = mounted
                .iter()
                .find(|v| v.uuid.as_deref() == Some(rec.fs_uuid.as_str()))
                .map(|v| v.path.as_str());
            crate::library::summarize(&catalog, rec, online_mount, &settings)
        })
        .collect())
}

/// Forget a drive: purge every catalog row remembered from it and
/// drop its volume record. Refused while the drive is connected
/// (its files are live data, not memory) and for the system volume.
#[tauri::command]
pub async fn forget_volume(state: State<'_, AppState>, fs_uuid: String) -> Cmd<u64> {
    let catalog = state.catalog.lock().unwrap();
    let record = catalog
        .volume(&fs_uuid)
        .map_err(CommandError::from)?
        .ok_or_else(|| {
            CommandError::from(UfmError::NotFound(format!("unknown volume {fs_uuid}")))
        })?;
    if crate::library::is_system_volume(&record) {
        return Err(CommandError::from(UfmError::InvalidInput(
            "Your main drive is always remembered — it's where your files live."
                .into(),
        )));
    }
    let mounted = current_filesystems();
    if mounted
        .iter()
        .any(|v| v.uuid.as_deref() == Some(fs_uuid.as_str()))
    {
        return Err(CommandError::from(UfmError::InvalidInput(
            "That drive is connected right now. Unplug it first — Fable never forgets a drive that's here.".into(),
        )));
    }
    let prefixes = crate::library::volume_prefixes(&record);
    let purged = catalog
        .purge_items_under_prefixes(&prefixes)
        .map_err(CommandError::from)?;
    catalog.delete_volume(&fs_uuid).map_err(CommandError::from)?;
    // Drop any retention override for the forgotten volume.
    let base = rom_media_base(&state);
    let mut settings = crate::library::load_settings(&base);
    if settings.overrides.remove(&fs_uuid).is_some() {
        let _ = crate::library::save_settings(&base, &settings);
    }
    Ok(purged)
}

/// Rebuild one volume's index from scratch: purge its remembered
/// rows and clear the resume checkpoint so the next walk is a true
/// full walk. Returns the volume's current mount point; the caller
/// starts indexing on the matching location.
#[tauri::command]
pub async fn rebuild_volume(state: State<'_, AppState>, fs_uuid: String) -> Cmd<String> {
    {
        let index = state.index_state.lock().unwrap();
        if index.running {
            return Err(CommandError::from(UfmError::InvalidInput(
                "Indexing is already running — wait for it to finish, then rebuild."
                    .into(),
            )));
        }
    }
    let mount_point = {
        let mounted = current_filesystems();
        mounted
            .iter()
            .find(|v| v.uuid.as_deref() == Some(fs_uuid.as_str()))
            .map(|v| v.path.clone())
            .ok_or_else(|| {
                CommandError::from(UfmError::NotFound(
                    "That drive isn't connected right now — connect it to rebuild its index."
                        .into(),
                ))
            })?
    };
    let catalog = state.catalog.lock().unwrap();
    let record = catalog
        .volume(&fs_uuid)
        .map_err(CommandError::from)?
        .ok_or_else(|| {
            CommandError::from(UfmError::NotFound(format!("unknown volume {fs_uuid}")))
        })?;
    let prefixes = crate::library::volume_prefixes(&record);
    catalog
        .purge_items_under_prefixes(&prefixes)
        .map_err(CommandError::from)?;
    drop(catalog);
    // The rebuild must not inherit resume state: a pruned walk over
    // a purged catalog would re-add nothing. Clearing the shared
    // checkpoint is safe — worst case, other locations lose resume
    // pruning and re-walk incrementally instead.
    let _ = std::fs::remove_file(&state.checkpoint_path);
    Ok(mount_point)
}

#[tauri::command]
pub fn get_rom_media_settings(
    state: State<'_, AppState>,
) -> Cmd<crate::rommedia::SettingsView> {
    Ok(crate::rommedia::settings_view(&rom_media_base(&state)))
}

#[tauri::command]
pub fn set_rom_media_settings(
    state: State<'_, AppState>,
    settings: crate::rommedia::RomMediaSettings,
) -> Cmd<()> {
    crate::rommedia::save_settings(&rom_media_base(&state), &settings)
        .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

#[tauri::command]
pub fn set_rom_media_secret(
    kind: String,
    value: Option<String>,
) -> Cmd<String> {
    if !crate::rommedia::SECRET_KINDS.contains(&kind.as_str()) {
        return Err(CommandError::from(UfmError::InvalidInput(format!(
            "unknown secret kind {kind}"
        ))));
    }
    crate::rommedia::secret_set(&kind, value.as_deref())
        .map_err(|e| CommandError::from(UfmError::Internal(e)))?;
    Ok(match value.as_deref() {
        Some(v) if !v.trim().is_empty() => "Saved to the system keyring.".into(),
        _ => "Removed from the system keyring.".into(),
    })
}

#[tauri::command]
pub async fn validate_rom_media_source(
    state: State<'_, AppState>,
    kind: String,
) -> Cmd<crate::rommedia::ValidateResult> {
    let base = rom_media_base(&state);
    let result = tauri::async_runtime::spawn_blocking(move || {
        crate::rommedia::validate_source(&base, &kind)
    })
    .await
    .map_err(|e| {
        CommandError::from(UfmError::Internal(format!("validation task failed: {e}")))
    })?;
    Ok(result)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RomMediaRetry {
    pub item_id: String,
    pub media_type: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RomMediaDownloadRequest {
    /// Dataset system keys; None = every system with identified ROMs.
    #[serde(default)]
    pub systems: Option<Vec<String>>,
    /// Display label for a single-system job title.
    #[serde(default)]
    pub system_label: Option<String>,
    /// Explicit (item, type) retries from the media report.
    #[serde(default)]
    pub retry: Vec<RomMediaRetry>,
}

#[tauri::command]
pub async fn start_rom_media_download(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    req: RomMediaDownloadRequest,
) -> Cmd<JobStart> {
    let base = rom_media_base(&state);
    let catalog_path = state.catalog_path.clone();
    let retry: Vec<(String, String)> = req
        .retry
        .iter()
        .map(|r| (r.item_id.clone(), r.media_type.clone()))
        .collect();
    let systems = req.systems.clone();
    let label = req.system_label.clone();
    let job = tauri::async_runtime::spawn_blocking(move || {
        let settings = crate::rommedia::load_settings(&base);
        let catalog = ufm_catalog::Catalog::open(&catalog_path)
            .map_err(|e| format!("catalog open failed: {e}"))?;
        crate::rommedia::plan_download(
            &catalog,
            &settings,
            systems,
            &retry,
            label.as_deref(),
        )
    })
    .await
    .map_err(|e| {
        CommandError::from(UfmError::Internal(format!("planning task failed: {e}")))
    })?
    .map_err(|e| CommandError::from(UfmError::InvalidInput(e)))?;
    start_job(app, &state, job)
}

#[tauri::command]
pub async fn rom_media_report(
    state: State<'_, AppState>,
    system: Option<String>,
) -> Cmd<crate::rommedia::ReportData> {
    let base = rom_media_base(&state);
    let catalog_path = state.catalog_path.clone();
    let report = tauri::async_runtime::spawn_blocking(move || {
        let settings = crate::rommedia::load_settings(&base);
        let catalog = ufm_catalog::Catalog::open(&catalog_path)
            .map_err(|e| format!("catalog open failed: {e}"))?;
        Ok::<_, String>(crate::rommedia::build_report(
            &catalog,
            &settings,
            system.as_deref(),
        ))
    })
    .await
    .map_err(|e| {
        CommandError::from(UfmError::Internal(format!("report task failed: {e}")))
    })?
    .map_err(|e| CommandError::from(UfmError::Internal(e)))?;
    Ok(report)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RomMediaConfirmRequest {
    pub item_id: String,
    pub media_type: String,
    pub source: String,
    pub url: String,
}

#[tauri::command]
pub async fn confirm_rom_media_match(
    state: State<'_, AppState>,
    req: RomMediaConfirmRequest,
) -> Cmd<String> {
    let base = rom_media_base(&state);
    let catalog_path = state.catalog_path.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let catalog = ufm_catalog::Catalog::open(&catalog_path)
            .map_err(|e| format!("catalog open failed: {e}"))?;
        crate::rommedia::confirm_match(
            &catalog,
            &base,
            &req.item_id,
            &req.media_type,
            &req.source,
            &req.url,
        )
    })
    .await
    .map_err(|e| {
        CommandError::from(UfmError::Internal(format!("confirm task failed: {e}")))
    })?
    .map_err(|e| CommandError::from(UfmError::InvalidInput(e)))?;
    Ok(result)
}

#[tauri::command]
pub fn rom_media_art(
    state: State<'_, AppState>,
    item_ids: Vec<String>,
) -> Cmd<std::collections::HashMap<String, String>> {
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    catalog
        .rom_media_art(&item_ids)
        .map_err(CommandError::from)
}

// ---------------------------------------------------------------------------
// Media Resolution (mediares.rs): TMDB key management (keyring only),
// the folder resolution scan, confirm/correct, and the journaled
// Organize job. The plan function IS the preview: start_organize
// replans deterministically, so what the user saw is what runs.
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn mediares_get_settings() -> Cmd<crate::mediares::SettingsView> {
    Ok(crate::mediares::settings_view())
}

#[tauri::command]
pub fn mediares_set_key(value: Option<String>) -> Cmd<String> {
    crate::mediares::tmdb_key_set(value.as_deref())
        .map_err(|e| CommandError::from(UfmError::Internal(e)))?;
    Ok(match value.as_deref() {
        Some(v) if !v.trim().is_empty() => "Saved to the system keyring.".into(),
        _ => "Removed from the system keyring.".into(),
    })
}

#[tauri::command]
pub async fn mediares_validate_key(
    key: Option<String>,
) -> Cmd<crate::mediares::ValidateResult> {
    // UX rider (2026-10-15): a non-empty field value wins — paste →
    // Check works without Save first. Empty field = check the saved
    // key, as before.
    let key = match key {
        Some(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => crate::mediares::tmdb_key_get().ok_or_else(|| {
            CommandError::from(UfmError::InvalidInput(
                "no TMDB key saved yet".into(),
            ))
        })?,
    };
    let result = tauri::async_runtime::spawn_blocking(move || {
        crate::mediares::TmdbClient::new(key).validate()
    })
    .await
    .map_err(|e| {
        CommandError::from(UfmError::Internal(format!("validation task failed: {e}")))
    })?;
    Ok(result)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaresScanRequest {
    pub root: String,
    pub paths: Vec<String>,
}

#[tauri::command]
pub async fn mediares_scan(
    state: State<'_, AppState>,
    req: MediaresScanRequest,
) -> Cmd<crate::mediares::ScanResult> {
    let catalog_path = state.catalog_path.clone();
    let key = crate::mediares::tmdb_key_get();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let catalog = ufm_catalog::Catalog::open(&catalog_path)
            .map_err(|e| format!("catalog open failed: {e}"))?;
        crate::mediares::scan(&catalog, Path::new(&req.root), &req.paths, key)
    })
    .await
    .map_err(|e| {
        CommandError::from(UfmError::Internal(format!("scan task failed: {e}")))
    })?
    .map_err(|e| CommandError::from(UfmError::InvalidInput(e)))?;
    Ok(result)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaresConfirmRequest {
    pub path: String,
    pub choice: crate::mediares::ConfirmChoice,
}

#[tauri::command]
pub async fn mediares_confirm(
    state: State<'_, AppState>,
    req: MediaresConfirmRequest,
) -> Cmd<crate::mediares::IdentityView> {
    let catalog_path = state.catalog_path.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let catalog = ufm_catalog::Catalog::open(&catalog_path)
            .map_err(|e| format!("catalog open failed: {e}"))?;
        crate::mediares::confirm(&catalog, Path::new(&req.path), &req.choice)
    })
    .await
    .map_err(|e| {
        CommandError::from(UfmError::Internal(format!("confirm task failed: {e}")))
    })?
    .map_err(|e| CommandError::from(UfmError::InvalidInput(e)))?;
    Ok(result)
}

#[tauri::command]
pub async fn mediares_plan_organize(
    state: State<'_, AppState>,
    req: MediaresScanRequest,
) -> Cmd<crate::mediares::OrganizePlan> {
    let catalog_path = state.catalog_path.clone();
    let plan = tauri::async_runtime::spawn_blocking(move || {
        let catalog = ufm_catalog::Catalog::open(&catalog_path)
            .map_err(|e| format!("catalog open failed: {e}"))?;
        crate::mediares::plan_organize(&catalog, Path::new(&req.root), &req.paths)
    })
    .await
    .map_err(|e| {
        CommandError::from(UfmError::Internal(format!("plan task failed: {e}")))
    })?
    .map_err(|e| CommandError::from(UfmError::InvalidInput(e)))?;
    Ok(plan)
}

/// Build the Organize job from a plan: one Mkdir chain per target
/// folder + one Move per file, every Move an outcome step (the undo
/// feed). Cross-device files decompose via the planner's verified
/// move, exactly like a hand-planned move job.
fn build_organize_job(
    state: &AppState,
    root: &Path,
    plan: &crate::mediares::OrganizePlan,
) -> Cmd<ufm_jobs::Job> {
    let location = location_for_path(state, root);
    let mut planner = PathPlanner {
        job: ufm_jobs::Job::new(
            format!(
                "Organize {} file{} for media servers",
                plan.actions.len(),
                if plan.actions.len() == 1 { "" } else { "s" }
            ),
            ufm_jobs::JobPreview::default(),
        ),
        seq: 0,
        location,
    };
    // abs dir -> step id that creates it (None = already on disk)
    let mut dir_steps: std::collections::HashMap<PathBuf, Option<String>> =
        std::collections::HashMap::new();
    dir_steps.insert(root.to_path_buf(), None);
    // Media Completion: (destination path, the step whose completion
    // puts the file there, identity) — fetch steps hang off these.
    let mut fetch_anchors: Vec<(
        String,
        String,
        crate::mediares::IdentityView,
    )> = Vec::new();
    for action in &plan.actions {
        let src = PathBuf::from(&action.src);
        let dst = PathBuf::from(&action.dst);
        let dst_parent = dst
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| root.to_path_buf());
        // Ensure the destination chain exists as steps.
        let mut dep: Option<String> = None;
        if let Ok(rel_parent) = dst_parent.strip_prefix(root) {
            let mut cur = root.to_path_buf();
            for comp in rel_parent.components() {
                cur = cur.join(comp.as_os_str());
                if let Some(existing) = dir_steps.get(&cur) {
                    if existing.is_some() {
                        dep = existing.clone();
                    }
                    continue;
                }
                if cur.is_dir() {
                    dir_steps.insert(cur.clone(), None);
                } else {
                    let parent_dep = dep.clone();
                    let id = planner.add_mkdir(
                        cur.parent().unwrap_or(root),
                        &comp.as_os_str().to_string_lossy(),
                        parent_dep,
                    );
                    dir_steps.insert(cur.clone(), Some(id.clone()));
                    dep = Some(id);
                }
            }
        }
        let new_name = dst
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let size = std::fs::metadata(&src).map(|m| m.len()).unwrap_or(0);
        planner.job.preview.files_affected += 1;
        planner.job.preview.bytes_to_move += size;
        if same_device(&src, &dst_parent) {
            let move_step = planner.add_step(
                format!("organize {}", src.display()),
                ufm_jobs::StepOp::Move {
                    src: job_item(
                        ItemId::new(),
                        location,
                        &src,
                        ItemKind::File,
                        Some(size),
                    ),
                    dst_parent: job_item(
                        ItemId::new(),
                        location,
                        &dst_parent,
                        ItemKind::Folder,
                        None,
                    ),
                    new_name,
                },
                dep.into_iter().collect(),
                true,
                None,
            );
            fetch_anchors.push((
                action.dst.clone(),
                move_step,
                action.identity.clone(),
            ));
        } else {
            let (_, _, del) =
                planner.add_move_file(&src, &dst_parent, &new_name, size, dep);
            if let Some(s) = planner.job.steps.get_mut(&del) {
                s.outcome = true;
                s.outcome_path = Some(action.dst.clone());
            }
            fetch_anchors.push((
                action.dst.clone(),
                del,
                action.identity.clone(),
            ));
        }
    }
    // Media Completion (Dickles, 2026-10-15): when the settings say
    // so, every organized file also gets its poster (and subtitle)
    // fetched AT its new home — each fetch step depends on the step
    // that lands the file there, and is a NON-outcome step (the
    // move is the undoable act; placed sidecars ride the job's
    // placed-file record via their Done notes).
    {
        let base = state
            .catalog_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default();
        let mc = crate::mediacomplete::load_settings(&base);
        let want_subs = mc.subtitles_on_organize
            && crate::opensubs::creds_state().has_api_key;
        if mc.artwork_on_organize || want_subs {
            for (dst, anchor, idn) in &fetch_anchors {
                if mc.artwork_on_organize {
                    if let Some(tid) = idn.tmdb_id {
                        planner.add_step(
                            format!("fetch artwork for {}", dst),
                            ufm_jobs::StepOp::FetchArtwork {
                                video_path: dst.clone(),
                                media_type: idn.media_type.clone(),
                                tmdb_id: tid,
                                season: idn.season.map(|s| s as u32),
                                fanart: mc.fanart,
                            },
                            vec![anchor.clone()],
                            false,
                            None,
                        );
                    }
                }
                if want_subs {
                    planner.add_step(
                        format!("fetch subtitle for {}", dst),
                        ufm_jobs::StepOp::FetchSubtitle {
                            video_path: dst.clone(),
                            language: mc.subtitle_language.clone(),
                            media_type: idn.media_type.clone(),
                            tmdb_id: idn.tmdb_id,
                            season: idn.season.map(|s| s as u32),
                            episode: idn.episode.map(|e| e as u32),
                        },
                        vec![anchor.clone()],
                        false,
                        None,
                    );
                }
            }
        }
    }
    Ok(planner.job)
}

#[tauri::command]
pub async fn mediares_start_organize(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    req: MediaresScanRequest,
) -> Cmd<JobStart> {
    let catalog_path = state.catalog_path.clone();
    let root = PathBuf::from(&req.root);
    let root_for_plan = root.clone();
    let plan = tauri::async_runtime::spawn_blocking(move || {
        let catalog = ufm_catalog::Catalog::open(&catalog_path)
            .map_err(|e| format!("catalog open failed: {e}"))?;
        crate::mediares::plan_organize(&catalog, &root_for_plan, &req.paths)
    })
    .await
    .map_err(|e| {
        CommandError::from(UfmError::Internal(format!("plan task failed: {e}")))
    })?
    .map_err(|e| CommandError::from(UfmError::InvalidInput(e)))?;
    if plan.actions.is_empty() {
        return Err(CommandError::from(UfmError::InvalidInput(
            "nothing to organize — every file was excluded (see the preview)".into(),
        )));
    }
    let job = build_organize_job(&state, &root, &plan)?;
    start_job(app, &state, job)
}

// ---------------------------------------------------------------------------
// Media Completion commands (Dickles, 2026-10-15): artwork and
// subtitle settings + fetches, the OpenSubtitles account block, and
// the background scheduler (agent control, runs, undo, pending).
// ---------------------------------------------------------------------------

fn mc_base(state: &AppState) -> PathBuf {
    state
        .catalog_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default()
}

#[tauri::command]
pub fn mediacomplete_get_settings(
    state: State<'_, AppState>,
) -> Cmd<crate::mediacomplete::McSettings> {
    Ok(crate::mediacomplete::load_settings(&mc_base(&state)))
}

#[tauri::command]
pub fn mediacomplete_save_settings(
    state: State<'_, AppState>,
    settings: crate::mediacomplete::McSettings,
) -> Cmd<crate::mediacomplete::McSettings> {
    let base = mc_base(&state);
    crate::mediacomplete::save_settings(&base, &settings)
        .map_err(|e| CommandError::from(UfmError::InvalidInput(e)))?;
    Ok(crate::mediacomplete::load_settings(&base))
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McStatusView {
    pub agent_running: bool,
    pub agent_pid: Option<u32>,
    pub agent_last_tick: Option<String>,
    /// "systemd" | "autostart" | "none"
    pub agent_install: String,
    pub next_runs: Vec<(String, String)>,
    pub opensubs: crate::opensubs::CredsState,
}

#[tauri::command]
pub fn mediacomplete_get_status(
    state: State<'_, AppState>,
) -> Cmd<McStatusView> {
    let base = mc_base(&state);
    let st = crate::mediacomplete::agent_status(&base);
    Ok(McStatusView {
        agent_running: st.running,
        agent_pid: st.pid,
        agent_last_tick: st.last_tick,
        agent_install: crate::mediacomplete::agent_install_kind().to_string(),
        next_runs: st.next_runs,
        opensubs: crate::opensubs::creds_state(),
    })
}

#[tauri::command]
pub fn mediacomplete_set_agent(
    state: State<'_, AppState>,
    enabled: bool,
) -> Cmd<String> {
    let base = mc_base(&state);
    let msg = crate::mediacomplete::set_agent_installed(&appimage_path(), enabled)
        .map_err(|e| CommandError::from(UfmError::InvalidInput(e)))?;
    let mut settings = crate::mediacomplete::load_settings(&base);
    settings.agent_enabled = enabled;
    crate::mediacomplete::save_settings(&base, &settings)
        .map_err(|e| CommandError::from(UfmError::InvalidInput(e)))?;
    Ok(msg)
}

#[tauri::command]
pub fn opensubs_set_secret(
    kind: String,
    value: Option<String>,
) -> Cmd<crate::opensubs::CredsState> {
    if !matches!(
        kind.as_str(),
        "opensubs-apikey" | "opensubs-username" | "opensubs-password"
    ) {
        return Err(CommandError::from(UfmError::InvalidInput(format!(
            "unknown secret kind: {kind}"
        ))));
    }
    crate::opensubs::secret_set(&kind, value.as_deref())
        .map_err(|e| CommandError::from(UfmError::InvalidInput(e)))?;
    Ok(crate::opensubs::creds_state())
}

#[tauri::command]
pub async fn opensubs_validate() -> Cmd<crate::opensubs::AccountView> {
    let view = tauri::async_runtime::spawn_blocking(|| {
        match crate::opensubs::OpenSubsClient::from_keyring() {
            Ok(c) => c.validate(),
            Err(e) => crate::opensubs::AccountView {
                ok: false,
                message: e,
                account: None,
                logged_in: false,
            },
        }
    })
    .await
    .map_err(|e| {
        CommandError::from(UfmError::Internal(format!("validate task failed: {e}")))
    })?;
    Ok(view)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McFetchView {
    /// done | skipped | failed | quota
    pub outcome: String,
    pub message: String,
    pub placed: Vec<String>,
    pub quota: Option<crate::opensubs::QuotaState>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McArtworkRequest {
    pub path: String,
    pub media_type: String,
    pub tmdb_id: i64,
    pub season: Option<u32>,
}

#[tauri::command]
pub async fn mediacomplete_fetch_artwork_now(
    state: State<'_, AppState>,
    req: McArtworkRequest,
) -> Cmd<McFetchView> {
    let base = mc_base(&state);
    let view = tauri::async_runtime::spawn_blocking(move || {
        let settings = crate::mediacomplete::load_settings(&base);
        match crate::mediacomplete::fetch_artwork(
            std::path::Path::new(&req.path),
            &req.media_type,
            req.tmdb_id,
            req.season,
            settings.fanart,
            crate::mediares::tmdb_key_get(),
            &|| false,
        ) {
            crate::mediacomplete::FetchVerdict::Done(placed) => McFetchView {
                outcome: "done".into(),
                message: format!("Placed {} artwork file(s).", placed.len()),
                placed,
                quota: None,
            },
            crate::mediacomplete::FetchVerdict::Skipped(m) => McFetchView {
                outcome: "skipped".into(),
                message: m,
                placed: vec![],
                quota: None,
            },
            crate::mediacomplete::FetchVerdict::Failed(m) => McFetchView {
                outcome: "failed".into(),
                message: m,
                placed: vec![],
                quota: None,
            },
        }
    })
    .await
    .map_err(|e| {
        CommandError::from(UfmError::Internal(format!("artwork task failed: {e}")))
    })?;
    Ok(view)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McSubtitleRequest {
    pub path: String,
    pub media_type: String,
    pub tmdb_id: Option<i64>,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub language: Option<String>,
}

#[tauri::command]
pub async fn mediacomplete_fetch_subtitle_now(
    state: State<'_, AppState>,
    req: McSubtitleRequest,
) -> Cmd<McFetchView> {
    let base = mc_base(&state);
    let view = tauri::async_runtime::spawn_blocking(move || {
        let settings = crate::mediacomplete::load_settings(&base);
        let lang = req
            .language
            .clone()
            .unwrap_or_else(|| settings.subtitle_language.clone());
        match crate::mediacomplete::fetch_subtitle(
            std::path::Path::new(&req.path),
            &lang,
            &req.media_type,
            req.tmdb_id,
            req.season,
            req.episode,
        ) {
            crate::mediacomplete::SubVerdict::Done { path, quota } => {
                let remaining = quota
                    .remaining
                    .map(|r| format!(" OpenSubtitles reports {r} download(s) left in the current window."))
                    .unwrap_or_default();
                McFetchView {
                    outcome: "done".into(),
                    message: format!("Subtitle saved beside the video.{remaining}"),
                    placed: vec![path],
                    quota: Some(quota),
                }
            }
            crate::mediacomplete::SubVerdict::Skipped(m) => McFetchView {
                outcome: "skipped".into(),
                message: m,
                placed: vec![],
                quota: None,
            },
            crate::mediacomplete::SubVerdict::QuotaReached(q) => McFetchView {
                outcome: "quota".into(),
                message: q
                    .message
                    .clone()
                    .unwrap_or_else(|| "OpenSubtitles download allowance reached".into()),
                placed: vec![],
                quota: Some(q),
            },
            crate::mediacomplete::SubVerdict::Failed(m) => McFetchView {
                outcome: "failed".into(),
                message: m,
                placed: vec![],
                quota: None,
            },
        }
    })
    .await
    .map_err(|e| {
        CommandError::from(UfmError::Internal(format!("subtitle task failed: {e}")))
    })?;
    Ok(view)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McRunView {
    pub id: String,
    pub schedule_name: String,
    pub target: String,
    pub started: String,
    pub finished: Option<String>,
    pub was_late: bool,
    pub status: String,
    pub summary: serde_json::Value,
    pub pending: serde_json::Value,
    pub quota: Option<serde_json::Value>,
    pub can_undo: bool,
}

fn run_view(row: &ufm_catalog::MediaCompleteRunRow) -> McRunView {
    McRunView {
        id: row.id.clone(),
        schedule_name: row.schedule_name.clone(),
        target: row.target.clone(),
        started: row.started.clone(),
        finished: row.finished.clone(),
        was_late: row.was_late,
        status: row.status.clone(),
        summary: serde_json::from_str(&row.summary_json)
            .unwrap_or(serde_json::json!({})),
        pending: serde_json::from_str(&row.pending_json)
            .unwrap_or(serde_json::json!([])),
        quota: row
            .quota_json
            .as_deref()
            .and_then(|q| serde_json::from_str(q).ok()),
        can_undo: row.undone_at.is_none() && row.status != "running",
    }
}

#[tauri::command]
pub fn mediacomplete_list_runs(
    state: State<'_, AppState>,
) -> Cmd<Vec<McRunView>> {
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(|e| CommandError::from(UfmError::Internal(e.to_string())))?;
    let rows = catalog
        .recent_mediacomplete_runs(20)
        .map_err(|e| CommandError::from(UfmError::Internal(e.to_string())))?;
    Ok(rows.iter().map(run_view).collect())
}

#[tauri::command]
pub async fn mediacomplete_run_now(
    state: State<'_, AppState>,
    schedule_id: String,
) -> Cmd<McRunView> {
    let base = mc_base(&state);
    let settings = crate::mediacomplete::load_settings(&base);
    let Some(sched) = settings
        .schedules
        .iter()
        .find(|s| s.id == schedule_id)
        .cloned()
    else {
        return Err(CommandError::from(UfmError::InvalidInput(
            "no such schedule".into(),
        )));
    };
    let row = tauri::async_runtime::spawn_blocking(move || {
        crate::mediacomplete::run_completion(&sched, false)
    })
    .await
    .map_err(|e| {
        CommandError::from(UfmError::Internal(format!("run task failed: {e}")))
    })?
    .map_err(|e| CommandError::from(UfmError::InvalidInput(e)))?;
    // Stamp the schedule's bookkeeping so the agent doesn't fire
    // the same slot again tonight.
    let mut settings = crate::mediacomplete::load_settings(&base);
    if let Some(s) = settings.schedules.iter_mut().find(|s| s.id == schedule_id) {
        s.last_started = Some(
            chrono::Local::now()
                .naive_local()
                .format("%Y-%m-%dT%H:%M:%S")
                .to_string(),
        );
        s.last_status = Some(row.status.clone());
        let _ = crate::mediacomplete::save_settings(&base, &settings);
    }
    Ok(run_view(&row))
}

#[tauri::command]
pub async fn mediacomplete_undo_run(run_id: String) -> Cmd<String> {
    let msg = tauri::async_runtime::spawn_blocking(move || {
        crate::mediacomplete::undo_run(&run_id)
    })
    .await
    .map_err(|e| {
        CommandError::from(UfmError::Internal(format!("undo task failed: {e}")))
    })?
    .map_err(|e| CommandError::from(UfmError::InvalidInput(e)))?;
    Ok(msg)
}

/// Confirm one pending item from a run's morning checklist: applies
/// the mediares confirm (the identity becomes confirmed truth) and
/// removes the entry from the run's pending list. The file itself
/// completes on the NEXT run (or a manual Organize) — said plainly
/// in the UI copy.
#[tauri::command]
pub async fn mediacomplete_confirm_pending(
    state: State<'_, AppState>,
    run_id: String,
    path: String,
    choice: crate::mediares::ConfirmChoice,
) -> Cmd<crate::mediares::IdentityView> {
    let catalog_path = state.catalog_path.clone();
    let out = tauri::async_runtime::spawn_blocking(move || {
        let catalog = ufm_catalog::Catalog::open(&catalog_path)
            .map_err(|e| format!("catalog open failed: {e}"))?;
        let outcome =
            crate::mediares::confirm(&catalog, std::path::Path::new(&path), &choice)?;
        if let Some(mut row) = catalog
            .mediacomplete_run(&run_id)
            .map_err(|e| e.to_string())?
        {
            let mut pending: Vec<serde_json::Value> =
                serde_json::from_str(&row.pending_json).unwrap_or_default();
            pending.retain(|p| {
                p.get("path").and_then(|x| x.as_str()) != Some(path.as_str())
            });
            row.pending_json = serde_json::Value::Array(pending).to_string();
            let _ = catalog.upsert_mediacomplete_run(&row);
        }
        Ok::<_, String>(outcome)
    })
    .await
    .map_err(|e| {
        CommandError::from(UfmError::Internal(format!("confirm task failed: {e}")))
    })?
    .map_err(|e| CommandError::from(UfmError::InvalidInput(e)))?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// Music Completion commands (Dickles, 2026-10-15): tag enrichment
// proposals + journaled apply, the Lyrics Manager, schedules that
// ride the mediacomplete agent, runs + undo + the morning
// checklist. The music sibling of the block above.
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn musiccomplete_get_settings(
    state: State<'_, AppState>,
) -> Cmd<crate::musiccomplete::MuSettings> {
    Ok(crate::musiccomplete::load_settings(&mc_base(&state)))
}

#[tauri::command]
pub fn musiccomplete_save_settings(
    state: State<'_, AppState>,
    settings: crate::musiccomplete::MuSettings,
) -> Cmd<crate::musiccomplete::MuSettings> {
    let base = mc_base(&state);
    crate::musiccomplete::save_settings(&base, &settings)
        .map_err(|e| CommandError::from(UfmError::InvalidInput(e)))?;
    Ok(crate::musiccomplete::load_settings(&base))
}

#[tauri::command]
pub fn musiccomplete_get_status() -> Cmd<crate::musiccomplete::MuStatus> {
    Ok(crate::musiccomplete::status())
}

/// Install fpcalc for AcoustID fingerprinting (dependency law:
/// offered in context by the completion dialog and Preferences;
/// the polkit prompt is the system's, then the caller re-scans).
#[tauri::command]
pub async fn musiccomplete_install_fpcalc() -> Cmd<()> {
    tauri::async_runtime::spawn_blocking(crate::musiccomplete::install_fpcalc)
        .await
        .map_err(|e| {
            CommandError::from(UfmError::Internal(format!("install task failed: {e}")))
        })?
        .map_err(|e| CommandError::from(UfmError::Internal(e)))?;
    Ok(())
}

#[tauri::command]
pub fn musiccomplete_set_acoustid_key(
    value: Option<String>,
) -> Cmd<crate::musiccomplete::MuStatus> {
    crate::musiccomplete::acoustid_key_set(value.as_deref())
        .map_err(|e| CommandError::from(UfmError::InvalidInput(e)))?;
    Ok(crate::musiccomplete::status())
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MuScanRequest {
    pub root: String,
    pub paths: Vec<String>,
}

#[tauri::command]
pub async fn musiccomplete_scan(
    state: State<'_, AppState>,
    req: MuScanRequest,
) -> Cmd<crate::musiccomplete::ScanView> {
    let base = mc_base(&state);
    let view = tauri::async_runtime::spawn_blocking(move || {
        let settings = crate::musiccomplete::load_settings(&base);
        crate::musiccomplete::propose_paths(
            std::path::Path::new(&req.root),
            &req.paths,
            &settings,
        )
    })
    .await
    .map_err(|e| {
        CommandError::from(UfmError::Internal(format!("scan task failed: {e}")))
    })?
    .map_err(|e| CommandError::from(UfmError::InvalidInput(e)))?;
    Ok(view)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MuApplyRequest {
    pub target: String,
    pub items: Vec<crate::musiccomplete::ApplyItem>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MuRunView {
    pub id: String,
    pub schedule_name: String,
    pub target: String,
    pub started: String,
    pub finished: Option<String>,
    pub was_late: bool,
    pub status: String,
    pub summary: serde_json::Value,
    pub pending: serde_json::Value,
    pub can_undo: bool,
}

fn mu_run_view(row: &ufm_catalog::MusicCompleteRunRow) -> MuRunView {
    MuRunView {
        id: row.id.clone(),
        schedule_name: row.schedule_name.clone(),
        target: row.target.clone(),
        started: row.started.clone(),
        finished: row.finished.clone(),
        was_late: row.was_late,
        status: row.status.clone(),
        summary: serde_json::from_str(&row.summary_json)
            .unwrap_or(serde_json::json!({})),
        pending: serde_json::from_str(&row.pending_json)
            .unwrap_or(serde_json::json!([])),
        can_undo: row.undone_at.is_none() && row.status != "running",
    }
}

#[tauri::command]
pub async fn musiccomplete_apply(
    state: State<'_, AppState>,
    req: MuApplyRequest,
) -> Cmd<MuRunView> {
    let base = mc_base(&state);
    let row = tauri::async_runtime::spawn_blocking(move || {
        let settings = crate::musiccomplete::load_settings(&base);
        crate::musiccomplete::apply_review(&req.target, &req.items, &settings)
    })
    .await
    .map_err(|e| {
        CommandError::from(UfmError::Internal(format!("apply task failed: {e}")))
    })?
    .map_err(|e| CommandError::from(UfmError::InvalidInput(e)))?;
    Ok(mu_run_view(&row))
}

#[tauri::command]
pub async fn musiccomplete_lyrics_get(
    path: String,
    fetch: bool,
) -> Cmd<crate::musiccomplete::LyricsView> {
    let view = tauri::async_runtime::spawn_blocking(move || {
        crate::musiccomplete::lyrics_view(std::path::Path::new(&path), fetch)
    })
    .await
    .map_err(|e| {
        CommandError::from(UfmError::Internal(format!("lyrics task failed: {e}")))
    })?
    .map_err(|e| CommandError::from(UfmError::InvalidInput(e)))?;
    Ok(view)
}

#[tauri::command]
pub async fn musiccomplete_lyrics_save(
    state: State<'_, AppState>,
    path: String,
    text: Option<String>,
) -> Cmd<String> {
    let base = mc_base(&state);
    let msg = tauri::async_runtime::spawn_blocking(move || {
        let settings = crate::musiccomplete::load_settings(&base);
        crate::musiccomplete::lyrics_save(
            std::path::Path::new(&path),
            text.as_deref(),
            &settings,
        )
    })
    .await
    .map_err(|e| {
        CommandError::from(UfmError::Internal(format!("lyrics save failed: {e}")))
    })?
    .map_err(|e| CommandError::from(UfmError::InvalidInput(e)))?;
    Ok(msg)
}

#[tauri::command]
pub fn musiccomplete_list_runs(
    state: State<'_, AppState>,
) -> Cmd<Vec<MuRunView>> {
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(|e| CommandError::from(UfmError::Internal(e.to_string())))?;
    let rows = catalog
        .recent_musiccomplete_runs(20)
        .map_err(|e| CommandError::from(UfmError::Internal(e.to_string())))?;
    Ok(rows.iter().map(mu_run_view).collect())
}

#[tauri::command]
pub async fn musiccomplete_run_now(
    state: State<'_, AppState>,
    schedule_id: String,
) -> Cmd<MuRunView> {
    let base = mc_base(&state);
    let settings = crate::musiccomplete::load_settings(&base);
    let Some(sched) = settings
        .schedules
        .iter()
        .find(|s| s.id == schedule_id)
        .cloned()
    else {
        return Err(CommandError::from(UfmError::InvalidInput(
            "no such schedule".into(),
        )));
    };
    let sched_id = sched.id.clone();
    let row = tauri::async_runtime::spawn_blocking(move || {
        crate::musiccomplete::run_scheduled(&sched, false)
    })
    .await
    .map_err(|e| {
        CommandError::from(UfmError::Internal(format!("run task failed: {e}")))
    })?
    .map_err(|e| CommandError::from(UfmError::InvalidInput(e)))?;
    // Stamp the schedule's bookkeeping so the agent doesn't fire
    // the same slot again tonight.
    let mut settings = crate::musiccomplete::load_settings(&base);
    if let Some(s) = settings.schedules.iter_mut().find(|s| s.id == sched_id) {
        s.last_started = Some(
            chrono::Local::now()
                .naive_local()
                .format("%Y-%m-%dT%H:%M:%S")
                .to_string(),
        );
        s.last_status = Some(row.status.clone());
        let _ = crate::musiccomplete::save_settings(&base, &settings);
    }
    Ok(mu_run_view(&row))
}

#[tauri::command]
pub async fn musiccomplete_undo_run(run_id: String) -> Cmd<String> {
    let msg = tauri::async_runtime::spawn_blocking(move || {
        crate::musiccomplete::undo_run(&run_id)
    })
    .await
    .map_err(|e| {
        CommandError::from(UfmError::Internal(format!("undo task failed: {e}")))
    })?
    .map_err(|e| CommandError::from(UfmError::InvalidInput(e)))?;
    Ok(msg)
}

/// Confirm one pending item from a music run's morning checklist:
/// the file is re-proposed fresh and completed on the spot (fills
/// only — overwrites still need the review surface). Removes the
/// entry from the run's pending list.
#[tauri::command]
pub async fn musiccomplete_confirm_pending(
    run_id: String,
    path: String,
) -> Cmd<String> {
    let msg = tauri::async_runtime::spawn_blocking(move || {
        crate::musiccomplete::confirm_pending(&run_id, &path)
    })
    .await
    .map_err(|e| {
        CommandError::from(UfmError::Internal(format!("confirm task failed: {e}")))
    })?
    .map_err(|e| CommandError::from(UfmError::InvalidInput(e)))?;
    Ok(msg)
}

// ---------------------------------------------------------------------------
// File Auditor commands (Dickles, 2026-10-15): settings, runs, the
// checklist (flags), resolutions (the confirm / ask-lane / dismissal
// machinery), and the memory managers. Every fix lands as a
// journaled job through start_job — the Auditor proposes, the user
// disposes, the journal remembers.
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditorRunView {
    pub id: String,
    pub schedule_name: String,
    pub target: String,
    pub started: String,
    pub finished: Option<String>,
    pub was_late: bool,
    pub status: String,
    pub summary: serde_json::Value,
}

fn auditor_run_view(row: &ufm_catalog::AuditorRunRow) -> AuditorRunView {
    AuditorRunView {
        id: row.id.clone(),
        schedule_name: row.schedule_name.clone(),
        target: row.target.clone(),
        started: row.started.clone(),
        finished: row.finished.clone(),
        was_late: row.was_late,
        status: row.status.clone(),
        summary: serde_json::from_str(&row.summary_json)
            .unwrap_or_else(|_| serde_json::json!({})),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditorFlagView {
    pub id: String,
    pub run_id: String,
    pub path: String,
    pub family: String,
    pub lane: String,
    pub class_key: String,
    pub title: String,
    pub detail: String,
    pub proposal: Option<serde_json::Value>,
    pub state: String,
    pub created: String,
}

fn auditor_flag_view(row: &ufm_catalog::AuditorFlagRow) -> AuditorFlagView {
    AuditorFlagView {
        id: row.id.clone(),
        run_id: row.run_id.clone(),
        path: row.path.clone(),
        family: row.family.clone(),
        lane: row.lane.clone(),
        class_key: row.class_key.clone(),
        title: row.title.clone(),
        detail: row.detail.clone(),
        proposal: row
            .proposal_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok()),
        state: row.state.clone(),
        created: row.created.clone(),
    }
}

fn auditor_catalog(state: &AppState) -> Cmd<ufm_catalog::Catalog> {
    ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(|e| CommandError::from(UfmError::Internal(e.to_string())))
}

#[tauri::command]
pub fn auditor_get_settings(
    state: State<'_, AppState>,
) -> Cmd<crate::auditor::AuditorSettings> {
    Ok(crate::auditor::load_settings(&mc_base(&state)))
}

#[tauri::command]
pub fn auditor_save_settings(
    state: State<'_, AppState>,
    settings: crate::auditor::AuditorSettings,
) -> Cmd<crate::auditor::AuditorSettings> {
    let base = mc_base(&state);
    crate::auditor::save_settings(&base, &settings)
        .map_err(|e| CommandError::from(UfmError::Internal(e)))?;
    Ok(crate::auditor::load_settings(&base))
}

#[tauri::command]
pub async fn auditor_run_now(
    state: State<'_, AppState>,
    target: String,
    schedule_id: Option<String>,
) -> Cmd<AuditorRunView> {
    let base = mc_base(&state);
    let catalog_path = state.catalog_path.clone();
    let settings = crate::auditor::load_settings(&base);
    let (name, late) = match &schedule_id {
        Some(id) => settings
            .schedules
            .iter()
            .find(|s| &s.id == id)
            .map(|s| (s.name.clone(), false))
            .unwrap_or_else(|| ("Scheduled audit".to_string(), false)),
        None => ("Manual audit".to_string(), false),
    };
    let sid = schedule_id.clone();
    let row = tauri::async_runtime::spawn_blocking(move || {
        let catalog = ufm_catalog::Catalog::open(&catalog_path)
            .map_err(|e| format!("catalog open failed: {e}"))?;
        let probe = crate::auditor::FfprobeProbe::new();
        crate::auditor::run_audit(
            &catalog,
            &crate::auditor::RunRequest {
                target,
                schedule_id: sid,
                schedule_name: name,
                late,
            },
            &settings,
            &probe,
        )
    })
    .await
    .map_err(|e| {
        CommandError::from(UfmError::Internal(format!("audit task failed: {e}")))
    })?
    .map_err(|e| CommandError::from(UfmError::InvalidInput(e)))?;
    if let Some(id) = &schedule_id {
        let mut settings = crate::auditor::load_settings(&base);
        if let Some(s) = settings.schedules.iter_mut().find(|s| &s.id == id) {
            s.last_started = Some(crate::auditor::stamp(crate::auditor::now_local()));
            s.last_status = Some(row.status.clone());
            let _ = crate::auditor::save_settings(&base, &settings);
        }
    }
    Ok(auditor_run_view(&row))
}

#[tauri::command]
pub fn auditor_list_runs(state: State<'_, AppState>) -> Cmd<Vec<AuditorRunView>> {
    let catalog = auditor_catalog(&state)?;
    let rows = catalog
        .recent_auditor_runs(20)
        .map_err(|e| CommandError::from(UfmError::Internal(e.to_string())))?;
    Ok(rows.iter().map(auditor_run_view).collect())
}

#[tauri::command]
pub fn auditor_list_flags(
    state: State<'_, AppState>,
    state_filter: Option<String>,
) -> Cmd<Vec<AuditorFlagView>> {
    let catalog = auditor_catalog(&state)?;
    let rows = catalog
        .auditor_flags(state_filter.as_deref(), 1000)
        .map_err(|e| CommandError::from(UfmError::Internal(e.to_string())))?;
    Ok(rows.iter().map(auditor_flag_view).collect())
}

#[tauri::command]
pub fn auditor_open_count(state: State<'_, AppState>) -> Cmd<u64> {
    let catalog = auditor_catalog(&state)?;
    catalog
        .auditor_open_flag_count()
        .map_err(|e| CommandError::from(UfmError::Internal(e.to_string())))
}

fn auditor_step(
    id: &str,
    label: String,
    op: ufm_jobs::StepOp,
    outcome: bool,
    depends_on: Vec<String>,
) -> ufm_jobs::Step {
    ufm_jobs::Step {
        id: id.to_string(),
        label,
        op,
        depends_on,
        state: ufm_jobs::StepState::Pending,
        compensation: None,
        outcome,
        outcome_path: None,
    }
}

/// Build the journaled job a flag's plan calls for.
fn auditor_fix_job(flag: &ufm_catalog::AuditorFlagRow) -> Cmd<ufm_jobs::Job> {
    use ufm_jobs::{Job, JobPreview, StepOp};
    let path = PathBuf::from(&flag.path);
    let meta = std::fs::symlink_metadata(&path).map_err(|e| {
        CommandError::from(UfmError::InvalidInput(format!(
            "{} is no longer there ({e})",
            flag.path
        )))
    })?;
    let kind = if meta.is_dir() {
        ItemKind::Folder
    } else {
        ItemKind::File
    };
    let item = job_item(
        ItemId::new(),
        LocationId::new(),
        &path,
        kind,
        Some(meta.len()),
    );
    let name = item.name.clone();
    let job = match crate::auditor::plan_for(flag) {
        crate::auditor::FixPlan::Rename { to_name } => {
            let parent_path = path.parent().ok_or_else(|| {
                CommandError::from(UfmError::InvalidInput("no parent folder".into()))
            })?;
            let dst = parent_path.join(&to_name);
            if dst.exists() {
                return Err(CommandError::from(UfmError::InvalidInput(format!(
                    "“{to_name}” already exists in that folder — nothing was renamed"
                ))));
            }
            let parent_item = job_item(
                ItemId::new(),
                LocationId::new(),
                parent_path,
                ItemKind::Folder,
                None,
            );
            let mut j = Job::new(
                format!("Rename {name} → {to_name}"),
                JobPreview::default(),
            );
            j.add_step(auditor_step(
                "s0001",
                format!("rename to {to_name}"),
                StepOp::Move {
                    src: item,
                    dst_parent: parent_item,
                    new_name: to_name,
                },
                true,
                vec![],
            ));
            j
        }
        crate::auditor::FixPlan::Trash => {
            let mut j = Job::new(format!("Trash {name}"), JobPreview::default());
            j.add_step(auditor_step(
                "s0001",
                format!("move {name} to trash"),
                StepOp::Delete {
                    target: item,
                    use_trash: true,
                },
                true,
                vec![],
            ));
            j
        }
        crate::auditor::FixPlan::Convert {
            dst_name,
            kind,
            replace,
        } => {
            let mut j = Job::new(
                format!("Convert {name} ({kind})"),
                JobPreview::default(),
            );
            j.add_step(auditor_step(
                "s0001",
                format!("convert to {dst_name}"),
                StepOp::ConvertMedia {
                    src: item.clone(),
                    dst_name,
                    kind,
                },
                true,
                vec![],
            ));
            if replace {
                j.add_step(auditor_step(
                    "s0002",
                    format!("move original {name} to trash"),
                    StepOp::Delete {
                        target: item,
                        use_trash: true,
                    },
                    false,
                    vec!["s0001".to_string()],
                ));
            }
            j
        }
        crate::auditor::FixPlan::None => {
            return Err(CommandError::from(UfmError::InvalidInput(
                "this flag has no fix to apply — Skip it, once or forever"
                    .into(),
            )));
        }
    };
    Ok(job)
}

/// Fix All Similar: compose ONE journaled job covering every
/// given flag's fix (steps renumbered into one sequence, each
/// flag's internal step dependencies remapped). Flags whose fix
/// can't be planned anymore (file gone, target name now taken)
/// are skipped, not fatal — the caller reports how many landed.
/// Returns the job and the flags actually included.
fn auditor_fix_all_job(
    flags: &[ufm_catalog::AuditorFlagRow],
) -> Cmd<(ufm_jobs::Job, Vec<String>)> {
    use ufm_jobs::{Job, JobPreview};
    let mut combined: Option<Job> = None;
    let mut included: Vec<String> = Vec::new();
    let mut next = 0usize;
    for flag in flags {
        let Ok(mut single) = auditor_fix_job(flag) else {
            continue;
        };
        let target = combined.get_or_insert_with(|| {
            Job::new(
                format!(
                    "Fix {} similar items ({})",
                    flags.len(),
                    flag.class_key
                ),
                JobPreview::default(),
            )
        });
        // Renumber this flag's steps into the combined sequence.
        let mut steps: Vec<ufm_jobs::Step> = single.steps.drain().map(|(_, s)| s).collect();
        steps.sort_by(|a, b| a.id.cmp(&b.id));
        let mut id_map: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for s in &steps {
            next += 1;
            id_map.insert(s.id.clone(), format!("s{next:04}"));
        }
        for mut s in steps {
            s.id = id_map[&s.id].clone();
            s.depends_on = s
                .depends_on
                .iter()
                .map(|d| id_map.get(d).cloned().unwrap_or_else(|| d.clone()))
                .collect();
            target.add_step(s);
        }
        included.push(flag.id.clone());
    }
    match combined {
        Some(mut job) if !included.is_empty() => {
            job.title = format!(
                "Fix {} similar item{} ({})",
                included.len(),
                if included.len() == 1 { "" } else { "s" },
                flags[0].class_key
            );
            Ok((job, included))
        }
        _ => Err(CommandError::from(UfmError::InvalidInput(
            "none of the matching files could be fixed anymore — they may have moved or been renamed".into(),
        ))),
    }
}

/// Trash job for proposal-less flags (copy chains, cold files…) —
/// refused for folders except the empty-folder family it exists
/// for, so a series-gap flag (whose path IS a folder) can never
/// trash a tree by accident.
fn auditor_trash_job(flag: &ufm_catalog::AuditorFlagRow) -> Cmd<ufm_jobs::Job> {
    use ufm_jobs::{Job, JobPreview, StepOp};
    let path = PathBuf::from(&flag.path);
    let meta = std::fs::symlink_metadata(&path).map_err(|e| {
        CommandError::from(UfmError::InvalidInput(format!(
            "{} is no longer there ({e})",
            flag.path
        )))
    })?;
    if meta.is_dir() && flag.class_key != "clutter:empty-folder" {
        return Err(CommandError::from(UfmError::InvalidInput(
            "that flag is about a whole folder — its files are listed individually; nothing was trashed"
                .into(),
        )));
    }
    let kind = if meta.is_dir() {
        ItemKind::Folder
    } else {
        ItemKind::File
    };
    let item = job_item(
        ItemId::new(),
        LocationId::new(),
        &path,
        kind,
        Some(meta.len()),
    );
    let mut j = Job::new(format!("Trash {}", item.name), JobPreview::default());
    j.add_step(auditor_step(
        "s0001",
        "move to trash".into(),
        StepOp::Delete {
            target: item,
            use_trash: true,
        },
        true,
        vec![],
    ));
    Ok(j)
}

/// Resolve one checklist item.
/// resolution: "apply" (run the plan as a job) | "trash_it" |
/// "ok" (dismissal memory). scope (for "ok" and ask-lane answers):
/// "one" | "similar" | "never" — "similar"/"never" write a
/// standing class rule and close every open flag of that class.
#[tauri::command]
pub async fn auditor_resolve(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    flag_id: String,
    resolution: String,
    scope: Option<String>,
) -> Cmd<String> {
    // `scope` is kept for call compatibility; the four-label
    // answer grammar carries its scope in the resolution itself.
    let _ = &scope;
    let catalog = auditor_catalog(&state)?;
    let flag = catalog
        .auditor_flag(&flag_id)
        .map_err(|e| CommandError::from(UfmError::Internal(e.to_string())))?
        .ok_or_else(|| {
            CommandError::from(UfmError::InvalidInput("no such flag".into()))
        })?;
    let now = crate::auditor::stamp(crate::auditor::now_local());
    let set_state = |catalog: &ufm_catalog::Catalog, state_name: &str| {
        let _ = catalog.set_auditor_flag_state(&flag.id, state_name, &now);
    };
    match resolution.as_str() {
        "apply" => {
            let job = auditor_fix_job(&flag)?;
            start_job(app, &state, job).map(|_| {
                set_state(&catalog, "confirmed");
                "fix started as a job — watch it in Transfers".to_string()
            })
        }
        "trash_it" => {
            let job = auditor_trash_job(&flag)?;
            start_job(app, &state, job).map(|_| {
                set_state(&catalog, "confirmed");
                "moved to trash as a job".to_string()
            })
        }
        // The answer grammar (Dickles, 2026-10-15 — four exact
        // labels on every question card): "Just this one" (the
        // apply/trash_it arms), "Fix All Similar (Irreversible)",
        // "Skip This One Once", "Skip This One Forever". The old
        // scope-select answers ("This one / All similar / Never…")
        // are retired.
        "apply_all" => {
            // ONE journaled job covering every current match of
            // this class, plus the standing rule: this question
            // never repeats — future matches arrive as FACTS with
            // the fix proposed (run_audit promotes the lane), never
            // silently fixed (the founding law).
            let open = catalog
                .auditor_flags(Some("open"), 5000)
                .unwrap_or_default();
            let matches: Vec<ufm_catalog::AuditorFlagRow> = open
                .into_iter()
                .filter(|f| f.class_key == flag.class_key)
                .collect();
            let (job, included) = auditor_fix_all_job(&matches)?;
            let count = included.len();
            start_job(app, &state, job).map(|_| {
                for id in &included {
                    let _ = catalog.set_auditor_flag_state(id, "confirmed", &now);
                }
                let _ = catalog.set_auditor_rule(&flag.class_key, "fix", &now);
                format!(
                    "fix started for {count} file{} as one job — watch it in Transfers. Files like this will arrive as facts with the fix proposed from now on",
                    if count == 1 { "" } else { "s" }
                )
            })
        }
        "skip_once" => {
            // Dismissed for THIS run only: no dismissal memory, no
            // rule. The next audit re-derives the flag from the
            // file and it resurfaces (the run pipeline only
            // carries state='open' flags, so a 'skipped' row is
            // history, not a match).
            set_state(&catalog, "skipped");
            Ok("skipped for now — it will come back on the next audit".into())
        }
        "skip_forever" => {
            catalog
                .add_auditor_dismissal(&flag.path, &flag.family, &now)
                .map_err(|e| {
                    CommandError::from(UfmError::Internal(e.to_string()))
                })?;
            set_state(&catalog, "dismissed");
            Ok("remembered — this one won't be flagged again".into())
        }
        other => Err(CommandError::from(UfmError::InvalidInput(format!(
            "unknown resolution: {other}"
        )))),
    }
}

/// Clear findings (Auditor item 13, Dickles 2026-10-15): empties
/// the findings list ONLY. Dismissal memory and ask-lane rules
/// survive — what he taught it stays taught; the full reset is a
/// separate Preferences-level action. The clear is JOURNALED as a
/// run-history row carrying the cleared count + timestamp, so the
/// Auditor's own history shows it happened. The UI only calls
/// this after its warning dialog is confirmed.
#[tauri::command]
pub fn auditor_clear_findings(state: State<'_, AppState>) -> Cmd<String> {
    let catalog = auditor_catalog(&state)?;
    let cleared = catalog
        .clear_auditor_flags()
        .map_err(|e| CommandError::from(UfmError::Internal(e.to_string())))?;
    let now = crate::auditor::stamp(crate::auditor::now_local());
    let target = catalog
        .recent_auditor_runs(1)
        .ok()
        .and_then(|rows| rows.into_iter().next())
        .map(|r| r.target)
        .unwrap_or_default();
    let row = ufm_catalog::AuditorRunRow {
        id: uuid::Uuid::new_v4().to_string(),
        schedule_id: None,
        schedule_name: "Findings cleared".into(),
        target,
        started: now.clone(),
        finished: Some(now),
        was_late: false,
        status: "cleared".into(),
        summary_json: serde_json::json!({
            "clearedFlags": cleared,
            "notes": [format!(
                "Cleared {cleared} finding(s). Nothing was renamed, moved, or deleted — skips and standing rules were kept."
            )],
        })
        .to_string(),
    };
    let _ = catalog.upsert_auditor_run(&row);
    Ok(format!(
        "cleared {cleared} finding{} — your skips and rules are untouched",
        if cleared == 1 { "" } else { "s" }
    ))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditorDismissalView {
    pub path: String,
    pub family: String,
    pub created: String,
}

#[tauri::command]
pub fn auditor_list_dismissals(
    state: State<'_, AppState>,
) -> Cmd<Vec<AuditorDismissalView>> {
    let catalog = auditor_catalog(&state)?;
    let rows = catalog
        .auditor_dismissals()
        .map_err(|e| CommandError::from(UfmError::Internal(e.to_string())))?;
    Ok(rows
        .into_iter()
        .map(|(path, family, created)| AuditorDismissalView {
            path,
            family,
            created,
        })
        .collect())
}

#[tauri::command]
pub fn auditor_delete_dismissal(
    state: State<'_, AppState>,
    path: String,
    family: String,
) -> Cmd<()> {
    let catalog = auditor_catalog(&state)?;
    catalog
        .delete_auditor_dismissal(&path, &family)
        .map_err(|e| CommandError::from(UfmError::Internal(e.to_string())))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditorRuleView {
    pub class_key: String,
    pub decision: String,
    pub created: String,
}

#[tauri::command]
pub fn auditor_list_rules(state: State<'_, AppState>) -> Cmd<Vec<AuditorRuleView>> {
    let catalog = auditor_catalog(&state)?;
    let rows = catalog
        .auditor_rules()
        .map_err(|e| CommandError::from(UfmError::Internal(e.to_string())))?;
    Ok(rows
        .into_iter()
        .map(|(class_key, decision, created)| AuditorRuleView {
            class_key,
            decision,
            created,
        })
        .collect())
}

#[tauri::command]
pub fn auditor_delete_rule(state: State<'_, AppState>, class_key: String) -> Cmd<()> {
    let catalog = auditor_catalog(&state)?;
    catalog
        .delete_auditor_rule(&class_key)
        .map_err(|e| CommandError::from(UfmError::Internal(e.to_string())))
}

#[tauri::command]
pub fn auditor_list_user_words(state: State<'_, AppState>) -> Cmd<Vec<String>> {
    let catalog = auditor_catalog(&state)?;
    catalog
        .auditor_user_words()
        .map_err(|e| CommandError::from(UfmError::Internal(e.to_string())))
}

#[tauri::command]
pub fn auditor_add_user_word(
    state: State<'_, AppState>,
    word: String,
) -> Cmd<Vec<String>> {
    let catalog = auditor_catalog(&state)?;
    let now = crate::auditor::stamp(crate::auditor::now_local());
    catalog
        .add_auditor_user_word(word.trim(), &now)
        .map_err(|e| CommandError::from(UfmError::Internal(e.to_string())))?;
    catalog
        .auditor_user_words()
        .map_err(|e| CommandError::from(UfmError::Internal(e.to_string())))
}

#[cfg(test)]
mod tests {
    #[test]
    fn auditor_fix_all_composes_one_job_for_all_matches() {
        // Fix All Similar (Dickles, 2026-10-15): every current
        // match of a class lands in ONE journaled job.
        let dir = std::env::temp_dir().join(format!(
            "fable-fixall-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a")).unwrap();
        std::fs::create_dir_all(dir.join("b")).unwrap();
        std::fs::write(dir.join("a/The Richrd Story.txt"), b"x").unwrap();
        std::fs::write(dir.join("b/The Richrd Story.txt"), b"x").unwrap();
        let mk = |sub: &str, id: &str| ufm_catalog::AuditorFlagRow {
            id: id.into(),
            run_id: "r".into(),
            path: dir.join(sub).join("The Richrd Story.txt").to_string_lossy().into_owned(),
            family: "typo".into(),
            lane: "question".into(),
            class_key: "typo:richrd".into(),
            title: "t".into(),
            detail: String::new(),
            proposal_json: Some(
                serde_json::json!({"kind": "rename", "to": "The Richard Story.txt"})
                    .to_string(),
            ),
            state: "open".into(),
            created: "t".into(),
            updated: "t".into(),
        };
        let flags = vec![mk("a", "f1"), mk("b", "f2")];
        let (job, included) = super::auditor_fix_all_job(&flags).expect("job composes");
        assert_eq!(included.len(), 2);
        assert_eq!(job.steps.len(), 2, "ONE job holding both renames");
        assert!(job.title.contains('2'), "title counts the matches: {}", job.title);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn seam_job_requests_accept_frontend_camel_case() {
        // SEAM TEST (ROM-lists fix block item 1): the frontend sends
        // camelCase keys — dstParent / dstDir / useTrash (ui/src/api.ts).
        // These fixtures mirror those payloads exactly. When the request
        // structs lacked #[serde(rename_all = "camelCase")], EVERY
        // start_* command rejected every call with "missing field
        // `use_trash`" / `dst_dir` / `dst_parent` — Trash, Move, Copy
        // and both FromPaths variants were all dead (2026-10-14).
        // If a rename is ever lost again, this test fails the gate
        // instead of the user.
        let id = "11111111-1111-1111-1111-111111111111";
        let copy: super::CopyRequest = serde_json::from_value(serde_json::json!({
            "sources": [id], "dstParent": id
        }))
        .expect("start_copy payload must deserialize");
        assert_eq!(copy.sources.len(), 1);
        assert_eq!(copy.dst_parent.to_string(), id);

        let mv: super::MoveRequest = serde_json::from_value(serde_json::json!({
            "sources": [id], "dstDir": "/tmp/dest"
        }))
        .expect("start_move payload must deserialize");
        assert_eq!(mv.dst_dir, "/tmp/dest");

        let mvp: super::MovePathsRequest = serde_json::from_value(serde_json::json!({
            "sources": ["/a/b"], "dstDir": "/tmp/dest"
        }))
        .expect("start_move_from_paths payload must deserialize");
        assert_eq!(mvp.dst_dir, "/tmp/dest");

        let cpp: super::CopyPathsRequest = serde_json::from_value(serde_json::json!({
            "sources": ["/a/b"], "dstDir": "/tmp/dest"
        }))
        .expect("start_copy_from_paths payload must deserialize");
        assert_eq!(cpp.dst_dir, "/tmp/dest");

        let del: super::DeleteRequest = serde_json::from_value(serde_json::json!({
            "sources": [id], "useTrash": true
        }))
        .expect("start_delete payload must deserialize");
        assert!(del.use_trash);
        let del_perm: super::DeleteRequest = serde_json::from_value(serde_json::json!({
            "sources": [id], "useTrash": false
        }))
        .expect("start_delete (permanent) payload must deserialize");
        assert!(!del_perm.use_trash);

        let delp: super::DeletePathsRequest = serde_json::from_value(serde_json::json!({
            "paths": ["/a/b"], "useTrash": true
        }))
        .expect("start_delete_from_paths payload must deserialize");
        assert!(delp.use_trash);

        // The old snake_case shape must NOT parse (it would mean the
        // rename fell off and the frontend broke silently again).
        assert!(serde_json::from_value::<super::DeleteRequest>(serde_json::json!({
            "sources": [id], "use_trash": true
        }))
        .is_err());
        assert!(serde_json::from_value::<super::MoveRequest>(serde_json::json!({
            "sources": [id], "dst_dir": "/tmp/dest"
        }))
        .is_err());
    }

    #[test]
    fn job_start_serializes_camel_case() {
        // UI round: the start_* commands return JobStart; the
        // frontend reads jobId / alreadyRunning (api.ts JobStart).
        // A lost rename here hands the shell an undefined job id and
        // silently breaks every transfer door.
        let v = serde_json::to_value(super::JobStart {
            job_id: "abc".into(),
            already_running: true,
        })
        .expect("JobStart serializes");
        assert_eq!(v["jobId"], "abc");
        assert_eq!(v["alreadyRunning"], true);
    }

    #[test]
    fn email_type_extensions_cover_mail_files() {
        // UI round (Dickles: "I'm old"): Email is a search type.
        let exts = super::type_extensions("email").expect("email type exists");
        for e in ["eml", "msg", "mbox"] {
            assert!(exts.contains(&e), "email type must cover .{e}");
        }
    }

    #[test]
    fn job_signature_dedupes_identical_submissions() {
        // UI round double-submit guard: identical submissions must
        // share a signature (independent of the job's own id), and
        // anything that makes them a different operation — op kind,
        // sources, destination — must change it. A decomposed
        // cross-device move (Copy outcome steps + Delete steps for
        // the originals) must sign the same as a plain move.
        use ufm_jobs::{Job, JobPreview, Step, StepOp, StepState};
        let item = |path: &str| {
            super::job_item(
                ufm_core::ItemId(uuid::Uuid::nil()),
                ufm_core::LocationId(uuid::Uuid::nil()),
                std::path::Path::new(path),
                ufm_vfs::ItemKind::Folder,
                Some(10),
            )
        };
        // NOTE: step ids must be unique per job — add_step keys the
        // steps map by id, so a shared id silently eats steps (the
        // first version of this test did exactly that and its
        // decomposed move degenerated to a lone Delete).
        let step = |id: &str, op: StepOp, outcome: bool| Step {
            id: id.into(),
            label: "step".into(),
            op,
            depends_on: Vec::new(),
            state: StepState::Pending,
            compensation: None,
            outcome,
            outcome_path: None,
        };
        let copy_op = || StepOp::Copy {
            src: item("/superdog/richard"),
            dst_parent: item("/media/Desk SSD"),
            new_name: "richard".into(),
        };
        let mut copy1 = Job::new("Copy richard", JobPreview::default());
        copy1.add_step(step("s1", copy_op(), true));
        let mut copy2 = Job::new("Copy richard", JobPreview::default());
        copy2.add_step(step("s1", copy_op(), true));
        assert_eq!(super::job_signature(&copy1), super::job_signature(&copy2));

        let mut decomposed_move = Job::new("Move richard", JobPreview::default());
        decomposed_move.add_step(step("s1", copy_op(), true));
        decomposed_move.add_step(step(
            "s2",
            StepOp::Delete {
                target: item("/superdog/richard"),
                use_trash: false,
            },
            false,
        ));
        let mut plain_move = Job::new("Move richard", JobPreview::default());
        plain_move.add_step(step(
            "s1",
            StepOp::Move {
                src: item("/superdog/richard"),
                dst_parent: item("/media/Desk SSD"),
                new_name: "richard".into(),
            },
            true,
        ));
        assert_eq!(
            super::job_signature(&decomposed_move),
            super::job_signature(&plain_move)
        );
        assert_ne!(
            super::job_signature(&copy1),
            super::job_signature(&plain_move)
        );

        // Destination changes the operation.
        let mut copy_elsewhere = Job::new("Copy richard", JobPreview::default());
        copy_elsewhere.add_step(step(
            "s1",
            StepOp::Copy {
                src: item("/superdog/richard"),
                dst_parent: item("/media/Other"),
                new_name: "richard".into(),
            },
            true,
        ));
        assert_ne!(
            super::job_signature(&copy1),
            super::job_signature(&copy_elsewhere)
        );

        // Trash and permanent delete are different operations.
        let mut trash = Job::new("Trash", JobPreview::default());
        trash.add_step(step(
            "s1",
            StepOp::Delete {
                target: item("/superdog/richard"),
                use_trash: true,
            },
            true,
        ));
        let mut perm = Job::new("Delete", JobPreview::default());
        perm.add_step(step(
            "s1",
            StepOp::Delete {
                target: item("/superdog/richard"),
                use_trash: false,
            },
            true,
        ));
        assert_ne!(super::job_signature(&trash), super::job_signature(&perm));
        let mut trash2 = Job::new("Trash", JobPreview::default());
        trash2.add_step(step(
            "s1",
            StepOp::Delete {
                target: item("/superdog/richard"),
                use_trash: true,
            },
            true,
        ));
        assert_eq!(super::job_signature(&trash), super::job_signature(&trash2));
    }

    #[test]
    fn packed_shape_thresholds() {
        // UI round (packed transfer): the shape gate — count AND
        // mean size must both favor packing.
        assert!(!super::packed_shape(499, 499 * 1024)); // too few files
        assert!(!super::packed_shape(500, 500 * 2 * 1024 * 1024)); // mean 2 MiB
        assert!(super::packed_shape(500, 500 * 512 * 1024)); // mean 512 KiB
        assert!(super::packed_shape(20_000, 20_000 * 24 * 1024)); // his ROM tree shape
        assert!(super::packed_shape(500, 0)); // empty files are all overhead
    }

    #[test]
    fn ustar_header_roundtrip() {
        // UI round (packed transfer): the stream format is real
        // ustar — headers the packer writes must parse back exactly,
        // and the zero block must read as end-of-archive.
        for size in [0u64, 1, 511, 512, 513, 123_456, 512 * 1024 * 1024] {
            let h = super::ustar_header("s0042", size, 0o644);
            let (name, parsed_size, mode) =
                super::ustar_parse(&h).expect("header must parse");
            assert_eq!(name, "s0042");
            assert_eq!(parsed_size, size);
            assert_eq!(mode, 0o644);
            // Magic + checksum sanity (a real tar reader checks both).
            assert_eq!(&h[257..262], b"ustar");
            let stored = std::str::from_utf8(&h[148..154]).unwrap();
            let want: u64 = h
                .iter()
                .enumerate()
                .map(|(i, &b)| {
                    if (148..156).contains(&i) {
                        b' ' as u64
                    } else {
                        b as u64
                    }
                })
                .sum();
            assert_eq!(u64::from_str_radix(stored, 8).unwrap(), want);
        }
        assert!(super::ustar_parse(&[0u8; 512]).is_none());
    }

    #[test]
    fn packed_stream_extracts_and_reports_only_completed() {
        // UI round (packed transfer): the unpacker extracts a real
        // stream (fed in awkward 100-byte blocks to stress the
        // reassembly), reports exactly the files it finished, and a
        // truncated tail is NOT reported — its step stays Pending
        // for the per-file fallback, and no partial file survives.
        let root = std::env::temp_dir().join(format!(
            "ff-packed-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let dest_dir = root.join("deep/nested");
        std::fs::create_dir_all(&dest_dir).unwrap();
        let mk = |name: &str, body: &[u8]| {
            let mut s = super::ustar_header(name, body.len() as u64, 0o644).to_vec();
            s.extend_from_slice(body);
            let pad = (512 - body.len() % 512) % 512;
            s.extend(std::iter::repeat(0u8).take(pad));
            s
        };
        let mut stream = Vec::new();
        stream.extend(mk("s0001", b"hello"));
        stream.extend(mk("s0002", &[7u8; 1500]));
        // Truncated third entry: header promises 100 bytes, 40 arrive.
        stream.extend(super::ustar_header("s0003", 100, 0o644).to_vec());
        stream.extend([1u8; 40]);
        let dests: std::sync::Arc<
            std::collections::HashMap<String, std::path::PathBuf>,
        > = std::sync::Arc::new(
            [
                ("s0001".to_string(), dest_dir.join("a.txt")),
                ("s0002".to_string(), dest_dir.join("b.bin")),
                ("s0003".to_string(), dest_dir.join("c.bin")),
            ]
            .into_iter()
            .collect(),
        );
        let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(4);
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<String>(16);
        let t = std::thread::spawn(move || super::unpack_stream(rx, dests, done_tx));
        for chunk in stream.chunks(100) {
            if tx.send(chunk.to_vec()).is_err() {
                break;
            }
        }
        drop(tx);
        t.join().unwrap();
        let reported: Vec<String> = done_rx.try_iter().collect();
        assert_eq!(reported, vec!["s0001".to_string(), "s0002".to_string()]);
        assert_eq!(
            std::fs::read(dest_dir.join("a.txt")).unwrap(),
            b"hello".to_vec()
        );
        assert_eq!(std::fs::read(dest_dir.join("b.bin")).unwrap().len(), 1500);
        assert!(!dest_dir.join("c.bin").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scale_transition_gate_keeps_blob_writes_rare() {
        // P0 scale test: a 50,000-step job (Dickles' live DAG is
        // ~465k steps / a 616 MB blob) driven through 3,000
        // transitions. The gate must keep blob checkpoints rare:
        // every transition still journals (append-only), DB growth
        // stays in single-digit multiples of ONE blob, and the run
        // takes seconds. The old design — clone + serialize + write
        // the whole blob per transition — fails all three by orders
        // of magnitude.
        use ufm_jobs::{Job, JobPreview, Step, StepOp, StepState};
        let dir = std::env::temp_dir()
            .join(format!("ff-scale-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("catalog.db");
        let catalog = ufm_catalog::Catalog::open(&db).unwrap();
        let item = |path: &str, size: u64| {
            super::job_item(
                ufm_core::ItemId(uuid::Uuid::nil()),
                ufm_core::LocationId(uuid::Uuid::nil()),
                std::path::Path::new(path),
                ufm_vfs::ItemKind::File,
                Some(size),
            )
        };
        let dst = item("/dst", 0);
        let mut job = Job::new("scale", JobPreview::default());
        for i in 0..50_000 {
            job.add_step(Step {
                id: format!("s{i:05}"),
                label: format!("copy f{i}"),
                op: StepOp::Copy {
                    src: item(&format!("/src/f{i}"), 10),
                    dst_parent: dst.clone(),
                    new_name: format!("f{i}"),
                },
                depends_on: Vec::new(),
                state: StepState::Pending,
                compensation: None,
                outcome: false,
                outcome_path: None,
            });
        }
        let job_id = job.id;
        catalog.upsert_job(&job).unwrap();
        let handle = super::JobHandle::new(job, false);
        let mut journal = ufm_jobs::JobJournal::with_sink(std::sync::Arc::new(
            ufm_catalog::CatalogJournalSink::new(db.clone()),
        ));
        let mut gate = super::SnapshotGate::new();
        let db_size = |db: &std::path::Path| -> u64 {
            let main = std::fs::metadata(db).map(|m| m.len()).unwrap_or(0);
            let wal = std::fs::metadata(db.with_extension("db-wal"))
                .map(|m| m.len())
                .unwrap_or(0);
            main + wal
        };
        let baseline = db_size(&db);
        let t0 = std::time::Instant::now();
        let mut fx_bytes = 0;
        for i in 0..3_000 {
            let fx = super::transition_step(
                &handle,
                &catalog,
                &mut journal,
                &mut gate,
                &format!("s{i:05}"),
                StepState::Done,
                None,
            );
            fx_bytes = fx.bytes_done;
        }
        let elapsed = t0.elapsed();
        assert_eq!(fx_bytes, 30_000);
        assert_eq!(journal.entries().len(), 3_000);
        let entries = catalog.journal_entries(&job_id).unwrap();
        assert_eq!(entries.len(), 3_000);
        let grew = db_size(&db) - baseline;
        assert!(
            grew < baseline * 6,
            "db grew {grew} bytes on a {baseline}-byte baseline — blob writes are not gated"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(30),
            "3,000 transitions took {elapsed:?} — per-transition cost is not ~O(1)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn zombie_cancel_settles_without_executor() {
        // P0: a job whose executor died (the OOM, the session kill)
        // must settle to Cancelled through the command's own path —
        // no executor will ever consume its cancel flag.
        use ufm_jobs::{Job, JobPreview, Step, StepOp, StepState};
        let dir = std::env::temp_dir()
            .join(format!("ff-zombie-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("catalog.db");
        let catalog = ufm_catalog::Catalog::open(&db).unwrap();
        let item = |path: &str| {
            super::job_item(
                ufm_core::ItemId(uuid::Uuid::nil()),
                ufm_core::LocationId(uuid::Uuid::nil()),
                std::path::Path::new(path),
                ufm_vfs::ItemKind::File,
                Some(10),
            )
        };
        let mut job = Job::new("zombie", JobPreview::default());
        job.state = ufm_jobs::JobState::Running; // as a crash leaves it
        job.add_step(Step {
            id: "s0001".into(),
            label: "copy".into(),
            op: StepOp::Copy {
                src: item("/src/a"),
                dst_parent: item("/dst"),
                new_name: "a".into(),
            },
            depends_on: Vec::new(),
            state: StepState::Pending,
            compensation: None,
            outcome: false,
            outcome_path: None,
        });
        let job_id = job.id;
        catalog.upsert_job(&job).unwrap();
        // live = false: JobHandle::new never starts an executor.
        let handle = super::JobHandle::new(job, false);
        super::settle_cancelled(&handle, &catalog);
        assert_eq!(handle.snapshot().state, ufm_jobs::JobState::Cancelled);
        let loaded = catalog.load_job(&job_id).unwrap().unwrap();
        assert_eq!(loaded.state, ufm_jobs::JobState::Cancelled);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pause_flag_blocks_until_cancel_releases() {
        // P0: the executor's pause poll must hold while the flag is
        // set and release ~immediately when cancel lands — the
        // sub-second contract, at the mechanism level.
        use ufm_jobs::{Job, JobPreview};
        let handle = std::sync::Arc::new(super::JobHandle::new(
            Job::new("pause", JobPreview::default()),
            true, // paused
        ));
        let h2 = handle.clone();
        let waiter = std::thread::spawn(move || super::wait_while_paused(&h2));
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!waiter.is_finished(), "pause flag must hold the waiter");
        handle
            .cancel
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let t0 = std::time::Instant::now();
        let released = waiter.join().unwrap();
        assert!(!released, "cancel must make wait_while_paused return false");
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(2),
            "cancel release took {:?} — not sub-second",
            t0.elapsed()
        );
    }

    #[test]
    fn mkfs_commands_are_well_formed() {
        // Labels are sanitized before they ever reach a shell string:
        // quotes and shell metacharacters must not survive.
        assert_eq!(sanitize_label("My Disk; rm -rf /"), "My Disk rm -rf");
        assert_eq!(sanitize_label("  spaced  "), "spaced");
        let cmd = mkfs_command("ext4", "Photos", "/dev/sdb1").unwrap();
        assert_eq!(cmd, "mkfs.ext4 -F -L 'Photos' '/dev/sdb1'");
        let cmd = mkfs_command("vfat", "usb stick", "/dev/sdb1").unwrap();
        assert_eq!(cmd, "mkfs.vfat -F 32 -n 'USB STICK' '/dev/sdb1'");
        let cmd = mkfs_command("exfat", "", "/dev/sdb1").unwrap();
        assert_eq!(cmd, "mkfs.exfat '/dev/sdb1'");
        assert!(mkfs_command("zfs", "", "/dev/sdb1").is_err());
    }

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
            files_unchanged: 0,
            files_updated: 0,
            files_removed: 0,
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
            files_unchanged: 0,
            files_updated: 0,
            files_removed: 0,
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

    // ---------------------------------------------------------------
    // Inc-2 (§4): planner shapes + executor cores. Fixtures are plain
    // files under a per-test temp dir; no Tauri runtime is needed
    // because the planner/executor cores take plain paths by design.
    // ---------------------------------------------------------------

    fn inc2_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ufm-inc2-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_file(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    fn op_target_key(step: &ufm_jobs::Step) -> Option<String> {
        match &step.op {
            ufm_jobs::StepOp::Delete { target, .. } => target.provider_key.clone(),
            ufm_jobs::StepOp::Move { src, .. } => src.provider_key.clone(),
            ufm_jobs::StepOp::Copy { src, .. } => src.provider_key.clone(),
            _ => None,
        }
    }

    fn find_step<'a>(
        job: &'a ufm_jobs::Job,
        key: &Path,
    ) -> Option<&'a ufm_jobs::Step> {
        let key = key.to_string_lossy().into_owned();
        job.steps
            .values()
            .find(|s| op_target_key(s).as_deref() == Some(key.as_str()))
    }

    #[test]
    fn move_plan_same_device_is_single_rename_steps() {
        let base = inc2_dir("move-same");
        let dst = base.join("dst");
        std::fs::create_dir_all(&dst).unwrap();
        let file = base.join("a.txt");
        write_file(&file, b"alpha");
        let tree = base.join("tree");
        write_file(&tree.join("sub/b.txt"), b"beta");
        let stay = dst.join("stay.txt");
        write_file(&stay, b"stay");
        write_file(&dst.join("collide.txt"), b"old");
        let collide = base.join("collide.txt");
        write_file(&collide, b"new");

        let job = plan_move_job(
            &[file.clone(), tree.clone(), stay.clone()],
            &dst,
            LocationId::default(),
        )
        .unwrap();
        // Exactly one Move step per real source; the already-there
        // source produced zero steps; nothing is decomposed.
        let moves: Vec<_> = job
            .steps
            .values()
            .filter(|s| matches!(s.op, ufm_jobs::StepOp::Move { .. }))
            .collect();
        assert_eq!(moves.len(), 2);
        assert!(moves.iter().all(|s| s.outcome));
        assert!(job
            .steps
            .values()
            .all(|s| !matches!(s.op, ufm_jobs::StepOp::Delete { .. })));

        // Collision: resolved name + a preview NameCollision conflict.
        let job2 = plan_move_job(&[collide], &dst, LocationId::default()).unwrap();
        assert_eq!(job2.steps.len(), 1);
        let only = job2.steps.values().next().unwrap();
        match &only.op {
            ufm_jobs::StepOp::Move { new_name, .. } => {
                assert_eq!(new_name, "collide.txt 2")
            }
            _ => panic!("expected a Move step"),
        }
        assert_eq!(job2.preview.conflicts.len(), 1);
        assert!(matches!(
            job2.preview.conflicts[0].kind,
            ufm_jobs::ConflictKind::NameCollision
        ));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn move_plan_cross_device_decomposes() {
        let base = inc2_dir("move-cross");
        let dst = base.join("dst");
        std::fs::create_dir_all(&dst).unwrap();
        let tree = base.join("tree");
        write_file(&tree.join("f1.txt"), b"one");
        write_file(&tree.join("sub/f2.txt"), b"two");

        let job = plan_move_job_with(
            &[tree.clone()],
            &dst,
            LocationId::default(),
            &|_, _| false,
        )
        .unwrap();

        let copies: Vec<_> = job
            .steps
            .values()
            .filter(|s| matches!(s.op, ufm_jobs::StepOp::Copy { .. }))
            .collect();
        assert_eq!(copies.len(), 2);
        // Every Copy has a dependent source Delete.
        for copy in &copies {
            let src_key = op_target_key(copy).unwrap();
            let del = job.steps.values().find(|s| {
                matches!(s.op, ufm_jobs::StepOp::Delete { use_trash: false, .. })
                    && op_target_key(s).as_deref() == Some(src_key.as_str())
                    && s.depends_on.iter().any(|d| d == &copy.id)
            });
            assert!(del.is_some(), "copy {} lacks a dependent delete", copy.id);
        }
        // Directory Deletes depend on everything inside them; the
        // root Delete carries the outcome.
        let root_del = find_step(&job, &tree).expect("root delete");
        assert!(matches!(
            root_del.op,
            ufm_jobs::StepOp::Delete { use_trash: false, .. }
        ));
        assert!(root_del.outcome);
        assert_eq!(
            root_del.outcome_path.as_deref(),
            Some(dst.join("tree").to_string_lossy().as_ref())
        );
        assert_eq!(root_del.depends_on.len(), job.steps.len() - 1);
        let sub_del = find_step(&job, &tree.join("sub")).expect("sub delete");
        assert!(sub_del.depends_on.len() >= 4);
        // Runnable order places every Delete after its Copy.
        let order = job.runnable_order().unwrap();
        let pos = |id: &str| order.iter().position(|s| s == id).unwrap();
        for copy in &copies {
            let del = job
                .steps
                .values()
                .find(|s| {
                    s.depends_on.iter().any(|d| d == &copy.id)
                        && matches!(s.op, ufm_jobs::StepOp::Delete { .. })
                })
                .unwrap();
            assert!(pos(&copy.id) < pos(&del.id));
        }

        // A lone cross-device file: VerifyCopy -> Copy -> Delete, the
        // Delete carrying the outcome.
        let lone = base.join("lone.bin");
        write_file(&lone, b"lone");
        let job2 =
            plan_move_job_with(&[lone], &dst, LocationId::default(), &|_, _| false)
                .unwrap();
        assert_eq!(job2.steps.len(), 3);
        let del2 = job2
            .steps
            .values()
            .find(|s| matches!(s.op, ufm_jobs::StepOp::Delete { .. }))
            .unwrap();
        assert!(del2.outcome);
        assert_eq!(
            del2.outcome_path.as_deref(),
            Some(dst.join("lone.bin").to_string_lossy().as_ref())
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn delete_plan_trash_is_one_step_per_source() {
        let base = inc2_dir("del-trash");
        let tree = base.join("tree");
        write_file(&tree.join("x.txt"), b"xxx");
        write_file(&tree.join("y.txt"), b"yyyy");
        let lone = base.join("lone.txt");
        write_file(&lone, b"z");
        let job =
            plan_delete_job(&[tree, lone], true, LocationId::default()).unwrap();
        // One Delete step per top-level source — never decomposed.
        assert_eq!(job.steps.len(), 2);
        assert!(job.steps.values().all(|s| matches!(
            s.op,
            ufm_jobs::StepOp::Delete {
                use_trash: true,
                ..
            }
        ) && s.outcome));
        // Preview totals equal the walked fixture.
        assert_eq!(job.preview.files_affected, 3);
        assert_eq!(job.preview.bytes_to_move, 3 + 4 + 1);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn delete_plan_permanent_walks() {
        let base = inc2_dir("del-perm");
        let tree = base.join("tree");
        write_file(&tree.join("a.txt"), b"aaa");
        write_file(&tree.join("sub/b.txt"), b"bbbb");
        std::os::unix::fs::symlink(tree.join("a.txt"), tree.join("link")).unwrap();
        let job =
            plan_delete_job(&[tree.clone()], false, LocationId::default()).unwrap();
        let deletes: Vec<_> = job
            .steps
            .values()
            .filter(|s| {
                matches!(
                    s.op,
                    ufm_jobs::StepOp::Delete {
                        use_trash: false,
                        ..
                    }
                )
            })
            .collect();
        // a.txt, b.txt, the symlink (a leaf — never walked), sub, root.
        assert_eq!(deletes.len(), 5);
        let a_del = find_step(&job, &tree.join("a.txt")).expect("a delete");
        let b_del = find_step(&job, &tree.join("sub/b.txt")).expect("b delete");
        match &a_del.op {
            ufm_jobs::StepOp::Delete { target, .. } => {
                assert_eq!(target.size_bytes, Some(3))
            }
            _ => panic!("expected Delete"),
        }
        match &b_del.op {
            ufm_jobs::StepOp::Delete { target, .. } => {
                assert_eq!(target.size_bytes, Some(4))
            }
            _ => panic!("expected Delete"),
        }
        // Directory Deletes depend on their contents.
        let sub_del = find_step(&job, &tree.join("sub")).expect("sub delete");
        assert_eq!(sub_del.depends_on, vec![b_del.id.clone()]);
        let root_del = find_step(&job, &tree).expect("root delete");
        assert!(root_del.outcome);
        assert_eq!(root_del.depends_on.len(), 4);
        assert!(root_del.depends_on.contains(&sub_del.id));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn exec_move_rename_guards() {
        let base = inc2_dir("exec-move");
        let src = base.join("src.txt");
        let dst = base.join("dst.txt");
        write_file(&src, b"payload");
        // Normal rename.
        assert!(matches!(
            exec_move_rename(&src, &dst),
            StepOutcome::Done(None)
        ));
        assert!(!src.exists());
        assert_eq!(std::fs::read(&dst).unwrap(), b"payload");
        // !src && dst -> Done (the rename landed just before a crash).
        let ghost = base.join("ghost.txt");
        assert!(matches!(
            exec_move_rename(&ghost, &dst),
            StepOutcome::Done(None)
        ));
        // src && dst -> Failed, and the destination is UNCHANGED
        // (the overwrite guard).
        write_file(&src, b"new");
        assert!(matches!(
            exec_move_rename(&src, &dst),
            StepOutcome::Failed(_)
        ));
        assert_eq!(std::fs::read(&dst).unwrap(), b"payload");
        assert_eq!(std::fs::read(&src).unwrap(), b"new");
        // Both missing -> Failed.
        let nowhere = base.join("nowhere.txt");
        assert!(matches!(
            exec_move_rename(&ghost, &nowhere),
            StepOutcome::Failed(_)
        ));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn exec_trash_writes_info_first_and_returns_name() {
        let base = inc2_dir("exec-trash");
        let root = base.join("Trash");
        let work = base.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = work.join("hello.txt");
        write_file(&f, b"hi");
        let name = exec_trash(&f, &root).unwrap();
        assert_eq!(name, "hello.txt");
        assert!(!f.exists());
        assert_eq!(
            std::fs::read(root.join("files/hello.txt")).unwrap(),
            b"hi"
        );
        let info =
            std::fs::read_to_string(root.join("info/hello.txt.trashinfo")).unwrap();
        assert!(info.contains(&format!("Path={}", f.to_string_lossy())));
        assert!(info.contains("DeletionDate="));
        // A second same-named file gets "hello.txt 2".
        let f2 = work.join("hello.txt");
        write_file(&f2, b"hi2");
        let name2 = exec_trash(&f2, &root).unwrap();
        assert_eq!(name2, "hello.txt 2");
        assert_eq!(
            std::fs::read(root.join("files/hello.txt 2")).unwrap(),
            b"hi2"
        );
        // A directory trashes whole, tree intact.
        let d = work.join("folder");
        write_file(&d.join("inner.txt"), b"in");
        let dname = exec_trash(&d, &root).unwrap();
        assert_eq!(dname, "folder");
        assert!(root.join("files/folder/inner.txt").exists());
        assert!(!d.exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn exec_permanent_delete_removes_file_and_empty_dir_only() {
        let base = inc2_dir("exec-perm");
        let f = base.join("f.txt");
        write_file(&f, b"x");
        assert!(matches!(
            exec_permanent_delete(&f),
            StepOutcome::Done(None)
        ));
        assert!(!f.exists());
        let empty = base.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(matches!(
            exec_permanent_delete(&empty),
            StepOutcome::Done(None)
        ));
        assert!(!empty.exists());
        // Non-empty dir -> Failed, contents intact (remove_dir, never
        // remove_dir_all).
        let full = base.join("full");
        write_file(&full.join("keep.txt"), b"keep");
        assert!(matches!(
            exec_permanent_delete(&full),
            StepOutcome::Failed(_)
        ));
        assert_eq!(std::fs::read(full.join("keep.txt")).unwrap(), b"keep");
        // Missing target -> Skipped (absence is the goal state).
        let missing = base.join("missing");
        assert!(matches!(
            exec_permanent_delete(&missing),
            StepOutcome::Skipped
        ));
        let _ = std::fs::remove_dir_all(&base);
    }
}

// ---------------------------------------------------------------------------
// Distro info — feeds the theme engine's Auto detection (same data the
// Linux System Profiler GUI uses: /etc/os-release ID + session desktop).
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
pub struct DistroInfo {
    pub os_id: String,
    pub desktop: String,
}

#[tauri::command]
pub async fn distro_info() -> Cmd<DistroInfo> {
    let mut os_id = String::new();
    if let Ok(text) = std::fs::read_to_string("/etc/os-release") {
        for line in text.lines() {
            if let Some(v) = line.strip_prefix("ID=") {
                os_id = v.trim().trim_matches('"').to_lowercase();
            }
        }
    }
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    Ok(DistroInfo { os_id, desktop })
}

// ---------------------------------------------------------------------------
// Scheduled folder synchronization.
//
// A schedule is a saved (A, B, direction, interval) profile. Saving one
// writes a systemd *user* timer that runs this same binary headless
// (`--run-sync <id>`); the timer fires whether or not the GUI is open.
// Because a timer alone can't start a GUI-less run before login, the
// first schedule also installs an autostart entry — the dialog says so
// in plain language before the user confirms.
// ---------------------------------------------------------------------------

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct SyncSchedule {
    pub id: String,
    pub a: String,
    pub b: String,
    /// "a2b" | "b2a" | "both" (both = newer file wins, per file)
    pub direction: String,
    pub interval_min: u32,
}

fn fable_data_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    std::path::PathBuf::from(home).join(".local/share/fable-files")
}

fn schedules_path() -> std::path::PathBuf {
    fable_data_dir().join("sync-schedules.json")
}

fn load_schedules() -> Vec<SyncSchedule> {
    std::fs::read_to_string(schedules_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn store_schedules(list: &[SyncSchedule]) -> Result<(), String> {
    let dir = fable_data_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let tmp = dir.join("sync-schedules.json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(list).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, schedules_path()).map_err(|e| e.to_string())
}

/// The path systemd should execute: the AppImage (stable across runs),
/// never the transient mount path of the current process.
fn appimage_path() -> String {
    if let Ok(p) = std::env::var("APPIMAGE") {
        if !p.is_empty() {
            return p;
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    format!("{home}/Applications/fable-files.AppImage")
}

fn sync_unit_names(id: &str) -> (String, String) {
    (format!("fable-files-sync-{id}.service"), format!("fable-files-sync-{id}.timer"))
}

fn user_unit_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    std::path::PathBuf::from(home).join(".config/systemd/user")
}

fn write_sync_units(sched: &SyncSchedule) -> Result<(), String> {
    let dir = user_unit_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let (service, timer) = sync_unit_names(&sched.id);
    let exe = appimage_path();
    let service_text = format!(
        "[Unit]\nDescription=Fable Files scheduled sync {id}\n\n[Service]\nType=oneshot\nExecStart=\"{exe}\" --run-sync {id}\n",
        id = sched.id, exe = exe
    );
    let timer_text = format!(
        "[Unit]\nDescription=Run Fable Files sync {id} every {mins} minutes\n\n[Timer]\nOnBootSec=3min\nOnUnitActiveSec={mins}min\nPersistent=true\nUnit={service}\n\n[Install]\nWantedBy=timers.target\n",
        id = sched.id, mins = sched.interval_min, service = service
    );
    std::fs::write(dir.join(&service), service_text).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(&timer), timer_text).map_err(|e| e.to_string())?;
    let _ = systemctl_user(&["daemon-reload"]);
    let _ = systemctl_user(&["enable", "--now", &timer]);
    Ok(())
}

fn remove_sync_units(id: &str) {
    let (service, timer) = sync_unit_names(id);
    let _ = systemctl_user(&["disable", "--now", &timer]);
    let dir = user_unit_dir();
    let _ = std::fs::remove_file(dir.join(&service));
    let _ = std::fs::remove_file(dir.join(&timer));
    let _ = systemctl_user(&["daemon-reload"]);
}

/// Autostart entry exists exactly while at least one schedule does.
fn sync_autostart(schedule_count: usize) {
    let home = std::env::var("HOME").unwrap_or_default();
    let path = std::path::PathBuf::from(home).join(".config/autostart/fable-files.desktop");
    if schedule_count > 0 {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let text = format!(
            "[Desktop Entry]\nType=Application\nName=Fable Files\nComment=Starts with the system so scheduled folder syncs can run\nExec=\"{}\"\nIcon=fable-files\nX-GNOME-Autostart-enabled=true\n",
            appimage_path()
        );
        let _ = std::fs::write(&path, text);
    } else {
        let _ = std::fs::remove_file(&path);
    }
}

#[tauri::command]
pub async fn list_sync_schedules() -> Cmd<Vec<SyncSchedule>> {
    Ok(load_schedules())
}

#[tauri::command]
pub async fn save_sync_schedule(
    a: String,
    b: String,
    direction: String,
    interval_min: u32,
) -> Cmd<SyncSchedule> {
    if a.trim().is_empty() || b.trim().is_empty() || a == b {
        return Err(CommandError::from(UfmError::Internal(
            "pick two different folders".into(),
        )));
    }
    let id = format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    );
    let sched = SyncSchedule {
        id: id.clone(),
        a,
        b,
        direction: if ["a2b", "b2a", "both"].contains(&direction.as_str()) {
            direction
        } else {
            "a2b".into()
        },
        interval_min: interval_min.clamp(5, 24 * 60),
    };
    let mut list = load_schedules();
    list.push(sched.clone());
    store_schedules(&list).map_err(|e| CommandError::from(UfmError::Internal(e)))?;
    write_sync_units(&sched).map_err(|e| CommandError::from(UfmError::Internal(e)))?;
    sync_autostart(list.len());
    Ok(sched)
}

#[tauri::command]
pub async fn delete_sync_schedule(id: String) -> Cmd<()> {
    let mut list = load_schedules();
    list.retain(|x| x.id != id);
    store_schedules(&list).map_err(|e| CommandError::from(UfmError::Internal(e)))?;
    remove_sync_units(&id);
    sync_autostart(list.len());
    Ok(())
}

/// Headless scheduled-sync entry point (`fable-files --run-sync <id>`).
/// Mirrors the dialog's semantics: compare both trees; copy what the
/// direction demands (for "both", the newer copy of each file wins).
/// Appends one line per run to sync.log. Returns a process exit code.
pub fn run_sync_headless(id: &str) -> i32 {
    let list = load_schedules();
    let Some(sched) = list.iter().find(|x| x.id == id) else {
        return 2;
    };
    let a_root = std::path::Path::new(&sched.a);
    let b_root = std::path::Path::new(&sched.b);
    let map_a = scan_tree(a_root);
    let map_b = scan_tree(b_root);
    let mut copied = 0usize;
    let mut errors = 0usize;
    let copy_one = |from_root: &std::path::Path, to_root: &std::path::Path, rel: &str,
                    copied: &mut usize, errors: &mut usize| {
        let from = from_root.join(rel);
        let to = to_root.join(rel);
        if let Some(parent) = to.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::copy(&from, &to) {
            Ok(_) => {
                if let Ok(md) = std::fs::metadata(&from) {
                    if let Ok(mt) = md.modified() {
                        let _ = filetime::set_file_mtime(
                            &to,
                            filetime::FileTime::from_system_time(mt),
                        );
                    }
                }
                *copied += 1;
            }
            Err(_) => *errors += 1,
        }
    };
    let mut rels: std::collections::BTreeSet<&String> = std::collections::BTreeSet::new();
    rels.extend(map_a.keys());
    rels.extend(map_b.keys());
    for rel in rels {
        let in_a = map_a.get(rel);
        let in_b = map_b.get(rel);
        match (sched.direction.as_str(), in_a, in_b) {
            ("a2b", Some(_), _) => {
                let same = matches!((in_a, in_b), (Some((sa, ma)), Some((sb, mb))) if sa == sb && (*ma - *mb).abs() <= 2);
                if !same {
                    copy_one(a_root, b_root, rel, &mut copied, &mut errors);
                }
            }
            ("b2a", _, Some(_)) => {
                let same = matches!((in_a, in_b), (Some((sa, ma)), Some((sb, mb))) if sa == sb && (*ma - *mb).abs() <= 2);
                if !same {
                    copy_one(b_root, a_root, rel, &mut copied, &mut errors);
                }
            }
            ("both", Some((sa, ma)), Some((sb, mb))) => {
                if !(sa == sb && (*ma - *mb).abs() <= 2) {
                    if ma >= mb {
                        copy_one(a_root, b_root, rel, &mut copied, &mut errors);
                    } else {
                        copy_one(b_root, a_root, rel, &mut copied, &mut errors);
                    }
                }
            }
            ("both", Some(_), None) => copy_one(a_root, b_root, rel, &mut copied, &mut errors),
            ("both", None, Some(_)) => copy_one(b_root, a_root, rel, &mut copied, &mut errors),
            _ => {}
        }
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = format!(
        "{stamp} sync {id} ({dir}) {a} <-> {b}: {copied} copied, {errors} errors\n",
        id = sched.id, dir = sched.direction, a = sched.a, b = sched.b
    );
    let dir = fable_data_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("sync.log")) {
        use std::io::Write;
        let _ = f.write_all(line.as_bytes());
    }
    if errors > 0 { 1 } else { 0 }
}

// ---------------------------------------------------------------------------
// Player round tests: window fit, cast parsers, DLNA, media server.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod player_round_tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn player_window_fit_hugs_the_picture() {
        // 2.35:1 (Brave, 1920x816-ish): the screenshot case.
        let (w, h) = player_fit_size(1920, 816);
        assert_eq!((w, h), (640.0, 272.0));
        // 16:9
        assert_eq!(player_fit_size(1920, 1080), (640.0, 360.0));
        // 4:3
        assert_eq!(player_fit_size(1440, 1080), (640.0, 480.0));
        // Portrait phone clip: height clamps, width trades down.
        assert_eq!(player_fit_size(720, 1280), (506.0, 900.0));
        // Ultra-wide: height floor, width stays in band.
        let (w, h) = player_fit_size(3840, 800);
        assert_eq!(h, 200.0);
        assert!(w <= 1280.0);
        // Degenerate metadata falls back to the old default.
        assert_eq!(player_fit_size(0, 0), (560.0, 340.0));
    }

    #[test]
    fn catt_scan_parses_the_real_ip_first_format() {
        // Verbatim shape from a live `catt scan` (catt 0.13.3).
        let out = "\
Scanning Chromecasts...
192.168.12.226 - Natalie Vizio - Vizio VFD40M-0809
192.168.12.157 - Richard tv - Google Cast Group
192.168.12.209 - [LG] webOS GX90SA - LG TV
";
        let devs = parse_catt_scan(out);
        assert_eq!(devs.len(), 3);
        assert_eq!(devs[0].name, "Natalie Vizio");
        assert_eq!(devs[0].address, "192.168.12.226");
        assert_eq!(devs[0].kind, "chromecast");
        assert_eq!(devs[1].name, "Richard tv");
        assert_eq!(devs[2].name, "[LG] webOS GX90SA");
        assert!(parse_catt_scan("").is_empty());
        assert!(parse_catt_scan("no devices found").is_empty());
    }

    #[test]
    fn catt_status_json_parses() {
        let s = parse_catt_status(
            r#"{"player_state":"PLAYING","current_time":61.5,"duration":5610.0,"title":"Brave"}"#,
        );
        assert_eq!(s.state, "playing");
        assert_eq!(s.position, 61.5);
        assert_eq!(s.duration, 5610.0);
        assert_eq!(parse_catt_status("{}").state, "unknown");
        assert_eq!(parse_catt_status("not json").state, "unknown");
    }

    #[test]
    fn atv_status_json_parses() {
        let s = parse_atv_status(
            r#"{"device_state":"paused","position":30,"total_time":600,"media_type":"video"}"#,
        );
        assert_eq!(s.state, "paused");
        assert_eq!(s.position, 30.0);
        assert_eq!(s.duration, 600.0);
        assert_eq!(parse_atv_status("{}").state, "unknown");
    }

    #[test]
    fn hms_round_trips_and_junk_is_zero() {
        assert_eq!(parse_hms("1:33:37"), 5617.0);
        assert_eq!(parse_hms("02:03"), 123.0);
        assert_eq!(parse_hms("90"), 90.0);
        assert_eq!(parse_hms("NOT_IMPLEMENTED"), 0.0);
        assert_eq!(parse_hms("--:--"), 0.0);
        assert_eq!(parse_hms(""), 0.0);
        assert_eq!(hms_of(5617.0), "1:33:37");
        assert_eq!(hms_of(65.0), "0:01:05");
        assert_eq!(parse_hms(&hms_of(3723.0)), 3723.0);
    }

    #[test]
    fn soco_status_line_parses() {
        let s = parse_soco_status("PLAYING|0:01:30|1:33:37");
        assert_eq!(s.state, "playing");
        assert_eq!(s.position, 90.0);
        assert_eq!(s.duration, 5617.0);
        assert_eq!(parse_soco_status("STOPPED||").state, "idle");
    }

    #[test]
    fn url_resolution_and_xml_helpers() {
        assert_eq!(
            resolve_http_url("http://192.168.1.5:8080/desc.xml", "/avt/control"),
            "http://192.168.1.5:8080/avt/control"
        );
        assert_eq!(
            resolve_http_url("http://192.168.1.5:8080/upnp/desc.xml", "control"),
            "http://192.168.1.5:8080/upnp/control"
        );
        assert_eq!(
            resolve_http_url("http://192.168.1.5:8080", "control"),
            "http://192.168.1.5:8080/control"
        );
        assert_eq!(
            resolve_http_url("http://h/d.xml", "http://other/x"),
            "http://other/x"
        );
        assert_eq!(
            xml_tag_text("<a><friendlyName>TV &amp; Co</friendlyName></a>", "friendlyName"),
            Some("TV & Co".to_string())
        );
        assert_eq!(xml_tag_text("<a/>", "missing"), None);
    }

    #[test]
    fn dlna_description_parses_and_requires_avtransport() {
        let xml = r#"<?xml version="1.0"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
 <device>
  <friendlyName>Living Room TV</friendlyName>
  <serviceList>
   <service>
    <serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType>
    <controlURL>/upnp/control/AVTransport1</controlURL>
   </service>
  </serviceList>
 </device>
</root>"#;
        let (name, url) =
            parse_dlna_description(xml, "http://192.168.1.9:9197/dmr.xml").unwrap();
        assert_eq!(name, "Living Room TV");
        assert_eq!(url, "http://192.168.1.9:9197/upnp/control/AVTransport1");
        // No AVTransport service → not a renderer we can drive.
        let plain = "<root><device><friendlyName>Printer</friendlyName></device></root>";
        assert!(parse_dlna_description(plain, "http://h/d.xml").is_none());
    }

    #[test]
    fn ssdp_location_header_parses() {
        let resp = "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=1800\r\nLOCATION: http://192.168.1.9:9197/dmr.xml\r\nST: urn:schemas-upnp-org:device:MediaRenderer:1\r\n\r\n";
        assert_eq!(
            parse_ssdp_location(resp),
            Some("http://192.168.1.9:9197/dmr.xml".to_string())
        );
        assert_eq!(parse_ssdp_location("HTTP/1.1 200 OK\r\n\r\n"), None);
    }

    #[test]
    fn didl_carries_title_mime_and_url() {
        let d = dlna_didl("http://192.168.1.2:9/cast/abc", "Brave (2012).mkv", "video/x-matroska");
        assert!(d.contains("Brave (2012).mkv"));
        assert!(d.contains("http-get:*:video/x-matroska:*"));
        assert!(d.contains("http://192.168.1.2:9/cast/abc"));
    }

    /// A mock DLNA renderer on loopback: proves the SOAP
    /// envelope/action headers go out right and the status
    /// parsers read a real response — the full control path
    /// short of a physical TV.
    #[test]
    fn dlna_soap_round_trip_against_mock_renderer() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen2 = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { break };
                let seen3 = seen2.clone();
                std::thread::spawn(move || {
                    let mut req = Vec::new();
                    let mut buf = [0u8; 4096];
                    // Read headers, then the body per Content-Length.
                    let mut body_at = None;
                    let mut clen = 0usize;
                    while body_at.is_none() {
                        match s.read(&mut buf) {
                            Ok(0) => return,
                            Ok(n) => {
                                req.extend_from_slice(&buf[..n]);
                                if let Some(pos) = find_subslice(&req, b"\r\n\r\n") {
                                    body_at = Some(pos + 4);
                                    let head = String::from_utf8_lossy(&req[..pos]).to_lowercase();
                                    for line in head.lines() {
                                        if let Some(v) = line.strip_prefix("content-length:") {
                                            clen = v.trim().parse().unwrap_or(0);
                                        }
                                    }
                                }
                            }
                            Err(_) => return,
                        }
                    }
                    let body_at = body_at.unwrap();
                    while req.len() < body_at + clen {
                        match s.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => req.extend_from_slice(&buf[..n]),
                            Err(_) => break,
                        }
                    }
                    let text = String::from_utf8_lossy(&req).into_owned();
                    seen3.lock().unwrap().push(text.clone());
                    let reply = if text.contains("GetTransportInfo") {
                        "<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><u:GetTransportInfoResponse xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\"><CurrentTransportState>PLAYING</CurrentTransportState></u:GetTransportInfoResponse></s:Body></s:Envelope>"
                    } else if text.contains("GetPositionInfo") {
                        "<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><u:GetPositionInfoResponse xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\"><TrackDuration>1:33:37</TrackDuration><RelTime>0:01:30</RelTime></u:GetPositionInfoResponse></s:Body></s:Envelope>"
                    } else {
                        "<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body/></s:Envelope>"
                    };
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/xml; charset=\"utf-8\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        reply.len(),
                        reply
                    );
                    let _ = s.write_all(resp.as_bytes());
                });
            }
        });
        let control = format!("http://127.0.0.1:{port}/avt");
        // Play goes out as a proper SOAP action…
        dlna_soap(&control, "Play", "<InstanceID>0</InstanceID><Speed>1</Speed>").unwrap();
        // …and status reads state + position back.
        let st = dlna_status(&control).unwrap();
        assert_eq!(st.state, "playing");
        assert_eq!(st.position, 90.0);
        assert_eq!(st.duration, 5617.0);
        let reqs = seen.lock().unwrap();
        assert!(reqs.iter().any(|r| r.contains("SOAPACTION") || r.to_lowercase().contains("soapaction")));
        assert!(reqs.iter().any(|r| r.contains("#Play")));
        assert!(reqs.iter().any(|r| r.contains("<u:Play")));
    }

    fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
        hay.windows(needle.len()).position(|w| w == needle)
    }

    /// The real media server over a real socket: full GET carries
    /// the file's media type (the cast fix), Range GET returns the
    /// exact byte slice with 206 + Content-Range, unknown tokens
    /// 404. This is the server casting streams through.
    #[test]
    fn media_server_serves_ranges_with_media_type() {
        use std::net::TcpListener;
        let dir = std::env::temp_dir().join(format!("fable-media-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("clip.mp4");
        let bytes: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&file, &bytes).unwrap();
        let files = std::sync::Arc::new(std::sync::Mutex::new(
            std::collections::HashMap::from([("tok1".to_string(), file.clone())]),
        ));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let f = files.clone();
                std::thread::spawn(move || serve_media_conn(stream, f));
            }
        });
        let get = |req: &str| -> Vec<u8> {
            let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            s.write_all(req.as_bytes()).unwrap();
            let mut out = Vec::new();
            s.read_to_end(&mut out).unwrap();
            out
        };
        let split = |resp: &[u8]| -> (String, Vec<u8>) {
            let pos = find_subslice(resp, b"\r\n\r\n").unwrap();
            (
                String::from_utf8_lossy(&resp[..pos]).into_owned(),
                resp[pos + 4..].to_vec(),
            )
        };

        let (head, body) = split(&get("GET /cast/tok1 HTTP/1.1\r\nHost: x\r\n\r\n"));
        assert!(head.contains("200"), "{head}");
        assert!(head.to_lowercase().contains("content-type: video/mp4"), "{head}");
        assert_eq!(body, bytes);

        let (head, body) = split(&get(
            "GET /cast/tok1 HTTP/1.1\r\nHost: x\r\nRange: bytes=100-199\r\n\r\n",
        ));
        assert!(head.contains("206"), "{head}");
        assert!(head.contains("Content-Range: bytes 100-199/4096"), "{head}");
        assert_eq!(body, bytes[100..200].to_vec());

        let (head, _) = split(&get("GET /cast/nope HTTP/1.1\r\nHost: x\r\n\r\n"));
        assert!(head.contains("404"), "{head}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
