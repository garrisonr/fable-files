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
}

impl Default for EmuSettings {
    fn default() -> Self {
        Self {
            choices: BTreeMap::new(),
            view_enabled: true,
            view_root: String::new(),
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

fn run_stdout(cmd: &str, args: &[&str], timeout: Duration) -> Option<String> {
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
    run_stdout(
        "flatpak",
        &["list", "--app", "--columns=application"],
        Duration::from_secs(20),
    )
    .map(|s| s.lines().any(|l| l.trim() == app_id))
    .unwrap_or(false)
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

    // Standalone presence: probe each curated option's binaries.
    let mut standalones: BTreeSet<String> = BTreeSet::new();
    for sys in ufm_emu::systems_table() {
        for opt in &sys.options {
            if opt.kind == OptionKind::Standalone
                && opt
                    .probe_bins
                    .iter()
                    .any(|b| crate::deps::tool_present(b))
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

/// BIOS presence for an option: every required file exists in the
/// RetroArch system directory (v1 systems need none; the check is
/// wired so v1.1's BIOS systems inherit it).
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

/// One option's state here, BIOS included.
pub fn state_of(opt: &EmuOption, env: &EnvProbe, layout: Option<&RaLayout>) -> OptState {
    with_bios(option_state(opt, env), opt, bios_present(opt, layout))
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

/// Make sure RetroArch exists; returns its layout. Flatpak first
/// (amendment §4.1), apt via deps.rs as the fallback.
pub fn ensure_retroarch(progress: &Progress) -> Result<RaLayout, String> {
    let (env, layout) = probe_env();
    if let Some(l) = layout {
        return Ok(l);
    }
    if env.flatpak_available {
        progress("Installing RetroArch…", None);
        // A --user install resolves refs from USER remotes only,
        // and this box's flathub is registered system-wide — a
        // user install can't see it ("No remote refs found").
        // Register the same remote for the user first; still no
        // password, still entirely per-user.
        let user_has_flathub = run_stdout(
            "flatpak",
            &["remote-list", "--user", "--columns=name"],
            Duration::from_secs(20),
        )
        .map(|s| s.lines().any(|l| l.trim() == "flathub"))
        .unwrap_or(false);
        if !user_has_flathub {
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
                    "Fable couldn't install RetroArch: {e}. Nothing else is affected — try again from the same place."
                )
            })?;
        }
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

/// Install whatever an option needs, in dependency order, then
/// verify it reads Ready. The sheet's "Install & Play" is this
/// function followed by a launch.
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
            if state_of(opt, &env, layout2.as_ref()) != OptState::Ready {
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
                run_long(
                    "flatpak",
                    &["install", "--user", "-y", "--noninteractive", "flathub", app_id],
                    Duration::from_secs(1800),
                )
                .map_err(|e| format!("Fable couldn't install {}: {e}", opt.label))?;
                Ok(probe_env())
            }
            ufm_emu::InstallSpec::AptLogical { logical } => {
                progress(&format!("Installing {}…", opt.label), None);
                let bin = opt.probe_bins.first().cloned().unwrap_or_default();
                crate::deps::ensure_tool(&bin, logical)?;
                Ok(probe_env())
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
    // Standalone find-rules for the curated options: the probed
    // binary's name is its own command.
    for bin in &opt.probe_bins {
        if crate::deps::tool_present(bin) {
            let token = match opt.id.as_str() {
                "mesen-standalone" => "EMULATOR_MESEN",
                "snes9x-standalone" => "EMULATOR_SNES9X",
                _ => "",
            };
            if !token.is_empty() {
                emulators.insert(token.to_string(), bin.clone());
            }
            break;
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
pub fn best_present_copy<'a>(
    group: &'a GameGroup,
    mounted: &[crate::commands::MountedVolume],
    volumes: &[ufm_catalog::VolumeRecord],
) -> Option<&'a ufm_catalog::RomHomeGame> {
    group
        .rows
        .iter()
        .filter_map(|r| {
            let pk = r.item.provider_key.as_ref()?;
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
                let st = state_of(o, env, layout);
                SheetOptionWire {
                    id: o.id.clone(),
                    label: o.label.clone(),
                    verdict: o.verdict.clone(),
                    state: st,
                    state_word: st.word().to_string(),
                    needs_install: st.needs_install(),
                    class: o.class,
                    install_note: o.install_note.clone(),
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
    let Some(launch_row) = best_present_copy(&group, &mounted, &volumes).cloned() else {
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
    let settings = load_settings(&state.data_base);
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
                        let rom_path = p
                            .launch_row
                            .item
                            .provider_key
                            .clone()
                            .unwrap_or_default();
                        let view_root = p.settings.view_root_path();
                        match launch_and_note(state, &opt, p.layout.as_ref(), &view_root, &rom_path)
                        {
                            Ok(game_state) => EmuPlayOutcome::Launched {
                                state: game_state,
                                option_id,
                                played_provider_key: rom_path,
                            },
                            Err(e) => EmuPlayOutcome::Unavailable { message: e.message },
                        }
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
        let st = state_of(&opt, &prelude.env, prelude.layout.as_ref());
        let layout = if st.needs_install() {
            match ensure_option(&opt, &progress) {
                Ok((_, layout)) => layout,
                Err(e) => return EmuPlayOutcome::Unavailable { message: e },
            }
        } else if st == OptState::ManualOnly {
            return EmuPlayOutcome::Unavailable {
                message: opt.install_note.clone().unwrap_or_else(|| {
                    "That emulator isn't installed, and Fable can't install it yet.".to_string()
                }),
            };
        } else {
            prelude.layout.clone()
        };
        if remember {
            let mut settings = load_settings(&state.data_base);
            settings
                .choices
                .insert(platform.clone(), option_id.clone());
            let _ = save_settings(&state.data_base, &settings);
        }
        let rom_path = prelude
            .launch_row
            .item
            .provider_key
            .clone()
            .unwrap_or_default();
        let view_root = prelude.settings.view_root_path();
        match launch_and_note(state, &opt, layout.as_ref(), &view_root, &rom_path) {
            Ok(game_state) => EmuPlayOutcome::Launched {
                state: game_state,
                option_id,
                played_provider_key: rom_path,
            },
            Err(e) => EmuPlayOutcome::Unavailable { message: e.message },
        }
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("install task failed: {e}"))))
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
                let bin = chosen
                    .probe_bins
                    .iter()
                    .find(|b| crate::deps::tool_present(b))
                    .ok_or_else(|| {
                        CommandError::from(UfmError::NotFound(
                            "that emulator isn't installed".into(),
                        ))
                    })?;
                spawn_launch(&[bin.clone()], None)
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

    #[test]
    #[ignore]
    fn keeper_acceptance() {
        let catalog = std::env::var("FABLE_EMU_CATALOG").expect("FABLE_EMU_CATALOG");
        let data = std::env::var("FABLE_EMU_DATA").expect("FABLE_EMU_DATA");
        let report = crate::emu_farm::run_keeper(Path::new(&catalog), Path::new(&data));
        eprintln!("KEEPER: {report:#?}");
        assert!(report.error.is_none());
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
}
