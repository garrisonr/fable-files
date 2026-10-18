//! Emulation glue (SPEC-AMENDMENT "Emulation", Architecture A):
//! the machine-touching half of Play. The pure half — the mined
//! systems table, option states, the sheet decision, template
//! expansion, the unified view's math — lives in `ufm_emu` and is
//! unit-tested there. This module probes what's installed, installs
//! what's missing (dependency law: at first Play, in context, never
//! a dead end), launches the real file, and runs the alias keeper
//! (see `emu_farm`).
//!
//! Channels (amendment §4): RetroArch arrives by Flatpak user
//! install first (no password, every distro), falling back to apt
//! via deps.rs where Flatpak is unavailable. Cores are single `.so`
//! files fetched from the libretro buildbot into the user cores
//! directory of the detected RetroArch flavor — no RetroArch UI is
//! ever driven.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};
use ufm_emu::expand::LaunchVars;
use ufm_emu::resolve::with_bios;
use ufm_emu::{
    decide, expand_template, option_state, Decision, EmuOption, EnvProbe, OptState, OptionKind,
    RaFlavor, SystemEntry,
};

use crate::commands::{Cmd, CommandError};
use crate::AppState;
use ufm_core::UfmError;

// ---------------------------------------------------------------------------
// Settings — emu-settings.json beside rom-media.json (same atomic
// write discipline). The per-system emulator choice lives here
// (Preferences → ROMs owns the record, amendment §6).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EmuSettings {
    /// platform (catalog key) -> chosen option id.
    pub choices: BTreeMap<String, String>,
    /// Keep the unified view (~/ROMs farm + RetroArch playlists)
    /// reconciled. Default ON — Dickles ordered the magic aliases.
    pub view_enabled: bool,
    /// Unified-view root. Empty = ~/ROMs (ES-DE's default, so the
    /// future ES-DE door needs zero path configuration).
    pub view_root: String,
    /// Sandbox drive-access grants Fable applied with the man's
    /// consent (amendment §4.4, v2) — visible and removable in
    /// Preferences → ROMs.
    #[serde(default)]
    pub overrides: Vec<SandboxOverride>,
    /// The ES-DE companion Fable installed (v2): its version +
    /// the sha256 the publisher published for the exact file, so
    /// later launches can tell the installed AppImage is still
    /// the one Fable fetched.
    #[serde(default)]
    pub esde_version: String,
    #[serde(default)]
    pub esde_sha256: String,
    /// Emulation v2.1: the fleet-prep record — what the background
    /// prep pass has done, per item, so a restarted pass re-probes
    /// and skips what's already Ready instead of starting over.
    #[serde(default)]
    pub prep: PrepState,
    /// Emulation v2.1: the last update check's findings (the
    /// Preferences card + door badge render this record; checks
    /// never auto-apply).
    #[serde(default)]
    pub updates: UpdateState,
    /// Emulation v2.1: per-core buildbot baselines — the zip facts
    /// (length + Last-Modified) recorded when a core was fetched,
    /// compared against a HEAD at check time.
    #[serde(default)]
    pub core_records: BTreeMap<String, CoreRecord>,
    /// Emulation v2.1: per-AppImage baselines for the GitHub
    /// channel — the release asset Fable installed, so a newer
    /// asset name (or a different published digest) reads as an
    /// update rather than a mystery.
    #[serde(default)]
    pub app_records: BTreeMap<String, AppImageRecord>,
}

/// The prep pass's memory (v2.1): when it last completed, and the
/// last known state of each fleet item it has touched.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PrepState {
    /// Unix seconds of the last completed pass (0 = never).
    pub last_run: i64,
    /// Fleet item key -> last recorded outcome.
    pub items: BTreeMap<String, PrepItemState>,
}

/// One fleet item's recorded prep outcome.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PrepItemState {
    /// "ready" | "failed".
    pub state: String,
    /// Unix seconds of the outcome.
    pub at: i64,
    /// The failure, in the words the installer produced (ready
    /// items carry an empty string, unserialized).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub error: String,
}

/// The last update check's record (v2.1).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UpdateState {
    /// Unix seconds of the check (0 = never checked).
    pub checked_at: i64,
    pub items: Vec<UpdateItem>,
}

/// One available emulator update, as the card lists it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateItem {
    /// Apply key: "core:<core_file>" | "flatpak:<app_id>" |
    /// "appimage:<file_name>" | "esde".
    pub key: String,
    /// Human label ("RetroArch", "Mesen core", "ES-DE").
    pub label: String,
    /// One plain line about what's waiting.
    pub detail: String,
}

/// Buildbot facts recorded when Fable fetched a core (v2.1).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CoreRecord {
    pub len: u64,
    pub modified: String,
    pub at: i64,
}

/// What Fable installed through the GitHub AppImage channel
/// (v2.1): the release asset's name carries its version.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppImageRecord {
    pub asset: String,
    pub sha256: String,
    pub at: i64,
}

/// One recorded sandbox grant: this flatpak app may read this
/// filesystem root (`flatpak override --user --filesystem=…`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SandboxOverride {
    pub app_id: String,
    pub filesystem: String,
}

impl Default for EmuSettings {
    fn default() -> Self {
        Self {
            choices: BTreeMap::new(),
            view_enabled: true,
            view_root: String::new(),
            overrides: Vec::new(),
            esde_version: String::new(),
            esde_sha256: String::new(),
            prep: PrepState::default(),
            updates: UpdateState::default(),
            core_records: BTreeMap::new(),
            app_records: BTreeMap::new(),
        }
    }
}

impl EmuSettings {
    pub fn view_root_path(&self) -> PathBuf {
        if self.view_root.trim().is_empty() {
            default_view_root()
        } else {
            PathBuf::from(&self.view_root)
        }
    }
}

pub fn default_view_root() -> PathBuf {
    home_dir().join("ROMs")
}

pub fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

pub fn settings_path(base: &Path) -> PathBuf {
    base.join("emu-settings.json")
}

pub fn load_settings(base: &Path) -> EmuSettings {
    let path = settings_path(base);
    let mut s: EmuSettings = std::fs::read(&path)
        .ok()
        .and_then(|d| serde_json::from_slice(&d).ok())
        .unwrap_or_default();
    if s.view_root.trim().is_empty() {
        s.view_root = default_view_root().to_string_lossy().into_owned();
    }
    s
}

pub fn save_settings(base: &Path, settings: &EmuSettings) -> Result<(), String> {
    let path = settings_path(base);
    let tmp = base.join("emu-settings.json.part");
    let data = serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, data).map_err(|e| format!("write emu settings: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("save emu settings: {e}"))?;
    Ok(())
}

/// Catalog platform key -> ES-DE system id (the resolution table's
/// vocabulary). rommedia's SystemMap already carries the mapping
/// the media pipeline verified against ES-DE's own file.
pub fn esde_id(catalog_platform: &str) -> String {
    crate::rommedia::system_map(catalog_platform)
        .map(|m| m.esde.to_string())
        .unwrap_or_else(|| catalog_platform.to_string())
}

// ---------------------------------------------------------------------------
// Probing — what emulators/cores does this machine actually have.
// ---------------------------------------------------------------------------

/// Where a RetroArch flavor keeps its user data.
#[derive(Debug, Clone)]
pub struct RaLayout {
    pub flavor: RaFlavor,
    pub config_dir: PathBuf,
    pub cores_dir: PathBuf,
    pub playlists_dir: PathBuf,
    /// The command words that run RetroArch ("retroarch", or
    /// "flatpak run org.libretro.RetroArch").
    pub command: Vec<String>,
}

pub fn ra_layout(flavor: RaFlavor) -> RaLayout {
    match flavor {
        RaFlavor::Flatpak => {
            let config = home_dir()
                .join(".var/app/org.libretro.RetroArch/config/retroarch");
            RaLayout {
                flavor,
                cores_dir: config.join("cores"),
                playlists_dir: config.join("playlists"),
                config_dir: config,
                command: vec![
                    "flatpak".into(),
                    "run".into(),
                    "org.libretro.RetroArch".into(),
                ],
            }
        }
        RaFlavor::Native => {
            let config = home_dir().join(".config/retroarch");
            RaLayout {
                flavor,
                cores_dir: config.join("cores"),
                playlists_dir: config.join("playlists"),
                config_dir: config,
                command: vec!["retroarch".into()],
            }
        }
    }
}

/// System core directories (apt `libretro-*` packages land here) —
/// presence in any of them counts; Fable's launches pass the core
/// by absolute path from the user dir regardless.
const SYSTEM_CORE_DIRS: [&str; 3] = [
    "/usr/lib/x86_64-linux-gnu/libretro",
    "/usr/lib/libretro",
    "/usr/lib64/libretro",
];

pub fn run_stdout(cmd: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let out = child.wait_with_output().ok()?;
                return Some(String::from_utf8_lossy(&out.stdout).into_owned());
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(60));
            }
            Err(_) => return None,
        }
    }
}

fn flatpak_has_app(app_id: &str) -> bool {
    flatpak_app_ids().contains(app_id)
}

/// Every installed flatpak app id, one list call (v2: probing a
/// dozen options must not shell out per option).
fn flatpak_app_ids() -> BTreeSet<String> {
    run_stdout(
        "flatpak",
        &["list", "--app", "--columns=application"],
        Duration::from_secs(20),
    )
    .map(|s| s.lines().map(|l| l.trim().to_string()).collect())
    .unwrap_or_default()
}

/// Fable's managed AppImage home (the same ~/Applications the app
/// itself ships into).
pub fn applications_dir() -> PathBuf {
    home_dir().join("Applications")
}

/// The managed AppImage for a file name, when it is really there.
pub fn managed_appimage(file_name: &str) -> Option<PathBuf> {
    let p = applications_dir().join(file_name);
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

/// Does this option's install channel already have its artifact
/// on the machine? (Flatpak app installed / managed AppImage
/// present.) Presence is what turns an install spec from a
/// promise into a launchable thing.
fn channel_present(opt: &EmuOption, flatpak_apps: &BTreeSet<String>) -> bool {
    match &opt.install {
        ufm_emu::InstallSpec::Flatpak { app_id } => flatpak_apps.contains(app_id),
        ufm_emu::InstallSpec::AppImage { file_name, .. }
        | ufm_emu::InstallSpec::GitHubAppImage { file_name, .. } => {
            managed_appimage(file_name).is_some()
        }
        _ => false,
    }
}

fn flatpak_available() -> bool {
    if !crate::deps::tool_present("flatpak") {
        return false;
    }
    run_stdout("flatpak", &["remotes", "--columns=name"], Duration::from_secs(20))
        .map(|s| s.lines().any(|l| l.trim() == "flathub"))
        .unwrap_or(false)
}

/// Probe the machine. Returns the probe + the layout of the
/// preferred RetroArch flavor when one exists.
pub fn probe_env() -> (EnvProbe, Option<RaLayout>) {
    let flatpak_ra = crate::deps::tool_present("flatpak") && flatpak_has_app("org.libretro.RetroArch");
    let native_ra = crate::deps::tool_present("retroarch");
    // Flatpak wins when both exist (amendment §4.1 — it's the
    // channel Fable installs, so its dirs are where Fable's cores
    // and playlists belong).
    let flavor = if flatpak_ra {
        Some(RaFlavor::Flatpak)
    } else if native_ra {
        Some(RaFlavor::Native)
    } else {
        None
    };
    let layout = flavor.map(ra_layout);

    let mut cores: BTreeSet<String> = BTreeSet::new();
    let mut core_dirs: Vec<PathBuf> = SYSTEM_CORE_DIRS.iter().map(PathBuf::from).collect();
    if let Some(l) = &layout {
        core_dirs.push(l.cores_dir.clone());
    } else {
        // No RA yet: still report cores sitting in either flavor's
        // user dir (a half-removed install, a hand-placed core).
        core_dirs.push(ra_layout(RaFlavor::Flatpak).cores_dir);
        core_dirs.push(ra_layout(RaFlavor::Native).cores_dir);
    }
    for dir in &core_dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.ends_with(".so") {
                    cores.insert(name);
                }
            }
        }
    }

    // Standalone presence: each curated option's own binaries on
    // PATH, or (v2) the artifact of the channel Fable installs —
    // an installed flatpak app or a managed AppImage answers for
    // emulators whose binaries don't live on PATH at all.
    let flatpak_apps = flatpak_app_ids();
    let mut standalones: BTreeSet<String> = BTreeSet::new();
    for sys in ufm_emu::systems_table() {
        for opt in &sys.options {
            if opt.kind != OptionKind::Standalone {
                continue;
            }
            if opt
                .probe_bins
                .iter()
                .any(|b| crate::deps::tool_present(b))
                || channel_present(opt, &flatpak_apps)
            {
                standalones.insert(opt.id.clone());
            }
        }
    }

    (
        EnvProbe {
            ra: flavor,
            cores,
            standalones,
            flatpak_available: flatpak_available(),
        },
        layout,
    )
}

/// Legacy BIOS presence for an option (mined systems only): every
/// file on the option's own list exists in the RetroArch system
/// directory. Curated systems go through the requirement table —
/// see `state_of`.
pub fn bios_present(opt: &EmuOption, layout: Option<&RaLayout>) -> bool {
    if opt.bios.is_empty() {
        return true;
    }
    let Some(layout) = layout else {
        return false;
    };
    let system_dir = layout.config_dir.join("system");
    opt.bios.iter().all(|f| system_dir.join(f).is_file())
}

// ---------------------------------------------------------------------------
// BIOS / system files (amendment §6): presence, the card's data,
// and the Add flow. The requirement table lives in ufm_emu::bios;
// the files themselves are the man's own console dumps — Fable
// never downloads and never bundles them (the law).
// ---------------------------------------------------------------------------

/// Where system files live and where the Add flow files them: the
/// ACTIVE RetroArch flavor's system directory. When RetroArch
/// isn't installed yet, the Flatpak flavor's — that's the channel
/// Fable installs, so files filed now are exactly where the first
/// launch will look.
pub fn bios_system_root(layout: Option<&RaLayout>) -> PathBuf {
    match layout {
        Some(l) => l.config_dir.join("system"),
        None => ra_layout(RaFlavor::Flatpak).config_dir.join("system"),
    }
}

/// Where DuckStation keeps its system files: the flatpak app's
/// config home when the flatpak is how it's installed (Fable's
/// channel), else the native config home.
pub fn duckstation_bios_dir() -> PathBuf {
    let home = home_dir();
    if flatpak_has_app("org.duckstation.DuckStation")
        || !crate::deps::tool_present("duckstation")
    {
        home.join(".var/app/org.duckstation.DuckStation/config/duckstation/bios")
    } else {
        home.join(".config/duckstation/bios")
    }
}

/// Where PCSX2 keeps its BIOS files (flatpak config home, else
/// the native one — same shape rule as DuckStation).
pub fn pcsx2_bios_dir() -> PathBuf {
    let home = home_dir();
    if flatpak_has_app("net.pcsx2.PCSX2") || !crate::deps::tool_present("pcsx2") {
        home.join(".var/app/net.pcsx2.PCSX2/config/PCSX2/bios")
    } else {
        home.join(".config/PCSX2/bios")
    }
}

/// RPCS3's config home. Its requirement sets carry the
/// `dev_flash/sys/external` subdir — the firmware payload lives
/// there once RPCS3 has installed a PUP.
pub fn rpcs3_config_dir() -> PathBuf {
    home_dir().join(".config/rpcs3")
}

/// The system-file home for a STANDALONE option, when Fable
/// knows it (v2). Standalones keep their own folders — the gate
/// checks the folder the emulator itself will look in.
pub fn standalone_bios_root(sys_id: &str, opt: &EmuOption) -> Option<PathBuf> {
    match (sys_id, opt.id.as_str()) {
        ("psx", "duckstation-standalone") => Some(duckstation_bios_dir()),
        ("ps2", "pcsx2-standalone") => Some(pcsx2_bios_dir()),
        ("ps3", "rpcs3-standalone") => Some(rpcs3_config_dir()),
        _ => None,
    }
}

/// The home a requirement's presence is judged against for one
/// option: cores → the RetroArch system dir; known standalones →
/// their own folder; anything else → the RetroArch home (the
/// v1.1 behavior the card always had).
fn bios_root_for(sys: &SystemEntry, opt: &EmuOption, layout: Option<&RaLayout>) -> PathBuf {
    if opt.kind == OptionKind::Standalone {
        if let Some(root) = standalone_bios_root(&sys.id, opt) {
            return root;
        }
    }
    bios_system_root(layout)
}

fn dir_listing(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// The requirement's unsatisfied sets against a system root.
pub fn bios_missing_sets<'a>(
    req: &'a ufm_emu::BiosRequirement,
    system_root: &Path,
) -> Vec<&'a ufm_emu::BiosSet> {
    req.missing_sets(|set| dir_listing(&system_root.join(set.subdir)))
}

pub fn bios_satisfied(req: &ufm_emu::BiosRequirement, system_root: &Path) -> bool {
    bios_missing_sets(req, system_root).is_empty()
}

/// One option's state here, BIOS included.
///
/// Layering rules: the requirement table decides gating — a
/// gating requirement unsatisfied in the option's system-file
/// home makes it `NeedsBios`, layered UNDER any install need
/// (install first, then the sheet explains the file — never a
/// launch into a black screen). Homes (v2): cores look in the
/// RetroArch system dir; the standalone wave's emulators look in
/// their own folders (DuckStation's / PCSX2's config homes,
/// RPCS3's dev_flash) — a standalone is never gated by the
/// RetroArch dir. Non-gating requirements (optional /
/// recommended / only-some-games) inform through the card and
/// the sheet but never change the state. Mined systems without
/// a requirement keep the legacy per-option list check.
pub fn state_of(
    sys: &SystemEntry,
    opt: &EmuOption,
    env: &EnvProbe,
    layout: Option<&RaLayout>,
) -> OptState {
    let base = option_state(opt, env);
    match opt.kind {
        OptionKind::LibretroCore => match ufm_emu::requirement_for(&sys.id) {
            Some(req) if req.gate_play => {
                if base.needs_install() {
                    return base;
                }
                if bios_satisfied(req, &bios_system_root(layout)) {
                    base
                } else {
                    OptState::NeedsBios
                }
            }
            Some(_) => base,
            None => with_bios(base, opt, bios_present(opt, layout)),
        },
        OptionKind::Standalone => {
            // Only an installed-and-ready standalone can be gated
            // (install needs and manual-only states pass through —
            // the BIOS conversation starts once the emulator is
            // actually here, mirroring the cores' layering).
            if base != OptState::Ready {
                return base;
            }
            let Some(req) = ufm_emu::requirement_for(&sys.id) else {
                return base;
            };
            if !req.gate_play {
                return base;
            }
            let Some(root) = standalone_bios_root(&sys.id, opt) else {
                return base;
            };
            if bios_satisfied(req, &root) {
                base
            } else {
                OptState::NeedsBios
            }
        }
        OptionKind::OsDefault | OptionKind::EsDeDoor => base,
    }
}

/// The sheet's BIOS row data for one option, when its system's
/// requirement has anything unsatisfied worth saying: gating
/// requirements always; recommended ones quietly; optional ones
/// never (the card carries those).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BiosGateWire {
    /// Display names of the missing files.
    pub files: Vec<String>,
    /// The requirement's plain note.
    pub note: String,
    /// True when the missing files block this launch.
    pub blocking: bool,
}

fn bios_gate_wire(
    sys: &SystemEntry,
    opt: &EmuOption,
    layout: Option<&RaLayout>,
    state: OptState,
) -> Option<BiosGateWire> {
    if opt.kind != OptionKind::LibretroCore && opt.kind != OptionKind::Standalone {
        return None;
    }
    let req = ufm_emu::requirement_for(&sys.id)?;
    if !req.gate_play && req.need != ufm_emu::BiosNeed::Recommended {
        return None;
    }
    // The option's own home decides what's missing — a
    // standalone's row talks about ITS folder, not RetroArch's.
    let root = bios_root_for(sys, opt, layout);
    let missing = bios_missing_sets(req, &root);
    if missing.is_empty() {
        return None;
    }
    let mut files: Vec<String> = Vec::new();
    for set in missing {
        if set.all {
            // Every missing name is needed (Channel F's trio).
            files.extend(set.missing_names(&dir_listing(&root.join(set.subdir))));
        } else {
            // Any ONE of the names does (region variants) — say so
            // as one alternative list, not a shopping list.
            files.push(set.display_names().join(" or "));
        }
    }
    Some(BiosGateWire {
        files,
        note: req.note.to_string(),
        blocking: state == OptState::NeedsBios,
    })
}

/// One set's row on the System-files card.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BiosSetWire {
    pub what: String,
    pub names: Vec<String>,
    /// True when every name is needed (vs any one of them).
    pub all: bool,
    /// The set's corner of the system folder ("" = the folder
    /// itself, "dc" = Dreamcast's, "fbneo" = FinalBurn Neo's).
    /// Carried so the card can DRAW the folder the way it should
    /// look when the files are home (Dickles, 2026-10-17: "What
    /// does the format look like? Help them.").
    pub subdir: String,
    pub present: bool,
    pub missing: Vec<String>,
}

/// One system's row on the System-files card (Preferences → ROMs).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BiosSystemWire {
    pub system_id: String,
    pub title: String,
    pub need: ufm_emu::BiosNeed,
    pub satisfied: bool,
    pub gate_play: bool,
    /// Where these files live, home-relative ("~/.var/app/…").
    pub home: String,
    pub sets: Vec<BiosSetWire>,
    pub note: String,
}

pub fn home_relative(path: &Path) -> String {
    let home = home_dir();
    match path.strip_prefix(&home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

pub fn bios_system_wire(req: &ufm_emu::BiosRequirement, system_root: &Path) -> BiosSystemWire {
    let sets: Vec<BiosSetWire> = req
        .sets
        .iter()
        .map(|set| {
            let listing = dir_listing(&system_root.join(set.subdir));
            BiosSetWire {
                what: set.what.to_string(),
                names: set.display_names(),
                all: set.all,
                subdir: set.subdir.to_string(),
                present: set.satisfied_by(&listing),
                missing: set.missing_names(&listing),
            }
        })
        .collect();
    BiosSystemWire {
        system_id: req.system.to_string(),
        title: req.title.to_string(),
        need: req.need,
        satisfied: sets.iter().all(|s| s.present),
        gate_play: req.gate_play,
        home: home_relative(system_root),
        sets,
        note: req.note.to_string(),
    }
}

/// The home the System-files card judges one requirement
/// against: the home of the system's EFFECTIVE option (his saved
/// choice, else the curated default) — so the card answers the
/// question Play will ask. When his PlayStation choice is
/// DuckStation, the card reads DuckStation's folder; when it's a
/// Beetle core, RetroArch's system dir.
fn card_bios_root(
    req: &ufm_emu::BiosRequirement,
    layout: Option<&RaLayout>,
    settings: &EmuSettings,
) -> PathBuf {
    let Some(sys) = ufm_emu::table::system_by_id(req.system) else {
        return bios_system_root(layout);
    };
    let saved = settings
        .choices
        .iter()
        .find(|(platform, _)| esde_id(platform) == req.system)
        .map(|(_, id)| id.as_str());
    let opt = saved
        .and_then(|id| sys.option(id))
        .or_else(|| sys.default());
    match opt {
        Some(opt) => bios_root_for(sys, opt, layout),
        None => bios_system_root(layout),
    }
}

/// The whole card: every requirement system, games or not
/// (amendment §6 — state display is cheap; the flow is the feature).
pub fn bios_state_wires(
    layout: Option<&RaLayout>,
    settings: &EmuSettings,
) -> Vec<BiosSystemWire> {
    ufm_emu::BIOS_REQUIREMENTS
        .iter()
        .map(|req| bios_system_wire(req, &card_bios_root(req, layout, settings)))
        .collect()
}

/// Every home the Add flow files a system's files into: the
/// effective option's home FIRST (the card judges that one),
/// plus the RetroArch system dir when it differs — PS1/PS2 games
/// can be played from either side of the fence, and a 4 MB file
/// filed in both homes means whichever emulator he picks later
/// just works (v2). PS3 is the exception: its Add flow is the
/// PUP hand-off in `emu_bios_add`, never a file copy.
pub fn bios_add_roots(
    system_id: &str,
    layout: Option<&RaLayout>,
    settings: &EmuSettings,
) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(req) = ufm_emu::requirement_for(system_id) {
        roots.push(card_bios_root(req, layout, settings));
    }
    let ra_root = bios_system_root(layout);
    if !roots.contains(&ra_root) {
        roots.push(ra_root);
    }
    roots
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BiosFiledWire {
    /// The file the man picked (base name).
    pub picked: String,
    /// The name it was filed under.
    pub filed_as: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BiosRejectedWire {
    pub picked: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BiosAddWire {
    pub cancelled: bool,
    /// The system's refreshed card row after the filing.
    pub system: BiosSystemWire,
    pub filed: Vec<BiosFiledWire>,
    pub rejected: Vec<BiosRejectedWire>,
}

/// File picked files into a system's system-file home (the Add
/// flow's engine — the picker command feeds it, the acceptance
/// harness feeds it paths directly). Each file whose name matches
/// a needed file is copied under the canonical name (temp +
/// rename, then verified by size); unknown names are refused with
/// a plain reason. Nothing is ever fetched.
pub fn add_bios_files(
    system_id: &str,
    picked: &[PathBuf],
    system_root: &Path,
) -> Result<BiosAddWire, String> {
    let req = ufm_emu::requirement_for(system_id)
        .ok_or_else(|| "Fable doesn't track system files for that system.".to_string())?;
    let mut filed = Vec::new();
    let mut rejected = Vec::new();
    for path in picked {
        let picked_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let matched = req
            .sets
            .iter()
            .find_map(|set| set.match_pick(&picked_name).map(|canon| (set, canon)));
        let Some((set, canonical)) = matched else {
            rejected.push(BiosRejectedWire {
                picked: picked_name,
                reason: format!(
                    "That isn't one of the {} system files Fable is looking for ({}).",
                    req.title,
                    req.all_names().join(", ")
                ),
            });
            continue;
        };
        let dest_dir = system_root.join(set.subdir);
        let dest = dest_dir.join(&canonical);
        let outcome = (|| -> Result<(), String> {
            std::fs::create_dir_all(&dest_dir)
                .map_err(|e| format!("couldn't open the system folder: {e}"))?;
            let tmp = dest_dir.join(format!(".{canonical}.part"));
            std::fs::copy(path, &tmp).map_err(|e| format!("couldn't copy the file: {e}"))?;
            std::fs::rename(&tmp, &dest).map_err(|e| format!("couldn't file the copy: {e}"))?;
            // Verification: the filed copy is whole and non-empty
            // (the amendment names no hashes — name + a verified
            // copy is the check).
            let src_len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            let dst_len = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
            if dst_len == 0 || dst_len != src_len {
                let _ = std::fs::remove_file(&dest);
                return Err("The copy didn't verify — the original is untouched.".to_string());
            }
            Ok(())
        })();
        match outcome {
            Ok(()) => filed.push(BiosFiledWire {
                picked: picked_name,
                filed_as: canonical,
            }),
            Err(reason) => rejected.push(BiosRejectedWire {
                picked: picked_name,
                reason,
            }),
        }
    }
    Ok(BiosAddWire {
        cancelled: false,
        system: bios_system_wire(req, system_root),
        filed,
        rejected,
    })
}

// ---------------------------------------------------------------------------
// Installing — dependency law, in context, progress reported.
// ---------------------------------------------------------------------------

pub type Progress<'a> = dyn Fn(&str, Option<u32>) + Send + Sync + 'a;

/// Run a command to completion with a hard timeout (the deps.rs
/// discipline: a stuck prompt or mirror must never wedge a thread).
fn run_long(cmd: &str, args: &[&str], timeout: Duration) -> Result<(), String> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't start {cmd}: {e}"))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let out = child
                    .wait_with_output()
                    .map_err(|e| format!("{cmd} failed: {e}"))?;
                if out.status.success() {
                    return Ok(());
                }
                let tail = String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                return Err(if tail.is_empty() {
                    format!("{cmd} didn't finish the job")
                } else {
                    tail
                });
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{cmd} timed out"));
                }
                std::thread::sleep(Duration::from_millis(150));
            }
            Err(e) => return Err(format!("{cmd} failed: {e}")),
        }
    }
}

/// Make sure the USER flatpak installation can see Flathub.
///
/// A --user install resolves refs from USER remotes only, and a
/// box whose flathub is registered system-wide (this one) can't
/// serve user installs ("No remote refs found"). Register the
/// same remote for the user first — idempotent, no password,
/// entirely per-user. v1 learned this for RetroArch; v2's whole
/// flatpak wave needs it, so it lives here now.
pub fn ensure_user_flathub() -> Result<(), String> {
    let user_has_flathub = run_stdout(
        "flatpak",
        &["remote-list", "--user", "--columns=name"],
        Duration::from_secs(20),
    )
    .map(|s| s.lines().any(|l| l.trim() == "flathub"))
    .unwrap_or(false);
    if user_has_flathub {
        return Ok(());
    }
    run_long(
        "flatpak",
        &[
            "remote-add",
            "--user",
            "--if-not-exists",
            "flathub",
            "https://dl.flathub.org/repo/flathub.flatpakrepo",
        ],
        Duration::from_secs(120),
    )
    .map_err(|e| {
        format!(
            "Fable couldn't set up Flathub for installs: {e}. Nothing else is affected — try again from the same place."
        )
    })
}

/// Make sure RetroArch exists; returns its layout. Flatpak first
/// (amendment §4.1), apt via deps.rs as the fallback.
pub fn ensure_retroarch(progress: &Progress) -> Result<RaLayout, String> {
    let (env, layout) = probe_env();
    if let Some(l) = layout {
        return Ok(l);
    }
    if env.flatpak_available {
        progress("Installing RetroArch…", None);
        ensure_user_flathub()?;
        run_long(
            "flatpak",
            &[
                "install",
                "--user",
                "-y",
                "--noninteractive",
                "flathub",
                "org.libretro.RetroArch",
            ],
            Duration::from_secs(1800),
        )
        .map_err(|e| {
            format!(
                "Fable couldn't install RetroArch: {e}. Nothing else is affected — try again from the same place."
            )
        })?;
    } else {
        progress("Installing RetroArch…", None);
        crate::deps::ensure_tool("retroarch", "retroarch")?;
    }
    let (_, layout) = probe_env();
    layout.ok_or_else(|| {
        "RetroArch's install finished, but RetroArch still isn't on this system — nothing else is affected, and you can try again from the same place."
            .to_string()
    })
}

/// Update one user flatpak in place (v2.1 update apply — only
/// ever run because he pressed the button on the updates card).
pub fn flatpak_update(app_id: &str) -> Result<(), String> {
    run_long(
        "flatpak",
        &["update", "--user", "-y", "--noninteractive", app_id],
        Duration::from_secs(1800),
    )
    .map_err(|e| format!("Fable couldn't update {app_id}: {e}"))
}

/// User flatpak app ids with an update waiting on the remote
/// (v2.1 update check). Empty when nothing is newer — and also
/// when flatpak itself can't answer, which a background check
/// treats as "nothing to report", never an error surface.
pub fn flatpak_pending_updates() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if let Some(text) = run_stdout(
        "flatpak",
        &["remote-ls", "--user", "--updates", "--columns=application"],
        Duration::from_secs(60),
    ) {
        for line in text.lines() {
            let id = line.trim();
            if !id.is_empty() {
                out.insert(id.to_string());
            }
        }
    }
    out
}

/// Fetch one core from the libretro buildbot into the flavor's
/// user cores dir. Idempotent; resumable by simply calling again.
pub fn fetch_core(
    layout: &RaLayout,
    core_file: &str,
    progress: &Progress,
) -> Result<PathBuf, String> {
    let dest = layout.cores_dir.join(core_file);
    if dest.is_file() {
        return Ok(dest);
    }
    download_core(layout, core_file, progress)
}

/// Fetch the buildbot's current core build, REPLACING the one on
/// disk (v2.1 update apply). The download stages through the same
/// `.part` discipline, so a failed refresh leaves the old core in
/// place rather than stranding the system.
pub fn refetch_core(
    layout: &RaLayout,
    core_file: &str,
    progress: &Progress,
) -> Result<PathBuf, String> {
    download_core(layout, core_file, progress)
}

/// The buildbot's current facts about a core zip — what an update
/// check compares the recorded fetch against. None when the
/// buildbot can't be reached; checks stay silent then.
pub fn core_remote_facts(core_file: &str) -> Option<(u64, String)> {
    let url = format!(
        "https://buildbot.libretro.com/nightly/linux/x86_64/latest/{core_file}.zip"
    );
    let resp = ureq::head(&url).call().ok()?;
    let len = resp
        .header("Content-Length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let modified = resp.header("Last-Modified").unwrap_or_default().to_string();
    Some((len, modified))
}

fn download_core(
    layout: &RaLayout,
    core_file: &str,
    progress: &Progress,
) -> Result<PathBuf, String> {
    let dest = layout.cores_dir.join(core_file);
    let url = format!(
        "https://buildbot.libretro.com/nightly/linux/x86_64/latest/{core_file}.zip"
    );
    progress("Downloading the core…", Some(0));
    let resp = ureq::get(&url)
        .call()
        .map_err(|e| format!("Fable couldn't reach the core library: {e}"))?;
    let total: u64 = resp
        .header("Content-Length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut reader = resp.into_reader();
    let mut bytes: Vec<u8> = Vec::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("the core download stopped: {e}"))?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buf[..n]);
        if bytes.len() > 300 * 1024 * 1024 {
            return Err("the core download grew implausibly large; stopped".to_string());
        }
        if total > 0 {
            let pct = ((bytes.len() as u64 * 100) / total).min(100) as u32;
            progress("Downloading the core…", Some(pct));
        }
    }
    progress("Unpacking the core…", None);
    let so = ufm_emu::zipx::extract_entry(&bytes, ".so")
        .map_err(|e| format!("the downloaded core couldn't be unpacked: {e}"))?;
    if so.len() < 4 || &so[0..4] != b"\x7fELF" {
        return Err("the downloaded core doesn't look like a core; refusing it".to_string());
    }
    std::fs::create_dir_all(&layout.cores_dir)
        .map_err(|e| format!("couldn't create the cores folder: {e}"))?;
    let tmp = layout.cores_dir.join(format!(".{core_file}.part"));
    std::fs::write(&tmp, &so).map_err(|e| format!("couldn't save the core: {e}"))?;
    std::fs::rename(&tmp, &dest).map_err(|e| format!("couldn't file the core: {e}"))?;
    Ok(dest)
}

// ---------------------------------------------------------------------------
// AppImages (v2) — the official-download channel. RPCS3 and ES-DE
// arrive as managed files in ~/Applications, verified against
// the sha256 the publisher's own channel publishes wherever one
// exists, and proved runnable (FUSE) before anyone calls them
// installed.
// ---------------------------------------------------------------------------

/// sha256 of a file, lowercase hex.
pub fn sha256_file(path: &Path) -> Option<String> {
    use sha2::Digest;
    let mut f = std::fs::File::open(path).ok()?;
    let mut h = sha2::Sha256::new();
    std::io::copy(&mut f, &mut h).ok()?;
    Some(format!("{:x}", h.finalize()))
}

/// Stream a URL to a file with progress. Written to a `.part`
/// sibling by the caller; this just moves the bytes.
fn download_to(url: &str, dest: &Path, label: &str, progress: &Progress) -> Result<u64, String> {
    use std::io::Write;
    let resp = ureq::get(url)
        .call()
        .map_err(|e| format!("Fable couldn't reach the download: {e}"))?;
    let total: u64 = resp
        .header("Content-Length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut reader = resp.into_reader();
    let mut out = std::fs::File::create(dest)
        .map_err(|e| format!("couldn't create the download file: {e}"))?;
    let mut buf = [0u8; 256 * 1024];
    let mut written: u64 = 0;
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("the download stopped: {e}"))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])
            .map_err(|e| format!("couldn't save the download: {e}"))?;
        written += n as u64;
        if written > 2 * 1024 * 1024 * 1024 {
            let _ = std::fs::remove_file(dest);
            return Err("the download grew implausibly large; stopped".to_string());
        }
        if total > 0 {
            let pct = ((written * 100) / total).min(100) as u32;
            progress(label, Some(pct));
        }
    }
    out.flush().map_err(|e| format!("couldn't save the download: {e}"))?;
    Ok(written)
}

/// A comparable version key from an asset name: the first four
/// digit groups ("rpcs3-v0.0.37-18146-…" → (0, 0, 37, 18146)).
/// RPCS3's release LIST order is not version order (an old build
/// can sit on top), so the newest asset is picked by name.
pub fn appimage_version_key(name: &str) -> (u64, u64, u64, u64) {
    let mut groups = name
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<u64>().ok());
    (
        groups.next().unwrap_or(0),
        groups.next().unwrap_or(0),
        groups.next().unwrap_or(0),
        groups.next().unwrap_or(0),
    )
}

/// Resolve a GitHub-releases AppImage: the newest asset whose
/// name ends with `asset_suffix`, plus the sha256 digest the
/// releases API publishes for it (when it publishes one).
pub fn resolve_github_appimage(
    repo: &str,
    asset_suffix: &str,
) -> Result<(String, Option<String>, String), String> {
    let url = format!("https://api.github.com/repos/{repo}/releases?per_page=10");
    let body = ureq::get(&url)
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("Fable couldn't check for the latest release: {e}"))?
        .into_string()
        .map_err(|e| format!("the release list couldn't be read: {e}"))?;
    let releases: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("the release list was unreadable: {e}"))?;
    let mut best: Option<((u64, u64, u64, u64), String, Option<String>, String)> = None;
    for release in releases.as_array().cloned().unwrap_or_default() {
        for asset in release["assets"].as_array().cloned().unwrap_or_default() {
            let name = asset["name"].as_str().unwrap_or_default();
            if !name.ends_with(asset_suffix) {
                continue;
            }
            let Some(dl) = asset["browser_download_url"].as_str() else {
                continue;
            };
            let digest = asset["digest"]
                .as_str()
                .and_then(|d| d.strip_prefix("sha256:"))
                .map(|d| d.to_string());
            let cand = (
                appimage_version_key(name),
                dl.to_string(),
                digest,
                name.to_string(),
            );
            if best.as_ref().map(|b| cand.0 > b.0).unwrap_or(true) {
                best = Some(cand);
            }
        }
    }
    best.map(|(_, dl, digest, name)| (dl, digest, name))
        .ok_or_else(|| "no suitable download was found in the latest releases".to_string())
}

/// Is FUSE 2 (the runtime every AppImage needs) on this system?
pub fn libfuse2_present() -> bool {
    for p in [
        "/usr/lib/x86_64-linux-gnu/libfuse.so.2",
        "/lib/x86_64-linux-gnu/libfuse.so.2",
        "/usr/lib/libfuse.so.2",
        "/usr/lib64/libfuse.so.2",
    ] {
        if Path::new(p).exists() {
            return true;
        }
    }
    run_stdout("ldconfig", &["-p"], Duration::from_secs(15))
        .map(|s| s.contains("libfuse.so.2"))
        .unwrap_or(false)
}

/// Prove an AppImage can start on this system — the AppImage
/// runtime answers `--appimage-version` itself, no display
/// needed. When FUSE is the problem, set it up (dependency law)
/// and prove again.
pub fn ensure_appimage_runnable(path: &Path, label: &str) -> Result<(), String> {
    let probe = |p: &Path| -> bool {
        run_stdout(
            &p.to_string_lossy(),
            &["--appimage-version"],
            Duration::from_secs(25),
        )
        .is_some()
    };
    if probe(path) {
        return Ok(());
    }
    crate::deps::ensure_with_check("libfuse2", &libfuse2_present).map_err(|e| {
        format!("{label} downloaded fine, but this system is missing the library AppImages need: {e}")
    })?;
    if probe(path) {
        return Ok(());
    }
    Err(format!(
        "{label} downloaded fine, but it couldn't start on this system — nothing else is affected, and you can try again from the same place."
    ))
}

/// Download an AppImage into ~/Applications under its stable
/// managed name: hash-verified when the channel published a
/// hash, then proved runnable. Idempotent — a present file that
/// matches the published hash is already installed.
pub fn install_appimage(
    url: &str,
    expected_sha256: Option<&str>,
    file_name: &str,
    label: &str,
    progress: &Progress,
) -> Result<PathBuf, String> {
    let dir = applications_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("couldn't open ~/Applications: {e}"))?;
    let dest = dir.join(file_name);
    if dest.is_file() {
        match expected_sha256 {
            Some(want) if sha256_file(&dest).as_deref() == Some(want) => {
                ensure_appimage_runnable(&dest, label)?;
                return Ok(dest);
            }
            None => {
                ensure_appimage_runnable(&dest, label)?;
                return Ok(dest);
            }
            _ => {}
        }
    }
    progress(&format!("Downloading {label}…"), Some(0));
    let part = dir.join(format!(".{file_name}.part"));
    download_to(url, &part, &format!("Downloading {label}…"), progress)?;
    if let Some(want) = expected_sha256 {
        progress("Verifying the download…", None);
        match sha256_file(&part) {
            Some(got) if got == want => {}
            _ => {
                let _ = std::fs::remove_file(&part);
                return Err(format!(
                    "{label}'s download didn't match the checksum its makers published, so Fable removed it. Nothing was installed."
                ));
            }
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o755));
    }
    std::fs::rename(&part, &dest).map_err(|e| format!("couldn't file {label}: {e}"))?;
    progress("Checking it runs…", None);
    ensure_appimage_runnable(&dest, label)?;
    Ok(dest)
}

/// Post-install verification for standalones (v2): the install
/// only counts when a fresh probe reads the option Ready —
/// BIOS-free, since `option_state` doesn't know about gates.
fn verify_option_ready(opt: &EmuOption, env: &EnvProbe) -> Result<(), String> {
    if option_state(opt, env) != OptState::Ready {
        return Err(
            "The setup finished, but the emulator still isn't ready — nothing else is affected, and you can try again from the same place."
                .to_string(),
        );
    }
    Ok(())
}

/// Install whatever an option needs, in dependency order, then
/// verify the install itself reads Ready. The sheet's "Install &
/// Play" is this function followed by the BIOS gate and a launch.
/// Verification is deliberately BIOS-free: a missing system file
/// is the sheet's gate to explain, never an install failure.
pub fn ensure_option(
    opt: &EmuOption,
    progress: &Progress,
) -> Result<(EnvProbe, Option<RaLayout>), String> {
    match opt.kind {
        OptionKind::LibretroCore => {
            let layout = ensure_retroarch(progress)?;
            let core = opt
                .core_file
                .clone()
                .ok_or("this core option doesn't name a core file")?;
            let (env, _) = probe_env();
            if !env.cores.contains(&core) {
                let name = opt.core_name.as_deref().unwrap_or("emulator");
                progress(&format!("Getting the {name} core…"), None);
                fetch_core(&layout, &core, progress)?;
            }
            let (env, layout2) = probe_env();
            if option_state(opt, &env) != OptState::Ready {
                return Err(
                    "The setup finished, but the emulator still isn't ready — nothing else is affected, and you can try again from the same place."
                        .to_string(),
                );
            }
            Ok((env, layout2))
        }
        OptionKind::Standalone => match &opt.install {
            ufm_emu::InstallSpec::Flatpak { app_id } => {
                progress(&format!("Installing {}…", opt.label), None);
                ensure_user_flathub()?;
                run_long(
                    "flatpak",
                    &["install", "--user", "-y", "--noninteractive", "flathub", app_id],
                    Duration::from_secs(1800),
                )
                .map_err(|e| format!("Fable couldn't install {}: {e}", opt.label))?;
                let (env, layout) = probe_env();
                verify_option_ready(opt, &env)?;
                Ok((env, layout))
            }
            ufm_emu::InstallSpec::AptLogical { logical } => {
                progress(&format!("Installing {}…", opt.label), None);
                let bin = opt.probe_bins.first().cloned().unwrap_or_default();
                crate::deps::ensure_tool(&bin, logical)?;
                let (env, layout) = probe_env();
                verify_option_ready(opt, &env)?;
                Ok((env, layout))
            }
            ufm_emu::InstallSpec::AppImage { url, file_name } => {
                install_appimage(url, None, file_name, &opt.label, progress)?;
                let (env, layout) = probe_env();
                verify_option_ready(opt, &env)?;
                Ok((env, layout))
            }
            ufm_emu::InstallSpec::GitHubAppImage {
                repo,
                asset_suffix,
                file_name,
            } => {
                progress(&format!("Checking for the latest {}…", opt.label), None);
                let (url, digest, _asset_name) = resolve_github_appimage(repo, asset_suffix)
                    .map_err(|e| format!("Fable couldn't install {}: {e}", opt.label))?;
                install_appimage(
                    &url,
                    digest.as_deref(),
                    file_name,
                    &opt.label,
                    progress,
                )?;
                let (env, layout) = probe_env();
                verify_option_ready(opt, &env)?;
                Ok((env, layout))
            }
            _ => Err(opt.install_note.clone().unwrap_or_else(|| {
                "Fable can't install this one yet.".to_string()
            })),
        },
        _ => Ok(probe_env()),
    }
}

// ---------------------------------------------------------------------------
// Launching — the real path, always (prime directive).
// ---------------------------------------------------------------------------

/// The `%EMULATOR_…%` tokens a template names (without the `%`s).
fn emulator_tokens(template: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(i) = rest.find("%EMULATOR_") {
        let tail = &rest[i + 1..];
        match tail.find('%') {
            Some(j) => {
                out.push(tail[..j].to_string());
                rest = &tail[j + 1..];
            }
            None => break,
        }
    }
    out
}

/// The command line that runs a standalone option HERE, as one
/// string for template substitution (multi-word is fine — the
/// expander tokenizes). Resolution order (v2): the channel
/// Fable manages wins — a managed AppImage by path; an installed
/// flatpak app by `flatpak run`; then the option's own binaries
/// on PATH (how hand-installed emulators are found).
pub fn standalone_command(opt: &EmuOption) -> Option<String> {
    match &opt.install {
        ufm_emu::InstallSpec::AppImage { file_name, .. }
        | ufm_emu::InstallSpec::GitHubAppImage { file_name, .. } => {
            if let Some(p) = managed_appimage(file_name) {
                return Some(p.to_string_lossy().into_owned());
            }
        }
        ufm_emu::InstallSpec::Flatpak { app_id } => {
            if flatpak_has_app(app_id) {
                return Some(format!("flatpak run {app_id}"));
            }
        }
        _ => {}
    }
    opt.probe_bins
        .iter()
        .find(|b| crate::deps::tool_present(b))
        .cloned()
}

/// Build the launch argv for an option + game path.
pub fn launch_argv(
    opt: &EmuOption,
    layout: Option<&RaLayout>,
    view_root: &Path,
    rom_path: &str,
) -> Result<(Vec<String>, Option<String>), String> {
    let mut emulators: HashMap<String, String> = HashMap::new();
    if let Some(l) = layout {
        emulators.insert("EMULATOR_RETROARCH".to_string(), l.command.join(" "));
    }
    // The option's resolved standalone command (v2 channels
    // included) answers every %EMULATOR_…% token in its template.
    if opt.kind == OptionKind::Standalone {
        if let Some(cmd) = standalone_command(opt) {
            for token in emulator_tokens(&opt.template) {
                emulators.insert(token, cmd.clone());
            }
        }
    }
    // Standalone find-rules: a probed binary answers its own
    // %EMULATOR_<NAME>% token in the option's template (the token's
    // suffix IS the binary's name — MESEN ↔ mesen, FS-UAE ↔
    // fs-uae; a longer binary name that starts with the suffix,
    // snes9x-gtk for SNES9X, also answers).
    for bin in &opt.probe_bins {
        if !crate::deps::tool_present(bin) {
            continue;
        }
        for token in emulator_tokens(&opt.template) {
            // A channel-resolved command (flatpak / AppImage) has
            // already answered this token — it stays (the BIOS
            // home is judged against that same channel).
            if emulators.contains_key(&token) {
                continue;
            }
            let suffix = token.strip_prefix("EMULATOR_").unwrap_or("");
            if suffix.eq_ignore_ascii_case(bin)
                || bin.to_lowercase().starts_with(&suffix.to_lowercase())
            {
                emulators.insert(token, bin.clone());
            }
        }
    }
    let core_dir = layout
        .map(|l| l.cores_dir.to_string_lossy().into_owned())
        .unwrap_or_default();
    let vars = LaunchVars {
        rom: rom_path.to_string(),
        core_dir,
        rom_path_root: view_root.to_string_lossy().into_owned(),
        emulators,
    };
    let expanded = expand_template(&opt.template, &vars);
    if expanded.argv.is_empty() || expanded.argv.iter().any(|a| a.contains("%EMULATOR_")) {
        return Err(
            "Fable couldn't work out how to start this emulator on this system.".to_string(),
        );
    }
    Ok((expanded.argv, expanded.cwd))
}

/// Spawn the emulator detached and give it a breath to fail loudly
/// (a core that can't load exits in well under a second; a game
/// that starts stays up).
pub fn spawn_launch(argv: &[String], cwd: Option<&str>) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("couldn't start the emulator: {e}"))?;
    std::thread::sleep(Duration::from_millis(900));
    match child.try_wait() {
        Ok(Some(status)) => Err(format!(
            "The emulator closed right away (exit {}). The game file is fine — the emulator had a problem starting.",
            status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into())
        )),
        Ok(None) => Ok(()),
        Err(_) => Ok(()),
    }
}

/// Launch + record the play (the shipped rom_home_note_play
/// machinery — play counts / Recently Played stay the one truth).
pub fn launch_and_note(
    state: &AppState,
    opt: &EmuOption,
    layout: Option<&RaLayout>,
    view_root: &Path,
    rom_path: &str,
) -> Cmd<ufm_catalog::RomGameState> {
    let (argv, cwd) = launch_argv(opt, layout, view_root, rom_path)
        .map_err(|e| CommandError::from(UfmError::Internal(e)))?;
    spawn_launch(&argv, cwd.as_deref())
        .map_err(|e| CommandError::from(UfmError::Internal(e)))?;
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path).map_err(CommandError::from)?;
    catalog
        .rom_game_note_play(rom_path)
        .map_err(CommandError::from)
}

// ---------------------------------------------------------------------------
// Wire shapes for the sheet + outcomes.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SheetOptionWire {
    pub id: String,
    pub label: String,
    pub verdict: String,
    pub state: OptState,
    pub state_word: String,
    pub needs_install: bool,
    pub class: ufm_emu::OptClass,
    pub install_note: Option<String>,
    /// The BIOS row (§6): present when this option's system needs
    /// a system file that isn't there — shown BEFORE any launch.
    pub bios: Option<BiosGateWire>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SheetWire {
    pub game_title: String,
    /// The copy Play was pressed on (a path — also the state key).
    pub provider_key: String,
    /// Catalog platform key ("nes").
    pub platform: String,
    pub system_id: String,
    pub system_name: String,
    pub short_name: String,
    pub options: Vec<SheetOptionWire>,
    pub preselect: String,
    /// "ask" (first Play) or "picker" (Play with…).
    pub mode: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "kind")]
pub enum EmuPlayOutcome {
    Launched {
        state: ufm_catalog::RomGameState,
        option_id: String,
        /// The copy that actually launched (best present copy —
        /// may differ from the pressed row when drives come and go).
        played_provider_key: String,
    },
    Sheet {
        sheet: SheetWire,
    },
    /// v2 (§4.4): the emulator is installed and the system files
    /// are fine, but its flatpak sandbox can't see the drive this
    /// game lives on. The sheet asks once — Allow access & Play —
    /// and `emu_grant_access_and_play` applies the scoped grant.
    AccessNeeded {
        option_id: String,
        /// The flatpak app the grant would go to.
        app_id: String,
        /// The option's friendly name ("DuckStation").
        app_label: String,
        /// The volume root the grant would open (nothing wider).
        filesystem: String,
        message: String,
    },
    Unavailable {
        message: String,
    },
}

/// One game's identity group from the shelf query: every row that
/// is the same game (same platform + sha1/title), i.e. its copies.
pub struct GameGroup {
    pub title: String,
    pub platform: String,
    pub rows: Vec<ufm_catalog::RomHomeGame>,
}

pub fn identity_of(row: &ufm_catalog::RomHomeGame) -> String {
    match &row.sha1 {
        // A real hash identifies the game. The catalog also stores
        // an all-zero sentinel when a file couldn't be hashed (his
        // SNES zips all carry it) — that is "no hash" wearing a
        // hash costume, and keying on it would collapse a whole
        // library into one game. Fall back to the DAT title, which
        // is how those rows were matched in the first place.
        Some(sha) if !sha.is_empty() && sha.chars().any(|c| c != '0') => {
            format!("{}|{sha}", row.platform)
        }
        _ => format!("{}|{}", row.platform, row.title),
    }
}

/// Find the group for a pressed game (by the copy that was pressed).
pub fn game_group(
    catalog: &ufm_catalog::Catalog,
    platform: &str,
    provider_key: &str,
) -> Result<Option<GameGroup>, String> {
    let rows = catalog
        .rom_home_games(Some(platform))
        .map_err(|e| e.to_string())?;
    let Some(pressed_idx) = rows
        .iter()
        .position(|r| r.item.provider_key.as_deref() == Some(provider_key))
    else {
        return Ok(None);
    };
    let identity = identity_of(&rows[pressed_idx]);
    let title = rows[pressed_idx].title.clone();
    let group_rows: Vec<ufm_catalog::RomHomeGame> = rows
        .into_iter()
        .filter(|r| identity_of(r) == identity)
        .collect();
    Ok(Some(GameGroup {
        title,
        platform: platform.to_string(),
        rows: group_rows,
    }))
}

/// A path's home volume: the volume whose root — a live
/// mountpoint or a remembered mount-history path — claims the
/// longest prefix of it. Presence means the home volume is mounted
/// RIGHT NOW. (Longest-live-prefix alone is not enough: "/" claims
/// every absolute path, which would make an unplugged drive's
/// games look internal. Library Memory's mount history is what
/// tells an unplugged drive's files from the internal disk's.)
pub struct HomeVolume {
    /// The claiming root (live mountpoint or history path).
    pub root: String,
    /// True when that root is a live mountpoint.
    pub live: bool,
    /// The catalog record claiming the path, when one does.
    pub record_label: Option<String>,
    /// Farm rank of the live mount (internal 0, fixed 1, removable 2).
    pub rank: u32,
}

pub fn home_volume(
    path: &str,
    mounted: &[crate::commands::MountedVolume],
    volumes: &[ufm_catalog::VolumeRecord],
) -> Option<HomeVolume> {
    // (prefix length, is-live, root, label, rank) — longest claim
    // wins; a live claim beats a history claim of the same length.
    let mut best: Option<(usize, bool, String, Option<String>, u32)> = None;
    let mut consider = |len: usize,
                          live: bool,
                          root: &str,
                          label: Option<String>,
                          rank: u32| {
        let better = match &best {
            None => true,
            Some((bl, blive, _, _, _)) => len > *bl || (len == *bl && live && !blive),
        };
        if better {
            best = Some((len, live, root.to_string(), label, rank));
        }
    };
    for v in mounted {
        if crate::commands::path_under(path, &v.path) {
            let rank = if v.path == "/" {
                0
            } else if !v.removable {
                1
            } else {
                2
            };
            consider(v.path.len(), true, &v.path, None, rank);
        }
    }
    for rec in volumes {
        for m in &rec.mount_history {
            if crate::commands::path_under(path, &m.path) {
                consider(m.path.len(), false, &m.path, rec.label.clone(), 2);
            }
        }
    }
    best.map(|(_, live, root, record_label, rank)| HomeVolume {
        root,
        live,
        record_label,
        rank,
    })
}

/// The best present copy of a group: ranked exactly like the farm
/// (internal first, then fixed volumes, then removable), so Play
/// and the view always agree about which copy is "the" copy.
/// Rows inside the unified view are the mirror's reflection, not
/// copies (the view is never a source — see emu_farm), and are
/// skipped even when the pressed row is one of them.
pub fn best_present_copy<'a>(
    group: &'a GameGroup,
    mounted: &[crate::commands::MountedVolume],
    volumes: &[ufm_catalog::VolumeRecord],
    view_root: &Path,
) -> Option<&'a ufm_catalog::RomHomeGame> {
    group
        .rows
        .iter()
        .filter_map(|r| {
            let pk = r.item.provider_key.as_ref()?;
            if Path::new(pk.as_str()).starts_with(view_root) {
                return None;
            }
            let home = home_volume(pk, mounted, volumes)?;
            home.live.then(|| (home.rank, pk.clone(), r))
        })
        .min_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)))
        .map(|(_, _, r)| r)
}

fn sheet_wire(
    sys: &SystemEntry,
    group: &GameGroup,
    pressed_key: &str,
    env: &EnvProbe,
    layout: Option<&RaLayout>,
    preselect: &str,
    mode: &str,
) -> SheetWire {
    SheetWire {
        game_title: group.title.clone(),
        provider_key: pressed_key.to_string(),
        platform: group.platform.clone(),
        system_id: sys.id.clone(),
        system_name: sys.fullname.clone(),
        short_name: sys.short_name.clone(),
        options: sys
            .options
            .iter()
            .map(|o| {
                let st = state_of(sys, o, env, layout);
                SheetOptionWire {
                    id: o.id.clone(),
                    label: o.label.clone(),
                    verdict: o.verdict.clone(),
                    state: st,
                    state_word: st.word().to_string(),
                    needs_install: st.needs_install(),
                    class: o.class,
                    install_note: o.install_note.clone(),
                    bios: bios_gate_wire(sys, o, layout, st),
                }
            })
            .collect(),
        preselect: preselect.to_string(),
        mode: mode.to_string(),
    }
}

/// Shared Play prelude: presence + group + probe. On success,
/// returns everything the decision needs plus the copy to launch.
struct PlayPrelude {
    sys: &'static SystemEntry,
    group: GameGroup,
    launch_row: ufm_catalog::RomHomeGame,
    env: EnvProbe,
    layout: Option<RaLayout>,
    settings: EmuSettings,
}

fn play_prelude(
    state: &AppState,
    catalog_platform: &str,
    provider_key: &str,
) -> Result<PlayPrelude, EmuPlayOutcome> {
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path).map_err(|e| {
        EmuPlayOutcome::Unavailable {
            message: format!("Fable couldn't read the library: {e}"),
        }
    })?;
    play_prelude_with_catalog(state, &catalog, catalog_platform, provider_key)
}

fn play_prelude_with_catalog(
    state: &AppState,
    catalog: &ufm_catalog::Catalog,
    catalog_platform: &str,
    provider_key: &str,
) -> Result<PlayPrelude, EmuPlayOutcome> {
    let unavailable = |message: String| EmuPlayOutcome::Unavailable { message };
    let group = match game_group(catalog, catalog_platform, provider_key) {
        Ok(Some(g)) => g,
        Ok(None) => {
            return Err(unavailable(
                "That game isn't in the library any more.".to_string(),
            ))
        }
        Err(e) => return Err(unavailable(format!("Fable couldn't read the library: {e}"))),
    };
    let system_id = esde_id(catalog_platform);
    let Some(sys) = ufm_emu::table::system_by_id(&system_id) else {
        return Err(unavailable(format!(
            "Fable doesn't know how to play {} games yet.",
            catalog_platform
        )));
    };
    // Presence at launch (amendment §2): never attempt a launch
    // against a missing file.
    let mounted = crate::commands::current_filesystems();
    let volumes = catalog.volumes().unwrap_or_default();
    let settings = load_settings(&state.data_base);
    let Some(launch_row) =
        best_present_copy(&group, &mounted, &volumes, &settings.view_root_path()).cloned()
    else {
        // Name the drive that owns the pressed copy, the way the
        // room itself phrases it.
        let message = match home_volume(provider_key, &mounted, &volumes) {
            Some(home) => match home.record_label {
                Some(label) => format!("Connect {label} to play"),
                None => "That game is on a drive that isn't connected right now.".to_string(),
            },
            None => "That game is on a drive that isn't connected right now.".to_string(),
        };
        return Err(unavailable(message));
    };
    let (env, layout) = probe_env();
    Ok(PlayPrelude {
        sys,
        group,
        launch_row,
        env,
        layout,
        settings,
    })
}

// ---------------------------------------------------------------------------
// Sandbox drive access (amendment §4.4, v2). A flatpak emulator
// can only read what its sandbox allows. Coverage is decided
// DECLARATIVELY — the app's own metadata filesystems plus the
// user's overrides, against the game's real path — never by
// trial launches. When a launch would be blind, the sheet asks
// once; consent applies one scoped grant for the game's home
// volume and records it where he can see and revoke it
// (Preferences → ROMs).
// ---------------------------------------------------------------------------

/// Every `filesystems=` entry in a flatpak metadata / override
/// dump (`filesystems=host;home;` style INI lines).
fn parse_filesystems_line(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        if let Some(v) = line.trim().strip_prefix("filesystems=") {
            for e in v.split(';') {
                let e = e.trim();
                if !e.is_empty() {
                    out.push(e.to_string());
                }
            }
        }
    }
    out
}

/// The grants an app effectively has: its shipped metadata plus
/// the user's overrides.
fn flatpak_filesystem_grants(app_id: &str) -> Vec<String> {
    let mut grants = Vec::new();
    if let Some(meta) = run_stdout(
        "flatpak",
        &["info", "--show-metadata", app_id],
        Duration::from_secs(20),
    ) {
        grants.extend(parse_filesystems_line(&meta));
    }
    if let Some(ovr) = run_stdout(
        "flatpak",
        &["override", "--user", "--show", app_id],
        Duration::from_secs(20),
    ) {
        grants.extend(parse_filesystems_line(&ovr));
    }
    grants
}

/// Do these grants cover this path? Grant spellings: `host`
/// (everything), `home` (the home tree), absolute paths, `~/…`
/// paths; `:ro`/`:rw`/`:create` mode suffixes don't change
/// coverage.
pub fn grant_covers(grants: &[String], path: &Path) -> bool {
    let home = home_dir();
    for g in grants {
        let e = g.trim();
        let base = match e.rsplit_once(':') {
            Some((head, mode)) if matches!(mode, "ro" | "rw" | "create") => head,
            _ => e,
        };
        match base {
            "host" => return true,
            "home" => {
                if path.starts_with(&home) {
                    return true;
                }
            }
            _ if base.starts_with('/') => {
                if path.starts_with(Path::new(base)) {
                    return true;
                }
            }
            _ if base.starts_with("~/") => {
                if path.starts_with(home.join(&base[2..])) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// Can this flatpak app actually read this file where it sits?
pub fn sandbox_can_see(app_id: &str, path: &Path) -> bool {
    grant_covers(&flatpak_filesystem_grants(app_id), path)
}

/// The §4.4 grant: open exactly this filesystem root to the app.
pub fn apply_sandbox_override(app_id: &str, filesystem: &str) -> Result<(), String> {
    run_long(
        "flatpak",
        &[
            "override",
            "--user",
            &format!("--filesystem={filesystem}"),
            app_id,
        ],
        Duration::from_secs(60),
    )
    .map_err(|e| format!("Fable couldn't open the drive to the emulator: {e}"))
}

/// Take a grant back (`flatpak override --nofilesystem=…`).
pub fn remove_sandbox_override(app_id: &str, filesystem: &str) -> Result<(), String> {
    run_long(
        "flatpak",
        &[
            "override",
            "--user",
            &format!("--nofilesystem={filesystem}"),
            app_id,
        ],
        Duration::from_secs(60),
    )
    .map_err(|e| format!("Fable couldn't take back the drive access: {e}"))
}

/// Record a grant in the settings (Preferences shows + revokes
/// from the same list — the record and the machine never drift:
/// recording happens only after the override landed).
pub fn record_override(base: &Path, app_id: &str, filesystem: &str) -> Result<(), String> {
    let mut settings = load_settings(base);
    if !settings
        .overrides
        .iter()
        .any(|o| o.app_id == app_id && o.filesystem == filesystem)
    {
        settings.overrides.push(SandboxOverride {
            app_id: app_id.to_string(),
            filesystem: filesystem.to_string(),
        });
        save_settings(base, &settings)?;
    }
    Ok(())
}

pub fn forget_override(base: &Path, app_id: &str, filesystem: &str) -> Result<(), String> {
    let mut settings = load_settings(base);
    settings
        .overrides
        .retain(|o| !(o.app_id == app_id && o.filesystem == filesystem));
    save_settings(base, &settings)
}

/// The access gap for one launch, when there is one: (app id,
/// the filesystem root a grant would open, the volume's name for
/// the sentence). The root is the game's home VOLUME's root —
/// one grant per drive, scoped to that drive — or, for a game
/// on the internal disk outside the sandbox's reach, the game's
/// own folder.
fn access_gap(
    state: &AppState,
    opt: &EmuOption,
    rom_path: &str,
) -> Option<(String, String, String)> {
    let ufm_emu::InstallSpec::Flatpak { app_id } = &opt.install else {
        return None;
    };
    if sandbox_can_see(app_id, Path::new(rom_path)) {
        return None;
    }
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path).ok();
    let volumes = catalog
        .as_ref()
        .and_then(|c| c.volumes().ok())
        .unwrap_or_default();
    let mounted = crate::commands::current_filesystems();
    let parent = Path::new(rom_path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/".to_string());
    let (filesystem, word) = match home_volume(rom_path, &mounted, &volumes) {
        Some(h) if h.live && h.root != "/" => {
            let word = h.record_label.clone().unwrap_or_else(|| h.root.clone());
            (h.root.clone(), word)
        }
        Some(h) => (
            parent,
            h.record_label.unwrap_or_else(|| "this computer".to_string()),
        ),
        None => (parent, "this computer".to_string()),
    };
    Some((app_id.clone(), filesystem, word))
}

/// The shared launch tail (§4.4 check, then launch + note). Used
/// by Play, Install & Play, and Allow access & Play, so every
/// road to a launch crosses the same sandbox gate.
fn finish_launch(
    state: &AppState,
    prelude: &PlayPrelude,
    opt: &EmuOption,
    layout: Option<&RaLayout>,
    option_id: &str,
) -> EmuPlayOutcome {
    let rom_path = prelude
        .launch_row
        .item
        .provider_key
        .clone()
        .unwrap_or_default();
    if let Some((app_id, filesystem, word)) = access_gap(state, opt, &rom_path) {
        return EmuPlayOutcome::AccessNeeded {
            option_id: option_id.to_string(),
            app_id,
            app_label: opt.label.clone(),
            filesystem,
            message: format!(
                "{} is installed, but its sandbox can't see the drive this game is on ({word}). Fable can open just that drive to {} — nothing else changes, and you can take it back any time in Preferences → ROMs.",
                opt.label, opt.label
            ),
        };
    }
    let view_root = prelude.settings.view_root_path();
    match launch_and_note(state, opt, layout, &view_root, &rom_path) {
        Ok(game_state) => EmuPlayOutcome::Launched {
            state: game_state,
            option_id: option_id.to_string(),
            played_provider_key: rom_path,
        },
        Err(e) => EmuPlayOutcome::Unavailable { message: e.message },
    }
}

// ---------------------------------------------------------------------------
// Commands.
// ---------------------------------------------------------------------------

/// Play was pressed. Either the game launches (saved choice, ready)
/// or the ask-once sheet's data comes back for the UI to render.
#[tauri::command]
pub async fn emu_play(
    app: tauri::AppHandle,
    provider_key: String,
    platform: String,
) -> Cmd<EmuPlayOutcome> {
    tauri::async_runtime::spawn_blocking(move || {
        let guard = app.state::<AppState>();
        let state: &AppState = &guard;
        match play_prelude(state, &platform, &provider_key) {
            Ok(p) => {
                let saved = p.settings.choices.get(&platform).map(|s| s.as_str());
                match decide(p.sys, &p.env, saved) {
                    Decision::PlayNow { option_id } => {
                        let opt = p.sys.option(&option_id).unwrap().clone();
                        // The BIOS gate (§6): a saved choice whose
                        // system file went missing (or was never
                        // filed) gets the sheet's explanation —
                        // never a launch into a black screen.
                        if state_of(p.sys, &opt, &p.env, p.layout.as_ref())
                            == OptState::NeedsBios
                        {
                            let sheet = sheet_wire(
                                p.sys,
                                &p.group,
                                &provider_key,
                                &p.env,
                                p.layout.as_ref(),
                                &option_id,
                                "ask",
                            );
                            return EmuPlayOutcome::Sheet { sheet };
                        }
                        finish_launch(state, &p, &opt, p.layout.as_ref(), &option_id)
                    }
                    Decision::Sheet { preselect } => {
                        let sheet = sheet_wire(
                            p.sys,
                            &p.group,
                            &provider_key,
                            &p.env,
                            p.layout.as_ref(),
                            &preselect,
                            "ask",
                        );
                        EmuPlayOutcome::Sheet { sheet }
                    }
                }
            }
            Err(outcome) => outcome,
        }
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("play task failed: {e}"))))
}

/// "Play with…" — the same sheet in picker mode (all options; the
/// UI adds "just this once / always" + the OS-default escape).
#[tauri::command]
pub async fn emu_sheet(
    app: tauri::AppHandle,
    provider_key: String,
    platform: String,
) -> Cmd<SheetWire> {
    tauri::async_runtime::spawn_blocking(move || {
        let guard = app.state::<AppState>();
        let state: &AppState = &guard;
        let prelude = play_prelude(state, &platform, &provider_key).map_err(|o| match o {
            EmuPlayOutcome::Unavailable { message } => {
                CommandError::from(UfmError::Internal(message))
            }
            _ => CommandError::from(UfmError::Internal("unavailable".into())),
        })?;
        let saved = prelude.settings.choices.get(&platform).map(|s| s.as_str());
        let preselect = saved
            .map(|s| s.to_string())
            .or_else(|| prelude.sys.default_option.clone())
            .unwrap_or_default();
        Ok(sheet_wire(
            prelude.sys,
            &prelude.group,
            &provider_key,
            &prelude.env,
            prelude.layout.as_ref(),
            &preselect,
            "picker",
        ))
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("sheet task failed: {e}"))))?
}

/// The sheet's primary button: install (if needed) with progress,
/// remember the choice when asked, then launch in the same sitting.
#[tauri::command]
pub async fn emu_install_and_play(
    app: tauri::AppHandle,
    provider_key: String,
    platform: String,
    option_id: String,
    remember: bool,
) -> Cmd<EmuPlayOutcome> {
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let guard = app.state::<AppState>();
        let state: &AppState = &guard;
        let prelude = match play_prelude(state, &platform, &provider_key) {
            Ok(p) => p,
            Err(o) => return o,
        };
        let Some(opt) = prelude.sys.option(&option_id).cloned() else {
            return EmuPlayOutcome::Unavailable {
                message: "That emulator isn't on the list for this system.".to_string(),
            };
        };
        let progress = |label: &str, pct: Option<u32>| {
            let _ = app2.emit(
                "emu-install-progress",
                serde_json::json!({ "label": label, "pct": pct }),
            );
        };
        let st = state_of(prelude.sys, &opt, &prelude.env, prelude.layout.as_ref());
        let mut layout = prelude.layout.clone();
        if st.needs_install() {
            match ensure_option(&opt, &progress) {
                Ok((_, l)) => layout = l,
                Err(e) => return EmuPlayOutcome::Unavailable { message: e },
            }
        } else if st == OptState::ManualOnly {
            return EmuPlayOutcome::Unavailable {
                message: opt.install_note.clone().unwrap_or_else(|| {
                    "That emulator isn't installed, and Fable can't install it yet.".to_string()
                }),
            };
        }
        if remember {
            let mut settings = load_settings(&state.data_base);
            settings
                .choices
                .insert(platform.clone(), option_id.clone());
            let _ = save_settings(&state.data_base, &settings);
        }
        // The BIOS gate (§6), after install and BEFORE launch:
        // re-probe (the install changed the machine) and, when a
        // required system file is still missing, hand back a fresh
        // sheet whose BIOS row offers the Add flow — the game is
        // never launched into a black screen.
        let (env2, layout2) = probe_env();
        if let Some(l) = layout2 {
            layout = Some(l);
        }
        if state_of(prelude.sys, &opt, &env2, layout.as_ref()) == OptState::NeedsBios {
            let sheet = sheet_wire(
                prelude.sys,
                &prelude.group,
                &provider_key,
                &env2,
                layout.as_ref(),
                &option_id,
                "ask",
            );
            return EmuPlayOutcome::Sheet { sheet };
        }
        finish_launch(state, &prelude, &opt, layout.as_ref(), &option_id)
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("install task failed: {e}"))))
}

/// The §4.4 consent, accepted: apply the scoped grant for the
/// game's drive, record it, and launch in the same sitting.
#[tauri::command]
pub async fn emu_grant_access_and_play(
    app: tauri::AppHandle,
    provider_key: String,
    platform: String,
    option_id: String,
    remember: bool,
) -> Cmd<EmuPlayOutcome> {
    tauri::async_runtime::spawn_blocking(move || {
        let guard = app.state::<AppState>();
        let state: &AppState = &guard;
        let prelude = match play_prelude(state, &platform, &provider_key) {
            Ok(p) => p,
            Err(o) => return o,
        };
        let Some(opt) = prelude.sys.option(&option_id).cloned() else {
            return EmuPlayOutcome::Unavailable {
                message: "That emulator isn't on the list for this system.".to_string(),
            };
        };
        let rom_path = prelude
            .launch_row
            .item
            .provider_key
            .clone()
            .unwrap_or_default();
        if let Some((app_id, filesystem, _word)) = access_gap(state, &opt, &rom_path) {
            if let Err(e) = apply_sandbox_override(&app_id, &filesystem) {
                return EmuPlayOutcome::Unavailable { message: e };
            }
            if let Err(e) = record_override(&state.data_base, &app_id, &filesystem) {
                return EmuPlayOutcome::Unavailable { message: e };
            }
        }
        if remember {
            let mut settings = load_settings(&state.data_base);
            settings.choices.insert(platform.clone(), option_id.clone());
            let _ = save_settings(&state.data_base, &settings);
        }
        match finish_launch(state, &prelude, &opt, prelude.layout.as_ref(), &option_id) {
            // The grant landed but the sandbox still can't see
            // the game — say so plainly instead of looping the
            // consent sheet.
            EmuPlayOutcome::AccessNeeded { .. } => EmuPlayOutcome::Unavailable {
                message: "Fable opened the drive, but the emulator still can't see the game — nothing else is affected, and the access you allowed is listed in Preferences → ROMs if you'd like to take it back."
                    .to_string(),
            },
            other => other,
        }
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("access task failed: {e}"))))
}

/// Preferences → ROMs: take back one recorded drive-access grant.
#[tauri::command]
pub async fn emu_remove_override(
    app: tauri::AppHandle,
    app_id: String,
    filesystem: String,
) -> Cmd<()> {
    tauri::async_runtime::spawn_blocking(move || {
        let guard = app.state::<AppState>();
        let state: &AppState = &guard;
        remove_sandbox_override(&app_id, &filesystem)
            .map_err(|e| CommandError::from(UfmError::Internal(e)))?;
        forget_override(&state.data_base, &app_id, &filesystem)
            .map_err(|e| CommandError::from(UfmError::Internal(e)))
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("override task failed: {e}"))))?
}

/// Preferences → ROMs → System files: every requirement system's
/// present/missing state, read from the emulator's expected
/// location (the active RetroArch flavor's system folder).
#[tauri::command]
pub async fn emu_bios_state(app: tauri::AppHandle) -> Cmd<Vec<BiosSystemWire>> {
    tauri::async_runtime::spawn_blocking(move || {
        let guard = app.state::<AppState>();
        let state: &AppState = &guard;
        let (_, layout) = probe_env();
        let settings = load_settings(&state.data_base);
        Ok(bios_state_wires(layout.as_ref(), &settings))
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("bios state task failed: {e}"))))?
}

/// The Add flow (§6): a file picker → the man's own system files,
/// filed under the right names in the emulator's system folder
/// and verified by a fresh state read (see `add_bios_files`).
/// Fable never downloads these files — they come from consoles
/// he owns. Available from the sheet's BIOS row and the
/// Preferences card alike (one command, both doors).
///
/// The picker is driven from Rust (the dialog plugin's blocking
/// API) rather than the webview: the app defines no capability
/// ACL for plugin commands, and Rust-side dialog calls are the
/// trusted side of that line.
#[tauri::command]
pub async fn emu_bios_add(app: tauri::AppHandle, system_id: String) -> Cmd<BiosAddWire> {
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        use tauri_plugin_dialog::DialogExt;
        let guard = app.state::<AppState>();
        let state: &AppState = &guard;
        let req = ufm_emu::requirement_for(&system_id).ok_or_else(|| {
            CommandError::from(UfmError::NotFound(format!(
                "no system files tracked for {system_id}"
            )))
        })?;
        let (_, layout) = probe_env();
        let settings = load_settings(&state.data_base);
        // PS3 system software is a hand-off, not a file copy (v2).
        if system_id == "ps3" {
            return ps3_firmware_add(&app, &app2, req, layout.as_ref());
        }
        let picked = app
            .dialog()
            .file()
            .set_title(format!("Choose the {} system file", req.title))
            .add_filter("System files", &["bin", "img", "rom", "pce", "zip", "dat"])
            .blocking_pick_files();
        // The card judges the effective option's home; the Add
        // flow files there first, then mirrors into the other
        // home (RetroArch's system dir) when it differs.
        let root = card_bios_root(req, layout.as_ref(), &settings);
        match picked {
            None => Ok(BiosAddWire {
                cancelled: true,
                system: bios_system_wire(req, &root),
                filed: Vec::new(),
                rejected: Vec::new(),
            }),
            Some(files) => {
                let paths: Vec<PathBuf> = files
                    .into_iter()
                    .filter_map(|f| f.into_path().ok())
                    .collect();
                let mut wire = add_bios_files(&system_id, &paths, &root)
                    .map_err(|e| CommandError::from(UfmError::Internal(e)))?;
                for extra in bios_add_roots(&system_id, layout.as_ref(), &settings) {
                    if extra != root {
                        let _ = add_bios_files(&system_id, &paths, &extra);
                    }
                }
                // Judge the result against the card's home, fresh.
                wire.system = bios_system_wire(req, &root);
                Ok(wire)
            }
        }
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("bios add task failed: {e}"))))?
}

/// The PS3 Add flow (v2): his PS3UPDAT.PUP is handed to RPCS3's
/// own firmware installer — Fable never unpacks Sony's update
/// file itself. RPCS3 is installed first (through the same
/// machinery as Play) when it isn't here yet.
fn ps3_firmware_add(
    app: &tauri::AppHandle,
    progress_app: &tauri::AppHandle,
    req: &ufm_emu::BiosRequirement,
    layout: Option<&RaLayout>,
) -> Cmd<BiosAddWire> {
    use tauri_plugin_dialog::DialogExt;
    let _ = layout;
    let root = rpcs3_config_dir();
    let picked = app
        .dialog()
        .file()
        .set_title("Choose the PS3 system software update (PS3UPDAT.PUP)")
        .add_filter("PS3 update file", &["PUP", "pup"])
        .blocking_pick_file();
    let Some(file) = picked else {
        return Ok(BiosAddWire {
            cancelled: true,
            system: bios_system_wire(req, &root),
            filed: Vec::new(),
            rejected: Vec::new(),
        });
    };
    let path = file
        .into_path()
        .map_err(|e| CommandError::from(UfmError::Internal(format!("picked file: {e}"))))?;
    let picked_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let reject = |reason: String| BiosAddWire {
        cancelled: false,
        system: bios_system_wire(req, &root),
        filed: Vec::new(),
        rejected: vec![BiosRejectedWire {
            picked: picked_name.clone(),
            reason,
        }],
    };
    if !picked_name.eq_ignore_ascii_case("PS3UPDAT.PUP") {
        return Ok(reject(
            "That isn't Sony's PS3 update file — it has to be named PS3UPDAT.PUP.".to_string(),
        ));
    }
    let sys = ufm_emu::table::system_by_id("ps3")
        .ok_or_else(|| CommandError::from(UfmError::NotFound("ps3 system".into())))?;
    let opt = sys
        .option("rpcs3-standalone")
        .cloned()
        .ok_or_else(|| CommandError::from(UfmError::NotFound("rpcs3 option".into())))?;
    let (env, _) = probe_env();
    if option_state(&opt, &env) != OptState::Ready {
        let progress = |label: &str, pct: Option<u32>| {
            let _ = progress_app.emit(
                "emu-install-progress",
                serde_json::json!({ "label": label, "pct": pct }),
            );
        };
        ensure_option(&opt, &progress)
            .map_err(|e| CommandError::from(UfmError::Internal(e)))?;
    }
    let Some(cmdline) = standalone_command(&opt) else {
        return Ok(reject(
            "RPCS3 isn't available, so the firmware can't be installed yet.".to_string(),
        ));
    };
    let mut argv: Vec<String> = ufm_emu::expand::tokenize(&cmdline);
    argv.push("--installfw".to_string());
    argv.push(path.to_string_lossy().into_owned());
    // RPCS3 installs firmware inside its own UI process — it
    // may show progress, finish and exit, or land in its main
    // window. So: spawn it, then watch the dev_flash payload
    // for the landing instead of waiting on an exit code that
    // may never be the signal. (Proven on this build: a bad
    // path leaves RPCS3 sitting in a dialog, happily alive.)
    let spawned = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            return Ok(reject(format!(
                "RPCS3 couldn't start to install the firmware: {e}"
            )))
        }
    };
    let deadline = Instant::now() + Duration::from_secs(150);
    loop {
        if bios_satisfied(req, &root) {
            return Ok(BiosAddWire {
                cancelled: false,
                system: bios_system_wire(req, &root),
                filed: vec![BiosFiledWire {
                    picked: picked_name,
                    filed_as: "PS3 system software (installed by RPCS3)".to_string(),
                }],
                rejected: Vec::new(),
            });
        }
        match child.try_wait() {
            Ok(Some(_)) => {
                return Ok(reject(
                    "RPCS3 ran, but the firmware didn't land — the file has to be Sony's official PS3UPDAT.PUP."
                        .to_string(),
                ))
            }
            Ok(None) => {
                if Instant::now() > deadline {
                    // Still working over there (or waiting on
                    // its own dialog) — leave it running: killing
                    // a firmware install mid-flight would be the
                    // one truly bad outcome.
                    return Ok(reject(
                        "RPCS3 is still holding the file — if its window is open, let it finish there. This card flips to Ready on its own once the firmware lands."
                            .to_string(),
                    ));
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            Err(_) => {
                return Ok(reject(
                    "Fable lost track of RPCS3 while it worked — check its window; this card flips to Ready once the firmware lands."
                        .to_string(),
                ))
            }
        }
    }
}

/// Open the chosen emulator with no game (the sheet footer's
/// "Set up my controller" — the emulator's own input setup owns
/// controller configuration; Fable builds no mapper in v1).
#[tauri::command]
pub async fn emu_open_emulator(app: tauri::AppHandle, platform: String) -> Cmd<()> {
    tauri::async_runtime::spawn_blocking(move || {
        let guard = app.state::<AppState>();
        let state: &AppState = &guard;
        let system_id = esde_id(&platform);
        let sys = ufm_emu::table::system_by_id(&system_id)
            .ok_or_else(|| CommandError::from(UfmError::NotFound("unknown system".into())))?;
        let settings = load_settings(&state.data_base);
        let chosen = settings
            .choices
            .get(&platform)
            .and_then(|id| sys.option(id))
            .or_else(|| sys.default())
            .ok_or_else(|| CommandError::from(UfmError::NotFound("no emulator".into())))?;
        let (_, layout) = probe_env();
        match chosen.kind {
            OptionKind::LibretroCore => {
                let layout = layout.ok_or_else(|| {
                    CommandError::from(UfmError::NotFound("RetroArch isn't installed".into()))
                })?;
                // Opening RetroArch bare lands in its menu, where
                // input setup lives. The 900ms liveness check in
                // spawn_launch doesn't apply — the menu staying up
                // is success, so spawn without the check.
                use std::os::unix::process::CommandExt;
                Command::new(&layout.command[0])
                    .args(&layout.command[1..])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .process_group(0)
                    .spawn()
                    .map(|_| ())
                    .map_err(|e| {
                        CommandError::from(UfmError::Internal(format!(
                            "couldn't start the emulator: {e}"
                        )))
                    })
            }
            OptionKind::Standalone => {
                // v2: the resolved channel command (flatpak run /
                // managed AppImage / PATH binary), tokenized the
                // same way launches are.
                let cmdline = standalone_command(chosen).ok_or_else(|| {
                    CommandError::from(UfmError::NotFound(
                        "that emulator isn't installed".into(),
                    ))
                })?;
                let argv = ufm_emu::expand::tokenize(&cmdline);
                if argv.is_empty() {
                    return Err(CommandError::from(UfmError::NotFound(
                        "that emulator isn't installed".into(),
                    )));
                }
                spawn_launch(&argv, None)
                    .map_err(|e| CommandError::from(UfmError::Internal(e)))
            }
            _ => Err(CommandError::from(UfmError::InvalidInput(
                "that option doesn't open on its own".into(),
            ))),
        }
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("open task failed: {e}"))))?
}

// ---------------------------------------------------------------------------
// Acceptance harness (box-only, ignored by default): drives the
// real cold-start path — probe, decide, install, launch — plus the
// keeper against the real catalog. See BUILD-CHECKPOINT.md.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_keys_pick_the_newest_rpcs3_asset() {
        assert!(
            appimage_version_key("rpcs3-v0.0.37-18146-ffeb16fe_linux64.AppImage")
                > appimage_version_key("rpcs3-v0.0.18-12817-fff0c96b_linux64.AppImage")
        );
        assert!(
            appimage_version_key("rpcs3-v0.0.38-18434-3c747b37_linux64.AppImage")
                > appimage_version_key("rpcs3-v0.0.37-18146-ffeb16fe_linux64.AppImage")
        );
    }

    #[test]
    fn filesystem_lines_parse_flatpaks_ini_shape() {
        let meta = "[Application]\nname=org.example.App\n[Context]\nfilesystems=home;/media;xdg-run/pipewire-0;\nsockets=wayland;\n";
        let grants = parse_filesystems_line(meta);
        assert_eq!(grants, vec!["home", "/media", "xdg-run/pipewire-0"]);
        assert!(parse_filesystems_line("[Context]\nsockets=wayland;\n").is_empty());
    }

    #[test]
    fn grant_coverage_follows_flatpaks_rules() {
        let home = home_dir();
        let rom = home.join("Games/psx/game.cue");
        assert!(grant_covers(&["host".into()], &rom));
        assert!(grant_covers(&["home".into()], &rom));
        assert!(grant_covers(&["home:ro".into()], &rom));
        assert!(!grant_covers(&["home".into()], Path::new("/media/Desk SSD/roms/x.nes")));
        assert!(grant_covers(
            &["/media/Desk SSD".into()],
            Path::new("/media/Desk SSD/roms/x.nes")
        ));
        // A grant for one drive doesn't leak onto another.
        assert!(!grant_covers(
            &["/media/Desk SSD".into()],
            Path::new("/media/Other/roms/x.nes")
        ));
        assert!(grant_covers(
            &["~/Games".into()],
            &home.join("Games/psx/game.cue")
        ));
        assert!(!grant_covers(&[], &rom));
    }
}

#[cfg(test)]
mod acceptance {
    use super::*;

    #[test]
    #[ignore]
    fn cold_start_acceptance() {
        if std::env::var("FABLE_EMU_ACCEPT").is_err() {
            return;
        }
        let rom = std::env::var("FABLE_EMU_ROM").expect("FABLE_EMU_ROM");
        let platform = std::env::var("FABLE_EMU_PLATFORM").unwrap_or_else(|_| "nes".into());
        let sys = ufm_emu::table::system_by_id(&esde_id(&platform)).expect("system");
        let (env, layout) = probe_env();
        eprintln!("PROBE: ra={:?} cores={} flatpak={}", env.ra, env.cores.len(), env.flatpak_available);
        match decide(sys, &env, None) {
            Decision::Sheet { preselect } => {
                eprintln!("DECISION: sheet, preselect={preselect}");
                let opt = sys.option(&preselect).unwrap().clone();
                let progress = |label: &str, pct: Option<u32>| {
                    eprintln!("[install] {label} {pct:?}")
                };
                let (_, layout2) = ensure_option(&opt, &progress).expect("install must succeed");
                let (argv, cwd) =
                    launch_argv(&opt, layout2.as_ref(), &default_view_root(), &rom).unwrap();
                eprintln!("ARGV: {argv:?} cwd={cwd:?}");
                spawn_launch(&argv, cwd.as_deref()).expect("emulator must start and stay up");
                eprintln!("LAUNCHED AND STAYED UP");
            }
            Decision::PlayNow { option_id } => {
                eprintln!("DECISION: play now via {option_id}");
                let opt = sys.option(&option_id).unwrap().clone();
                let (argv, cwd) =
                    launch_argv(&opt, layout.as_ref(), &default_view_root(), &rom).unwrap();
                eprintln!("ARGV: {argv:?} cwd={cwd:?}");
                spawn_launch(&argv, cwd.as_deref()).expect("emulator must start and stay up");
                eprintln!("LAUNCHED AND STAYED UP");
            }
        }
    }

    /// v1.1 pipeline proof for every core family: the sheet
    /// decision preselects the amendment's default, the default's
    /// core fetches through the v1 machinery, and the launch argv
    /// expands. BIOS-gated families assert the gate instead (their
    /// launch proof is the designed refusal — no BIOS exists on
    /// this box, by law Fable never fetches one).
    #[test]
    #[ignore]
    fn families_pipeline_acceptance() {
        if std::env::var("FABLE_EMU_ACCEPT").is_err() {
            return;
        }
        const FAMILIES: &[&str] = &[
            "gb", "gbc", "gba", "n64", "nds", "genesis", "mastersystem", "gamegear",
            "sg-1000", "segacd", "sega32x", "pcengine", "pcenginecd", "tg16", "saturn",
            "dreamcast", "arcade", "neogeo", "ngpc", "atari2600", "atari7800",
            "atari5200", "atarilynx", "colecovision", "intellivision", "odyssey2",
            "vectrex", "virtualboy", "wonderswan", "wonderswancolor", "3do", "pcfx",
            "neogeocd", "msx", "c64", "amigacd32", "channelf", "psx",
        ];
        let progress = |label: &str, pct: Option<u32>| eprintln!("[install] {label} {pct:?}");
        for id in FAMILIES {
            let sys = ufm_emu::table::system_by_id(id).unwrap_or_else(|| panic!("{id} in table"));
            let (env, layout) = probe_env();
            let chosen = match decide(sys, &env, None) {
                Decision::Sheet { preselect } => preselect,
                Decision::PlayNow { option_id } => option_id,
            };
            assert_eq!(
                chosen,
                sys.default_option.clone().unwrap_or_default(),
                "{id}: the decision must land on the curated default"
            );
            let opt = sys.option(&chosen).unwrap().clone();
            let req = ufm_emu::requirement_for(id);
            let gated = req.map(|r| r.gate_play).unwrap_or(false)
                && !bios_satisfied(req.unwrap(), &bios_system_root(layout.as_ref()));
            let (_, layout2) = ensure_option(&opt, &progress)
                .unwrap_or_else(|e| panic!("{id}: core install failed: {e}"));
            let (env2, _) = probe_env();
            let st = state_of(sys, &opt, &env2, layout2.as_ref());
            if gated {
                assert_eq!(st, OptState::NeedsBios, "{id}: must gate on the missing BIOS");
                eprintln!("FAMILY {id}: core fetched; GATE OK (NeedsBios — no launch by design)");
            } else {
                assert_eq!(st, OptState::Ready, "{id}: must read Ready after install");
                let (argv, cwd) = launch_argv(
                    &opt,
                    layout2.as_ref(),
                    &default_view_root(),
                    "/tmp/fable-accept-rom",
                )
                .unwrap_or_else(|e| panic!("{id}: argv expansion failed: {e}"));
                eprintln!("FAMILY {id}: READY argv={argv:?} cwd={cwd:?}");
            }
        }
    }

    /// v1.1 BIOS gate proof: on this (BIOS-less) box, gated systems
    /// read NeedsBios through the sheet wire with the Add row
    /// populated — the refusal IS the feature (§6).
    #[test]
    #[ignore]
    fn bios_gate_acceptance() {
        if std::env::var("FABLE_EMU_ACCEPT").is_err() {
            return;
        }
        for id in ["psx", "saturn", "channelf", "atarilynx", "odyssey2"] {
            let sys = ufm_emu::table::system_by_id(id).unwrap();
            let req = ufm_emu::requirement_for(id).unwrap();
            let (env, layout) = probe_env();
            let root = bios_system_root(layout.as_ref());
            let def = sys.default().unwrap();
            let st = state_of(sys, def, &env, layout.as_ref());
            if bios_satisfied(req, &root) {
                eprintln!("BIOS {id}: files present on this box — state {st:?}");
                continue;
            }
            let wire = bios_gate_wire(sys, def, layout.as_ref(), st);
            let wire = wire.unwrap_or_else(|| panic!("{id}: sheet must carry the BIOS row"));
            assert!(wire.blocking || st.needs_install(), "{id}: row blocks or install precedes");
            eprintln!("BIOS {id}: state={st:?} missing={:?}", wire.files);
        }
    }

    /// v1.1 Add-flow proof (pure fs, temp dirs): right names file
    /// under canonical names, wrong names are refused, sets flip
    /// satisfied only when complete.
    #[test]
    #[ignore]
    fn bios_add_acceptance() {
        let base = std::env::temp_dir().join(format!("fable-bios-accept-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("system");
        let src = base.join("picked");
        std::fs::create_dir_all(&src).unwrap();
        let dummy = |name: &str, bytes: usize| -> PathBuf {
            let p = src.join(name);
            std::fs::write(&p, vec![7u8; bytes]).unwrap();
            p
        };
        // PlayStation: an uppercase regional file files canonically;
        // a stranger is refused with a reason.
        let out = add_bios_files(
            "psx",
            &[dummy("SCPH5501.BIN", 2048), dummy("holiday.jpg", 512)],
            &root,
        )
        .unwrap();
        assert_eq!(out.filed.len(), 1);
        assert_eq!(out.filed[0].filed_as, "scph5501.bin");
        assert_eq!(out.rejected.len(), 1);
        assert!(out.system.satisfied);
        assert!(root.join("scph5501.bin").is_file());
        // Channel F: two of three isn't satisfied; the third completes it.
        let out = add_bios_files(
            "channelf",
            &[dummy("sl31253.bin", 1024), dummy("sl31254.bin", 1024)],
            &root,
        )
        .unwrap();
        assert!(!out.system.satisfied);
        assert_eq!(out.system.sets[0].missing, vec!["sl90025.bin".to_string()]);
        let out = add_bios_files("channelf", &[dummy("sl90025.bin", 1024)], &root).unwrap();
        assert!(out.system.satisfied);
        // Dreamcast: the pair files into the dc/ corner.
        let out = add_bios_files(
            "dreamcast",
            &[dummy("dc_boot.bin", 1024), dummy("dc_flash.bin", 1024)],
            &root,
        )
        .unwrap();
        assert!(out.system.satisfied);
        assert!(root.join("dc/dc_boot.bin").is_file());
        // Amiga CD32: a Kickstart keeps its own (versioned) name.
        let out = add_bios_files("amigacd32", &[dummy("kick40060.CD32", 1024)], &root).unwrap();
        assert!(out.system.satisfied);
        assert_eq!(out.filed[0].filed_as, "kick40060.CD32");
        let _ = std::fs::remove_dir_all(&base);
        eprintln!("BIOS ADD: all filing rules hold");
    }

    #[test]
    #[ignore]
    fn keeper_acceptance() {
        let catalog = std::env::var("FABLE_EMU_CATALOG").expect("FABLE_EMU_CATALOG");
        let data = std::env::var("FABLE_EMU_DATA").expect("FABLE_EMU_DATA");
        let report = crate::emu_farm::run_keeper(Path::new(&catalog), Path::new(&data));
        eprintln!("KEEPER: {report:#?}");
        assert!(report.error.is_none());
        // The view is never a source (v2): after a pass, NO farm
        // link may point inside the view root — the self-loop
        // class ES-DE's scan exposed (1,047 links on this box
        // before the fix). Every link must resolve to a real file.
        let settings = load_settings(Path::new(&data));
        let root = settings.view_root_path();
        let mut bad = 0;
        for system in ["nes", "snes"] {
            let dir = root.join(system);
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for e in entries.flatten() {
                    let p = e.path();
                    if std::fs::symlink_metadata(&p)
                        .map(|m| m.file_type().is_symlink())
                        .unwrap_or(false)
                    {
                        let target = std::fs::read_link(&p).unwrap_or_default();
                        if target.starts_with(&root) || !p.exists() {
                            bad += 1;
                            eprintln!("MIRROR/DANGLING: {} -> {}", p.display(), target.display());
                        }
                    }
                }
            }
        }
        assert_eq!(bad, 0, "no farm link may point at the view itself or dangle");
    }

    #[test]
    #[ignore]
    fn keeper_unplug_simulation() {
        // Reconcile against a sighting set missing the Desk SSD:
        // its games' links must repoint (surviving copy) or retire —
        // and NO link may dangle afterwards.
        let catalog = std::env::var("FABLE_EMU_CATALOG").expect("FABLE_EMU_CATALOG");
        let data = std::env::var("FABLE_EMU_DATA").expect("FABLE_EMU_DATA");
        let drop = std::env::var("FABLE_EMU_DROP_MOUNT").expect("FABLE_EMU_DROP_MOUNT");
        let all = crate::commands::current_filesystems();
        let reduced: Vec<_> = all
            .iter()
            .filter(|v| v.path != drop)
            .cloned()
            .collect();
        eprintln!("SIMULATION: dropping mount {drop} ({} -> {} filesystems)", all.len(), reduced.len());
        let report = crate::emu_farm::run_keeper_with_mounts(
            Path::new(&catalog),
            Path::new(&data),
            &reduced,
        );
        eprintln!("KEEPER-SIM: {report:#?}");
        // No dangling links anywhere in the farm afterwards.
        let settings = load_settings(Path::new(&data));
        let root = settings.view_root_path();
        let mut dangling = 0;
        for system in ["nes", "snes"] {
            let dir = root.join(system);
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for e in entries.flatten() {
                    let p = e.path();
                    if std::fs::symlink_metadata(&p).map(|m| m.file_type().is_symlink()).unwrap_or(false)
                        && !p.exists()
                    {
                        dangling += 1;
                        eprintln!("DANGLING: {}", p.display());
                    }
                }
            }
        }
        assert_eq!(dangling, 0, "links must never dangle after an unplug reconcile");
    }

    /// v2 BIOS gate for standalones: on a box with NO dumped
    /// system files anywhere, the three gated standalone defaults
    /// read NeedsBios against their OWN homes (never the
    /// RetroArch dir), an ungated standalone stays Ready, and the
    /// sheet's BIOS row exists for the gated rows.
    #[test]
    #[ignore]
    fn standalone_bios_gate_acceptance() {
        if std::env::var("FABLE_EMU_ACCEPT").is_err() {
            return;
        }
        let (env, layout) = probe_env();
        for (system, option_id) in [
            ("psx", "duckstation-standalone"),
            ("ps2", "pcsx2-standalone"),
            ("ps3", "rpcs3-standalone"),
        ] {
            let sys = ufm_emu::table::system_by_id(system).unwrap();
            let opt = sys.option(option_id).unwrap();
            let req = ufm_emu::requirement_for(system).unwrap();
            let root = standalone_bios_root(system, opt).expect("known home");
            if bios_satisfied(req, &root) {
                eprintln!("BIOS {system}: files present in {} — gate open", root.display());
                continue;
            }
            let st = state_of(sys, opt, &env, layout.as_ref());
            assert_eq!(
                st,
                OptState::NeedsBios,
                "{system}: installed standalone with no system files must gate"
            );
            let wire = bios_gate_wire(sys, opt, layout.as_ref(), st)
                .unwrap_or_else(|| panic!("{system}: sheet must carry the BIOS row"));
            assert!(wire.blocking, "{system}: BIOS row blocks");
            eprintln!("BIOS {system}: NeedsBios via {} — {:?}", root.display(), wire.files);
        }
        // An ungated standalone default never gates.
        let sys = ufm_emu::table::system_by_id("gc").unwrap();
        let opt = sys.option("dolphin-standalone").unwrap();
        assert_eq!(
            state_of(sys, opt, &env, layout.as_ref()),
            OptState::Ready,
            "gc: Dolphin has no system-file requirement"
        );
        eprintln!("BIOS standalone gate: layering holds");
    }

    /// v2 standalone wave: each default installs through FABLE's
    /// own `ensure_option` (the same call the sheet's Install &
    /// Play makes), reads Ready on a fresh probe, and expands a
    /// launch argv whose program + flags are real. Spawn-alive
    /// proofs are the separate test below (they need his desktop
    /// session env exported for the test binary).
    #[test]
    #[ignore]
    fn standalone_wave_acceptance() {
        if std::env::var("FABLE_EMU_ACCEPT").is_err() {
            return;
        }
        let progress = |label: &str, pct: Option<u32>| {
            eprintln!("INSTALL: {label} {:?}", pct);
        };
        let cases: &[(&str, &str, &str)] = &[
            ("psx", "duckstation-standalone", "/tmp/fable-standin-psx.cue"),
            ("gc", "dolphin-standalone", "/tmp/fable-standin-gc.iso"),
            ("wii", "dolphin-standalone", "/tmp/fable-standin-wii.iso"),
            ("ps2", "pcsx2-standalone", "/tmp/fable-standin-ps2.iso"),
            ("ps3", "rpcs3-standalone", "/tmp/fable-standin-ps3.iso"),
            ("psp", "ppsspp-standalone", "/tmp/fable-standin-psp.iso"),
            ("n3ds", "azahar-standalone", "/tmp/fable-standin-3ds.3ds"),
        ];
        for (system, option_id, standin) in cases {
            let sys = ufm_emu::table::system_by_id(system).unwrap();
            let opt = sys.option(option_id).unwrap().clone();
            ensure_option(&opt, &progress)
                .unwrap_or_else(|e| panic!("{system}: Fable-path install failed: {e}"));
            let (env, layout) = probe_env();
            assert_eq!(
                option_state(&opt, &env),
                OptState::Ready,
                "{system}: option must read Ready after install"
            );
            let (argv, _cwd) = launch_argv(&opt, layout.as_ref(), Path::new("/tmp"), standin)
                .unwrap_or_else(|e| panic!("{system}: argv: {e}"));
            eprintln!("WAVE {system}: ready; argv = {argv:?}");
            assert!(!argv.is_empty());
            assert!(
                !argv.iter().any(|a| a.contains("%EMULATOR_")),
                "{system}: template fully resolved"
            );
        }
    }

    /// v2 spawn-alive proofs: launch each installed standalone
    /// BARE (its own UI, no game), require it alive after a
    /// settle window, then kill it by handle. Proves the artifact
    /// Fable installed actually runs on this machine. RPCS3's
    /// template carries --no-gui + a ROM, so its bare proof is
    /// the AppImage itself (no template args).
    #[test]
    #[ignore]
    fn standalone_spawn_acceptance() {
        if std::env::var("FABLE_EMU_ACCEPT").is_err() {
            return;
        }
        let cases: &[(&str, &str)] = &[
            ("psx", "duckstation-standalone"),
            ("gc", "dolphin-standalone"),
            ("ps2", "pcsx2-standalone"),
            ("ps3", "rpcs3-standalone"),
            ("psp", "ppsspp-standalone"),
            ("n3ds", "azahar-standalone"),
        ];
        for (system, option_id) in cases {
            let sys = ufm_emu::table::system_by_id(system).unwrap();
            let opt = sys.option(option_id).unwrap().clone();
            let cmdline = standalone_command(&opt)
                .unwrap_or_else(|| panic!("{system}: no resolved command"));
            let argv = ufm_emu::expand::tokenize(&cmdline);
            eprintln!("SPAWN {system}: {argv:?}");
            let mut child = Command::new(&argv[0])
                .args(&argv[1..])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap_or_else(|e| panic!("{system}: spawn failed: {e}"));
            std::thread::sleep(Duration::from_millis(3500));
            let alive = matches!(child.try_wait(), Ok(None));
            let _ = child.kill();
            let _ = child.wait();
            assert!(alive, "{system}: emulator exited within the settle window");
            eprintln!("SPAWN {system}: alive after 3.5s, killed by handle");
        }
    }

    /// v2 §4.4 proof against a REAL external-volume game path
    /// (FABLE_EMU_EXT_ROM): the declarative check's answer, then —
    /// when the sandbox genuinely can't see it — the full consent
    /// machinery (grant → can see → read inside the sandbox →
    /// revoke) exercised through the same fns the commands call.
    #[test]
    #[ignore]
    fn sandbox_override_acceptance() {
        if std::env::var("FABLE_EMU_ACCEPT").is_err() {
            return;
        }
        let rom = std::env::var("FABLE_EMU_EXT_ROM").expect("FABLE_EMU_EXT_ROM");
        let app_id = "org.duckstation.DuckStation";
        let grants = flatpak_filesystem_grants(app_id);
        eprintln!("SANDBOX {app_id} grants: {grants:?}");
        let before = sandbox_can_see(app_id, Path::new(&rom));
        eprintln!("SANDBOX can-see({rom}) before = {before}");
        if before {
            eprintln!("SANDBOX: shipped permissions already cover it — flow not needed here");
            return;
        }
        // The game's home volume root is what a consent grants.
        let catalog_path = std::env::var("FABLE_EMU_CATALOG").expect("FABLE_EMU_CATALOG");
        let catalog = ufm_catalog::Catalog::open(Path::new(&catalog_path)).unwrap();
        let volumes = catalog.volumes().unwrap_or_default();
        let mounted = crate::commands::current_filesystems();
        let home = home_volume(&rom, &mounted, &volumes).expect("home volume");
        assert!(home.live, "the external volume must be mounted for this proof");
        apply_sandbox_override(app_id, &home.root).expect("apply override");
        let after = sandbox_can_see(app_id, Path::new(&rom));
        eprintln!("SANDBOX can-see after grant({}) = {after}", home.root);
        assert!(after, "the grant must flip the declarative check");
        // And the sandbox really can read the file now.
        let out = run_stdout(
            "flatpak",
            &[
                "run",
                "--command=stat",
                app_id,
                "-c",
                "%s",
                &rom,
            ],
            Duration::from_secs(30),
        );
        eprintln!("SANDBOX in-sandbox stat: {out:?}");
        assert!(out.is_some(), "stat inside the sandbox must succeed");
        // Clean up after ourselves: revoke, machine as found.
        remove_sandbox_override(app_id, &home.root).expect("revoke override");
        assert!(!sandbox_can_see(app_id, Path::new(&rom)));
        eprintln!("SANDBOX: grant + read + revoke all proven");
    }

    /// v2 ES-DE door proof: install through the door's own path,
    /// point it at the farm + media tree in its OWN settings
    /// file, launch it, and read back what IT wrote (log lines
    /// naming the systems it populated). Killed by handle after.
    #[test]
    #[ignore]
    fn esde_door_acceptance() {
        if std::env::var("FABLE_EMU_ACCEPT").is_err() {
            return;
        }
        let data = std::env::var("FABLE_EMU_DATA").expect("FABLE_EMU_DATA");
        let base = Path::new(&data);
        let progress = |label: &str, pct: Option<u32>| {
            eprintln!("ESDE: {label} {:?}", pct);
        };
        let state = crate::emu_esde::install_esde(base, &progress).expect("install via Fable path");
        eprintln!("ESDE state after install: {state:?}");
        assert!(state.installed);
        // First run generates ES-DE's own home; then Fable's two
        // entries land in ES-DE's own file.
        let argv = crate::emu_esde::esde_command().expect("esde command");
        let settings = load_settings(base);
        let view_root = settings.view_root_path();
        let media_root = crate::rommedia::load_settings(base).root_path();
        if !crate::emu_esde::esde_settings_path().is_file() {
            let mut init = Command::new(&argv[0])
                .args(&argv[1..])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("init spawn");
            let deadline = Instant::now() + Duration::from_secs(40);
            while !crate::emu_esde::esde_settings_path().is_file()
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(400));
            }
            let _ = init.kill();
            let _ = init.wait();
        }
        let wrote = crate::emu_esde::write_esde_settings(&view_root, &media_root)
            .expect("write settings");
        assert!(wrote, "ES-DE's settings file must exist after its first run");
        let text = std::fs::read_to_string(crate::emu_esde::esde_settings_path()).unwrap();
        assert!(
            text.contains("MediaDirectory"),
            "Fable's media tree must be in ES-DE's settings"
        );
        eprintln!("ESDE settings carry MediaDirectory -> {}", media_root.display());
        // The real launch: alive after settle, and its log must
        // show the farm's systems populating.
        let mut child = Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("launch spawn");
        std::thread::sleep(Duration::from_millis(12000));
        let alive = matches!(child.try_wait(), Ok(None));
        let log_path = crate::emu_esde::esde_home().join("logs").join("es_log.txt");
        let log = std::fs::read_to_string(&log_path).unwrap_or_default();
        // ES-DE's own census: "loaded N systems" + "Total game
        // count: M" — the farm's games, counted by ES-DE itself.
        let census: Vec<&str> = log
            .lines()
            .filter(|l| l.contains("systems") || l.contains("Total game count"))
            .collect();
        for h in &census {
            eprintln!("ESDE LOG: {h}");
        }
        let total: u64 = log
            .lines()
            .find_map(|l| l.split("Total game count:").nth(1))
            .and_then(|t| t.trim().parse().ok())
            .unwrap_or(0);
        let _ = child.kill();
        let _ = child.wait();
        assert!(alive, "ES-DE must stay up");
        assert!(
            total > 0,
            "ES-DE's log must count the farm's games (got {total})"
        );
        assert!(
            !log.contains("Too many levels of symbolic links"),
            "the farm must never hand ES-DE a loop"
        );
        eprintln!("ESDE: door proof complete — ES-DE counted {total} games");
    }
}
