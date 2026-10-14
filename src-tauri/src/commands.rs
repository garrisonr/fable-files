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
    /// volumes carry it so the shell can eject by path).
    pub path: Option<String>,
    /// Removable media (USB / SD card) — the sidebar offers eject.
    pub removable: bool,
}

// ---------------------------------------------------------------------------
// Mounted volumes (Round 7.2): the sidebar Locations list used to be only
// the persisted set — a freshly plugged drive never appeared. Volumes are
// now enumerated from lsblk on every list_locations call and merged in as
// provider="volume" runtime locations (registered in the browse maps so
// list_dir resolves them, never written to the catalog or the sidecar).
// ---------------------------------------------------------------------------

struct MountedVolume {
    name: String,
    path: String,
    removable: bool,
}

/// One mounted-filesystem candidate collected from the lsblk tree.
struct MountCandidate {
    device: String,
    mountpoint: String,
    label: Option<String>,
    removable: bool,
}

fn collect_mounts(
    node: &serde_json::Value,
    inherited_rm: bool,
    inherited_usb: bool,
    under_loop: bool,
    out: &mut Vec<MountCandidate>,
) {
    let ty = node.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let rm = node
        .get("rm")
        .and_then(|v| v.as_bool())
        .unwrap_or(inherited_rm);
    let usb = node.get("tran").and_then(|v| v.as_str()) == Some("usb") || inherited_usb;
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
                    removable: rm || usb,
                });
            }
        }
    }
    if let Some(children) = node.get("children").and_then(|v| v.as_array()) {
        for child in children {
            collect_mounts(child, rm, usb, loopish, out);
        }
    }
}

fn enumerate_mounted_volumes() -> Vec<MountedVolume> {
    let out = match std::process::Command::new("lsblk")
        .args([
            "-J",
            "-o",
            "NAME,TYPE,RM,TRAN,MOUNTPOINTS,LABEL,FSTYPE",
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
            collect_mounts(d, false, false, false, &mut candidates);
        }
    }
    // Drop system / pseudo mounts: the sidebar is for browsable volumes.
    candidates.retain(|c| {
        let mp = c.mountpoint.as_str();
        mp != "/"
            && mp != "/boot"
            && mp != "/boot/efi"
            && !mp.starts_with("/snap/")
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
    let mut volumes: Vec<MountedVolume> = by_device
        .into_values()
        .map(|c| {
            let name = c.label.clone().unwrap_or_else(|| {
                std::path::Path::new(&c.mountpoint)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| c.mountpoint.clone())
            });
            MountedVolume {
                name,
                path: c.mountpoint,
                removable: c.removable,
            }
        })
        .collect();
    volumes.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    volumes
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
pub async fn list_locations(state: State<'_, AppState>) -> Cmd<Vec<LocationSummary>> {
    let volumes = tauri::async_runtime::spawn_blocking(enumerate_mounted_volumes)
        .await
        .unwrap_or_default();
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
            });
        }
    }

    Ok(state.locations.lock().unwrap().clone())
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

fn job_state_name(state: ufm_jobs::JobState) -> &'static str {
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
fn job_bytes_done(job: &ufm_jobs::Job) -> u64 {
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

fn job_item(
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

/// Record one step transition everywhere it must land: the in-memory
/// snapshot, the durable journal, and the jobs-table snapshot.
/// Inc-2: when an outcome step (§2.2) finishes, its `JobOutcome` is
/// appended under the same lock, before the upsert, so the outcome is
/// durable in the same snapshot write as the transition itself.
fn transition_step(
    handle: &JobHandle,
    catalog: &ufm_catalog::Catalog,
    journal: &mut ufm_jobs::JobJournal,
    step_id: &str,
    to: ufm_jobs::StepState,
    note: Option<String>,
) -> ufm_jobs::Job {
    let (snap, from) = {
        let mut j = handle.job.lock().unwrap();
        let from = j
            .steps
            .get(step_id)
            .map(|s| s.state)
            .unwrap_or(ufm_jobs::StepState::Pending);
        if let Some(s) = j.steps.get_mut(step_id) {
            s.state = to;
        }
        if matches!(
            to,
            ufm_jobs::StepState::Done | ufm_jobs::StepState::Skipped
        ) {
            let outcome = j.steps.get(step_id).and_then(|s| {
                if s.outcome && !j.outcomes.iter().any(|o| o.step_id == step_id) {
                    Some(build_outcome(s, note.as_deref()))
                } else {
                    None
                }
            });
            if let Some(o) = outcome {
                j.outcomes.push(o);
            }
        }
        (j.clone(), from)
    };
    journal.record_with_note(snap.id, step_id, from, to, note);
    let _ = catalog.upsert_job(&snap);
    snap
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
    job_snapshot: &ufm_jobs::Job,
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
                            emit_job_progress(
                                app,
                                job_snapshot,
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
    }
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
                let snap = transition_step(
                    &handle,
                    &catalog,
                    &mut journal,
                    step_id,
                    ufm_jobs::StepState::Skipped,
                    None,
                );
                bytes_done = job_bytes_done(&snap);
                emit_job_progress(&app, &snap, bytes_done, None, None);
                continue;
            }
        }
        let snap = transition_step(
            &handle,
            &catalog,
            &mut journal,
            step_id,
            ufm_jobs::StepState::Running,
            None,
        );
        emit_job_progress(&app, &snap, bytes_done, Some(&step.label), None);
        let outcome = execute_step(&step.op, &handle, &app, &snap, &mut bytes_done);
        match outcome {
            StepOutcome::Done(note) => {
                let snap = transition_step(
                    &handle,
                    &catalog,
                    &mut journal,
                    step_id,
                    ufm_jobs::StepState::Done,
                    note,
                );
                bytes_done = job_bytes_done(&snap);
                emit_job_progress(&app, &snap, bytes_done, None, None);
            }
            StepOutcome::Skipped => {
                let snap = transition_step(
                    &handle,
                    &catalog,
                    &mut journal,
                    step_id,
                    ufm_jobs::StepState::Skipped,
                    None,
                );
                bytes_done = job_bytes_done(&snap);
                emit_job_progress(&app, &snap, bytes_done, None, None);
            }
            StepOutcome::Failed(msg) => {
                transition_step(
                    &handle,
                    &catalog,
                    &mut journal,
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

/// The shared tail of every `start_*` command (inc-2 §2.1):
/// journal-first upsert → register the handle → spawn the executor.
/// The snapshot is durable before any step runs, so even a crash
/// mid-operation leaves a resumable record.
fn start_job(
    app: tauri::AppHandle,
    state: &AppState,
    job: ufm_jobs::Job,
) -> Cmd<JobId> {
    let id = job.id;
    state.catalog.lock().unwrap().upsert_job(&job)?;
    state
        .jobs
        .lock()
        .unwrap()
        .insert(id, JobHandle::new(job, false));
    spawn_executor(app, state, id);
    Ok(id)
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
) -> Cmd<JobId> {
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
) -> Cmd<JobId> {
    let (srcs, location) = resolve_source_paths(&state, &req.sources)?;
    let job = plan_move_job(&srcs, Path::new(&req.dst_dir), location)?;
    start_job(app, &state, job)
}

#[tauri::command]
pub async fn start_move_from_paths(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    req: MovePathsRequest,
) -> Cmd<JobId> {
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
) -> Cmd<JobId> {
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
) -> Cmd<JobId> {
    let (srcs, location) = resolve_source_paths(&state, &req.sources)?;
    let job = plan_delete_job(&srcs, req.use_trash, location)?;
    start_job(app, &state, job)
}

#[tauri::command]
pub async fn start_delete_from_paths(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    req: DeletePathsRequest,
) -> Cmd<JobId> {
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
        .map(|j| JobSummary {
            job_id: j.id.to_string(),
            title: j.title.clone(),
            state: job_state_name(j.state).to_string(),
            steps_done: j
                .steps
                .values()
                .filter(|s| {
                    matches!(
                        s.state,
                        ufm_jobs::StepState::Done | ufm_jobs::StepState::Skipped
                    )
                })
                .count(),
            steps_total: j.steps.len(),
            bytes_done: job_bytes_done(j),
            bytes_total: j.preview.bytes_to_move,
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
    handle
        .pause
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let snap = {
        let mut j = handle.job.lock().unwrap();
        if matches!(
            j.state,
            ufm_jobs::JobState::Running | ufm_jobs::JobState::Queued
        ) {
            j.state = ufm_jobs::JobState::Paused;
        }
        j.clone()
    };
    state.catalog.lock().unwrap().upsert_job(&snap)?;
    emit_job_progress(&app, &snap, job_bytes_done(&snap), None, None);
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
        // No executor to finalize it (e.g. a recovered job): cancel
        // in place.
        let snap = {
            let mut j = handle.job.lock().unwrap();
            j.state = ufm_jobs::JobState::Cancelled;
            j.clone()
        };
        state.catalog.lock().unwrap().upsert_job(&snap)?;
        emit_job_progress(&app, &snap, job_bytes_done(&snap), None, None);
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
}

/// File-type categories for the advanced-search filter.
fn type_extensions(filter: &str) -> Option<&'static [&'static str]> {
    match filter {
        "image" => Some(&["png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "svg", "ico", "avif"][..]),
        "video" => Some(&["mp4", "mkv", "webm", "mov", "avi", "m4v", "mpg", "mpeg"][..]),
        "audio" => Some(&["mp3", "flac", "ogg", "oga", "wav", "m4a", "m4b", "opus", "aac"][..]),
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
    finish_search(state, rows, scope, path_prefix.as_deref(), modified_after.as_deref(), modified_before.as_deref())
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
        let extracted = tauri::async_runtime::spawn_blocking(move || {
            extract_document_text(&doc_path)
        })
        .await;
        match extracted {
            Ok(Ok(text)) if !text.trim().is_empty() => {
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
        if !is_image && !is_audio {
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
        use std::io::{Read, Write};
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
        let mut child = std::process::Command::new("wl-copy")
            .arg("--type")
            .arg(mime)
            .env_remove("LD_LIBRARY_PATH")
            .stdin(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not start wl-copy: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(data.as_bytes())
                .map_err(|e| format!("wl-copy write: {e}"))?;
        }
        // wl-copy forks a server and the parent exits promptly; a
        // nonzero exit is a REAL failure and must reach the user.
        let status = child.wait().map_err(|e| format!("wl-copy wait: {e}"))?;
        if !status.success() {
            let mut err_text = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                let _ = stderr.read_to_string(&mut err_text);
            }
            return Err(format!(
                "wl-copy exited {status}: {}",
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

/// Read file paths from the Wayland clipboard (wl-paste).
#[tauri::command]
pub async fn clipboard_read() -> Cmd<ClipboardData> {
    tauri::async_runtime::spawn_blocking(move || {
        let read_type = |mime: &str| -> Option<String> {
            let out = std::process::Command::new("wl-paste")
                .arg("--type")
                .arg(mime)
                .arg("--no-newline")
                .env_remove("LD_LIBRARY_PATH")
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
}

#[derive(serde::Serialize)]
pub struct RuleOutcome {
    pub label: String,
    pub matched: u32,
    pub added: u32,
    pub remaining: u32,
}

const MEDIA_EXTS: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "m4v", "webm", "mpg", "mpeg", "wmv", "ts", "m2ts",
    "mp3", "aac", "flac", "ogg", "oga", "opus", "wav", "m4a", "m4b", "aif", "aiff", "wma",
];

/// (ffprobe ran, duration if readable)
fn probe_duration(path: &std::path::Path) -> (bool, Option<f64>) {
    let out = std::process::Command::new("ffprobe")
        .args([
            "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0",
        ])
        .arg(path)
        .output();
    match out {
        Ok(o) => {
            let text = String::from_utf8_lossy(&o.stdout);
            (true, text.trim().parse::<f64>().ok().filter(|d| *d > 0.0))
        }
        Err(_) => (false, None),
    }
}

fn eval_rule(
    catalog: &ufm_catalog::Catalog,
    exts: &[String],
    min_duration_secs: Option<f64>,
    probe_budget: u32,
) -> std::result::Result<(Vec<Item>, u32, u32), CommandError> {
    let candidates = catalog
        .rule_candidates(exts)
        .map_err(CommandError::from)?;
    let mut probed = 0u32;
    let mut remaining = 0u32;
    let mut matched = Vec::new();
    for (item, dur) in candidates {
        let Some(pk) = item.provider_key.clone() else {
            continue;
        };
        let mut d = dur;
        if d.is_none() && min_duration_secs.is_some() {
            let ext = pk.rsplit('.').next().unwrap_or("").to_lowercase();
            if MEDIA_EXTS.contains(&ext.as_str()) {
                if probed < probe_budget {
                    probed += 1;
                    let (ran, secs) = probe_duration(std::path::Path::new(&pk));
                    if ran {
                        let _ = catalog.set_duration(&pk, secs.unwrap_or(0.0));
                        d = secs;
                    } else {
                        remaining += 1;
                    }
                } else {
                    remaining += 1;
                }
            }
        }
        let keep = match min_duration_secs {
            None => true,
            Some(min) => d.map(|x| x > min).unwrap_or(false),
        };
        if keep {
            matched.push(item);
        }
    }
    Ok((matched, probed, remaining))
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

#[tauri::command]
pub async fn rule_items(
    state: State<'_, AppState>,
    extensions: Vec<String>,
    min_duration_secs: Option<f64>,
    limit: Option<u32>,
    probe_budget: Option<u32>,
) -> Cmd<RuleItemsResult> {
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    let (mut items, probed, remaining) = eval_rule(
        &catalog,
        &extensions,
        min_duration_secs,
        probe_budget.unwrap_or(250),
    )?;
    items.truncate(limit.unwrap_or(1000) as usize);
    let items = remap_session_items(state.inner(), items);
    Ok(RuleItemsResult {
        items,
        probed,
        remaining,
    })
}

#[tauri::command]
pub async fn apply_label_rules(
    state: State<'_, AppState>,
    rules: Vec<LabelRule>,
) -> Cmd<Vec<RuleOutcome>> {
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    let mut outcomes = Vec::new();
    for rule in &rules {
        let (items, _probed, remaining) = eval_rule(
            &catalog,
            &rule.extensions,
            rule.min_duration_secs,
            250,
        )?;
        let mut added = 0u32;
        for item in &items {
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
            matched: items.len() as u32,
            added,
            remaining,
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
fn rom_source_label(sys: &str) -> &'static str {
    match sys {
        "arcade" | "neogeo" => "mame",
        "3do" | "dreamcast" | "gamecube" | "psx" | "ps2" | "ps3" | "saturn" | "segacd"
        | "wii" | "xbox" | "xbox360" => "redump",
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
        Ok(r) => r,
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
        let identified = ufm_media::romdat::identify_file(&ds, &path);
        let (title, platform, sha1, evidence) = match identified {
            Ok(Some(id)) => {
                stats.matched += 1;
                let ev = serde_json::json!({
                    "matched": true,
                    "crc": id.crc,
                    "md5": id.md5_hex(),
                    "size": size,
                    "modified": modified,
                    "source": rom_source_label(&id.sys),
                    "container": id.container,
                    "also": id.also,
                });
                (
                    Some(id.dat_name.clone()),
                    Some(id.sys.clone()),
                    Some(id.sha1_hex()),
                    ev,
                )
            }
            Ok(None) => {
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
        let printable_path;
        let _temp_dir;
        if office.contains(&ext.as_str()) {
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

/// Install mktorrent via pacman (polkit prompt).
#[tauri::command]
pub async fn install_mktorrent() -> Cmd<()> {
    tauri::async_runtime::spawn_blocking(move || {
        let out = std::process::Command::new("pkexec")
            .args(["pacman", "-S", "--noconfirm", "--needed", "mktorrent"])
            .output()
            .map_err(|e| format!("pkexec: {e}"))?;
        if out.status.success() {
            Ok::<(), String>(())
        } else {
            Err("install failed (or the password prompt was cancelled)".to_string())
        }
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

#[cfg(test)]
mod tests {
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
