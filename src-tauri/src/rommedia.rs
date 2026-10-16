//! rommedia: ROM media enrichment (Dickles, 2026-10-14/15).
//!
//! Fetches box art, screenshots, title shots and trailers for
//! DAT-identified ROMs from a cascade of community sources, and files
//! them ONCE in a canonical ES-DE-layout tree that ES-DE reads with
//! zero configuration, with a RetroArch thumbnails mirror maintained
//! from the same downloads. Design source: rom-media/MEMO.md; the
//! GUIDANCE LAW in DESIGN.md governs every user-facing string this
//! feature produces (teach, do not just fetch; free path first).
//!
//! Hard rules honored here:
//! - Credentials live ONLY in the OS keyring. They are never written
//!   to the settings file, the job snapshots, or any log. (Media
//!   URLs returned by ScreenScraper embed credentials — this module
//!   never logs a URL.)
//! - Existing files are sacred: a fetch never overwrites media that
//!   is already on disk, from any source, including the user's own
//!   hand-filed copies (an existing file at the canonical path is a
//!   "found" result with source "manual").
//! - Fuzzy name matches are never silently accepted: only an exact
//!   normalized-title match auto-accepts; anything else lands in the
//!   missing-media report for one-click human confirmation.
//! - Misses and errors are cached in the catalog (rom_media table):
//!   re-runs chase only what is still missing.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// System map: Fable dataset key -> the vocabularies of the outside
// world. Libretro directory names verified 2026-10-15 against the
// libretro-thumbnails GitHub org (repo names, underscores = spaces);
// ES-DE names verified against ES-DE's official es_systems.xml.
// ---------------------------------------------------------------------------

pub struct SystemMap {
    /// Libretro thumbnail server directories, in try order (a system
    /// can live in two repos: WonderSwan Color games sit in the Color
    /// repo, EU Odyssey² names in Videopac).
    pub libretro: &'static [&'static str],
    /// ES-DE system folder name (the canonical tree's system level).
    pub esde: &'static str,
    /// ScreenScraper system-name aliases (lowercase), matched against
    /// the API's own systemesListe when credentials exist — the id is
    /// resolved at runtime from the service's list, never guessed
    /// into a static table.
    pub ss_names: &'static [&'static str],
    /// EmuMovies directory-walk tokens (lowercase substrings).
    pub emu_tokens: &'static [&'static str],
}

pub fn system_map(sys: &str) -> Option<SystemMap> {
    let m = match sys {
        "3do" => SystemMap { libretro: &["The 3DO Company - 3DO"], esde: "3do", ss_names: &["3do"], emu_tokens: &["3do"] },
        "amiga" => SystemMap { libretro: &["Commodore - Amiga"], esde: "amiga", ss_names: &["amiga"], emu_tokens: &["amiga"] },
        "amigacd32" => SystemMap { libretro: &["Commodore - CD32"], esde: "amigacd32", ss_names: &["amiga cd32", "cd32"], emu_tokens: &["cd32", "amiga"] },
        "arcade" => SystemMap { libretro: &["MAME", "FBNeo - Arcade Games"], esde: "arcade", ss_names: &["mame", "arcade"], emu_tokens: &["mame", "arcade"] },
        "arcadia2001" => SystemMap { libretro: &["Emerson - Arcadia 2001"], esde: "arcadia", ss_names: &["arcadia 2001"], emu_tokens: &["arcadia"] },
        "atari2600" => SystemMap { libretro: &["Atari - 2600"], esde: "atari2600", ss_names: &["atari 2600", "vcs"], emu_tokens: &["atari 2600", "2600"] },
        "atari5200" => SystemMap { libretro: &["Atari - 5200"], esde: "atari5200", ss_names: &["atari 5200"], emu_tokens: &["atari 5200", "5200"] },
        "atari7800" => SystemMap { libretro: &["Atari - 7800"], esde: "atari7800", ss_names: &["atari 7800"], emu_tokens: &["atari 7800", "7800"] },
        "atari8bit" => SystemMap { libretro: &["Atari - 8-bit Family"], esde: "atari800", ss_names: &["atari 800", "atari 8-bit"], emu_tokens: &["atari 800", "atari 8"] },
        "c64" => SystemMap { libretro: &["Commodore - 64"], esde: "c64", ss_names: &["commodore 64", "c64"], emu_tokens: &["commodore 64", "c64"] },
        "cdi" => SystemMap { libretro: &["Philips - CD-i"], esde: "cdimono1", ss_names: &["cd-i", "cdi"], emu_tokens: &["cd-i", "cdi"] },
        "channelf" => SystemMap { libretro: &["Fairchild - Channel F"], esde: "channelf", ss_names: &["channel f"], emu_tokens: &["channel f"] },
        "colecovision" => SystemMap { libretro: &["Coleco - ColecoVision"], esde: "colecovision", ss_names: &["colecovision"], emu_tokens: &["colecovision", "coleco"] },
        "dreamcast" => SystemMap { libretro: &["Sega - Dreamcast"], esde: "dreamcast", ss_names: &["dreamcast"], emu_tokens: &["dreamcast"] },
        "gamecom" => SystemMap { libretro: &["Tiger - Game.com"], esde: "gamecom", ss_names: &["game.com", "gamecom"], emu_tokens: &["game.com", "gamecom"] },
        "gamecube" => SystemMap { libretro: &["Nintendo - GameCube"], esde: "gc", ss_names: &["gamecube"], emu_tokens: &["gamecube"] },
        "gamegear" => SystemMap { libretro: &["Sega - Game Gear"], esde: "gamegear", ss_names: &["game gear"], emu_tokens: &["game gear"] },
        "gb" => SystemMap { libretro: &["Nintendo - Game Boy"], esde: "gb", ss_names: &["game boy"], emu_tokens: &["game boy", "gameboy"] },
        "gba" => SystemMap { libretro: &["Nintendo - Game Boy Advance"], esde: "gba", ss_names: &["game boy advance"], emu_tokens: &["game boy advance", "gba"] },
        "gbc" => SystemMap { libretro: &["Nintendo - Game Boy Color"], esde: "gbc", ss_names: &["game boy color"], emu_tokens: &["game boy color", "gbc"] },
        "genesis" => SystemMap { libretro: &["Sega - Mega Drive - Genesis"], esde: "genesis", ss_names: &["megadrive", "genesis", "mega drive"], emu_tokens: &["genesis", "mega drive", "megadrive"] },
        "gx4000" => SystemMap { libretro: &["Amstrad - GX4000"], esde: "gx4000", ss_names: &["gx4000"], emu_tokens: &["gx4000"] },
        "intellivision" => SystemMap { libretro: &["Mattel - Intellivision"], esde: "intellivision", ss_names: &["intellivision"], emu_tokens: &["intellivision"] },
        "jaguar" => SystemMap { libretro: &["Atari - Jaguar"], esde: "atarijaguar", ss_names: &["jaguar"], emu_tokens: &["jaguar"] },
        "lynx" => SystemMap { libretro: &["Atari - Lynx"], esde: "atarilynx", ss_names: &["lynx"], emu_tokens: &["lynx"] },
        "mastersystem" => SystemMap { libretro: &["Sega - Master System - Mark III"], esde: "mastersystem", ss_names: &["master system"], emu_tokens: &["master system"] },
        "msx" => SystemMap { libretro: &["Microsoft - MSX", "Microsoft - MSX2"], esde: "msx", ss_names: &["msx"], emu_tokens: &["msx"] },
        "n3ds" => SystemMap { libretro: &["Nintendo - Nintendo 3DS"], esde: "n3ds", ss_names: &["nintendo 3ds", "3ds"], emu_tokens: &["3ds"] },
        "n64" => SystemMap { libretro: &["Nintendo - Nintendo 64"], esde: "n64", ss_names: &["nintendo 64"], emu_tokens: &["nintendo 64", "n64"] },
        "nds" => SystemMap { libretro: &["Nintendo - Nintendo DS"], esde: "nds", ss_names: &["nintendo ds"], emu_tokens: &["nintendo ds", "nds"] },
        "neogeo" => SystemMap { libretro: &["SNK - Neo Geo"], esde: "neogeo", ss_names: &["neo geo", "neogeo"], emu_tokens: &["neo geo", "neogeo"] },
        "neogeocd" => SystemMap { libretro: &["SNK - Neo Geo CD"], esde: "neogeocd", ss_names: &["neo geo cd"], emu_tokens: &["neo geo cd", "neogeocd"] },
        "neogeopocket" => SystemMap { libretro: &["SNK - Neo Geo Pocket Color", "SNK - Neo Geo Pocket"], esde: "ngpc", ss_names: &["neo geo pocket"], emu_tokens: &["neo geo pocket"] },
        "nes" => SystemMap { libretro: &["Nintendo - Nintendo Entertainment System"], esde: "nes", ss_names: &["nes", "nintendo entertainment system", "famicom"], emu_tokens: &["nes", "nintendo entertainment"] },
        "odyssey2" => SystemMap { libretro: &["Magnavox - Odyssey2", "Philips - Videopac"], esde: "odyssey2", ss_names: &["odyssey2", "videopac"], emu_tokens: &["odyssey", "videopac"] },
        "pcfx" => SystemMap { libretro: &["NEC - PC-FX"], esde: "pcfx", ss_names: &["pc-fx", "pcfx"], emu_tokens: &["pc-fx", "pcfx"] },
        "ps2" => SystemMap { libretro: &["Sony - PlayStation 2"], esde: "ps2", ss_names: &["playstation 2", "ps2"], emu_tokens: &["playstation 2", "ps2"] },
        "ps3" => SystemMap { libretro: &["Sony - PlayStation 3"], esde: "ps3", ss_names: &["playstation 3", "ps3"], emu_tokens: &["playstation 3", "ps3"] },
        "psp" => SystemMap { libretro: &["Sony - PlayStation Portable"], esde: "psp", ss_names: &["psp", "playstation portable"], emu_tokens: &["psp", "playstation portable"] },
        "psvita" => SystemMap { libretro: &["Sony - PlayStation Vita"], esde: "psvita", ss_names: &["ps vita", "vita"], emu_tokens: &["vita"] },
        "psx" => SystemMap { libretro: &["Sony - PlayStation"], esde: "psx", ss_names: &["playstation", "psx"], emu_tokens: &["playstation", "psx", "ps1"] },
        "pv1000" => SystemMap { libretro: &["Casio - PV-1000"], esde: "pv1000", ss_names: &["pv-1000"], emu_tokens: &["pv-1000"] },
        "saturn" => SystemMap { libretro: &["Sega - Saturn"], esde: "saturn", ss_names: &["saturn"], emu_tokens: &["saturn"] },
        "scv" => SystemMap { libretro: &["Epoch - Super Cassette Vision"], esde: "scv", ss_names: &["super cassette vision"], emu_tokens: &["cassette vision"] },
        "sega32x" => SystemMap { libretro: &["Sega - 32X"], esde: "sega32x", ss_names: &["32x", "mega drive 32x"], emu_tokens: &["32x"] },
        "segacd" => SystemMap { libretro: &["Sega - Mega-CD - Sega CD"], esde: "segacd", ss_names: &["mega-cd", "sega cd"], emu_tokens: &["sega cd", "mega-cd", "megacd"] },
        "sg1000" => SystemMap { libretro: &["Sega - SG-1000"], esde: "sg-1000", ss_names: &["sg-1000"], emu_tokens: &["sg-1000"] },
        "snes" => SystemMap { libretro: &["Nintendo - Super Nintendo Entertainment System"], esde: "snes", ss_names: &["snes", "super nintendo", "super famicom"], emu_tokens: &["snes", "super nintendo"] },
        "studio2" => SystemMap { libretro: &["RCA - Studio II"], esde: "studio2", ss_names: &["studio ii"], emu_tokens: &["studio ii"] },
        "superacan" => SystemMap { libretro: &["Funtech - Super Acan"], esde: "supracan", ss_names: &["super a'can"], emu_tokens: &["acan", "a'can"] },
        "tg16" => SystemMap { libretro: &["NEC - PC Engine - TurboGrafx 16", "NEC - PC Engine CD - TurboGrafx-CD"], esde: "tg16", ss_names: &["pc engine", "turbografx"], emu_tokens: &["turbografx", "pc engine"] },
        "vectrex" => SystemMap { libretro: &["GCE - Vectrex"], esde: "vectrex", ss_names: &["vectrex"], emu_tokens: &["vectrex"] },
        "vic20" => SystemMap { libretro: &["Commodore - VIC-20"], esde: "vic20", ss_names: &["vic-20", "vic20"], emu_tokens: &["vic-20", "vic20"] },
        "virtualboy" => SystemMap { libretro: &["Nintendo - Virtual Boy"], esde: "virtualboy", ss_names: &["virtual boy"], emu_tokens: &["virtual boy"] },
        "wii" => SystemMap { libretro: &["Nintendo - Wii"], esde: "wii", ss_names: &["wii"], emu_tokens: &["wii"] },
        "wiiu" => SystemMap { libretro: &["Nintendo - Wii U"], esde: "wiiu", ss_names: &["wii u"], emu_tokens: &["wii u"] },
        "wonderswan" => SystemMap { libretro: &["Bandai - WonderSwan Color", "Bandai - WonderSwan"], esde: "wonderswan", ss_names: &["wonderswan"], emu_tokens: &["wonderswan"] },
        "xbox" => SystemMap { libretro: &["Microsoft - Xbox"], esde: "xbox", ss_names: &["xbox"], emu_tokens: &["xbox"] },
        "xbox360" => SystemMap { libretro: &["Microsoft - Xbox 360"], esde: "xbox360", ss_names: &["xbox 360"], emu_tokens: &["xbox 360"] },
        "zxspectrum" => SystemMap { libretro: &["Sinclair - ZX Spectrum"], esde: "zxspectrum", ss_names: &["zx spectrum"], emu_tokens: &["zx spectrum", "spectrum"] },
        _ => return None,
    };
    Some(m)
}

/// All dataset system keys this feature knows (planner's universe
/// when "all systems" is requested is the catalog's, not this list —
/// systems missing here simply have no rungs and report as such).
pub fn known_systems() -> Vec<&'static str> {
    vec![
        "3do", "amiga", "amigacd32", "arcade", "arcadia2001", "atari2600",
        "atari5200", "atari7800", "atari8bit", "c64", "cdi", "channelf",
        "colecovision", "dreamcast", "gamecom", "gamecube", "gamegear",
        "gb", "gba", "gbc", "genesis", "gx4000", "intellivision", "jaguar",
        "lynx", "mastersystem", "msx", "n3ds", "n64", "nds", "neogeo",
        "neogeocd", "neogeopocket", "nes", "odyssey2", "pcfx", "ps2",
        "ps3", "psp", "psvita", "psx", "pv1000", "saturn", "scv",
        "sega32x", "segacd", "sg1000", "snes", "studio2", "superacan",
        "tg16", "vectrex", "vic20", "virtualboy", "wii", "wiiu",
        "wonderswan", "xbox", "xbox360", "zxspectrum",
    ]
}

// ---------------------------------------------------------------------------
// Settings — non-secret state only, one JSON file beside the catalog,
// written atomically (temp + rename, per the standing data-safety rule).
// ---------------------------------------------------------------------------

pub const MEDIA_TYPES: [&str; 4] = ["boxart", "screenshot", "titleshot", "trailer"];

pub fn media_type_label(media_type: &str) -> &'static str {
    match media_type {
        "boxart" => "box art",
        "screenshot" => "screenshot",
        "titleshot" => "title shot",
        "trailer" => "trailer",
        _ => "media",
    }
}

/// The ES-DE media folder name for a type (canonical tree level 3).
pub fn media_type_dir(media_type: &str) -> &'static str {
    match media_type {
        "boxart" => "covers",
        "screenshot" => "screenshots",
        "titleshot" => "titlescreens",
        "trailer" => "videos",
        _ => "media",
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TypeSettings {
    pub enabled: bool,
    #[serde(default)]
    pub dir_override: Option<String>,
}

impl Default for TypeSettings {
    fn default() -> Self {
        Self { enabled: true, dir_override: None }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RomMediaSettings {
    /// Canonical media root. Empty = default (~/Fable Media),
    /// expanded at load.
    pub media_root: String,
    /// RetroArch thumbnails directory for the mirror (None = off).
    pub retroarch_dir: Option<String>,
    pub boxart: TypeSettings,
    pub screenshots: TypeSettings,
    pub titles: TypeSettings,
    pub trailers: TypeSettings,
    /// small | standard | hd (EmuMovies quality appetite).
    pub trailer_tier: String,
    /// Display-only account names (secrets live in the keyring).
    pub screenscraper_user: Option<String>,
    pub emumovies_user: Option<String>,
}

impl Default for RomMediaSettings {
    fn default() -> Self {
        Self {
            media_root: String::new(),
            retroarch_dir: None,
            boxart: TypeSettings::default(),
            screenshots: TypeSettings::default(),
            titles: TypeSettings::default(),
            trailers: TypeSettings { enabled: false, dir_override: None },
            trailer_tier: "small".into(),
            screenscraper_user: None,
            emumovies_user: None,
        }
    }
}

impl RomMediaSettings {
    pub fn type_settings(&self, media_type: &str) -> &TypeSettings {
        match media_type {
            "screenshot" => &self.screenshots,
            "titleshot" => &self.titles,
            "trailer" => &self.trailers,
            _ => &self.boxart,
        }
    }
    pub fn root_path(&self) -> PathBuf {
        if self.media_root.trim().is_empty() {
            default_media_root()
        } else {
            PathBuf::from(&self.media_root)
        }
    }
}

pub fn default_media_root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join("Fable Media")
}

pub fn settings_path(base: &Path) -> PathBuf {
    base.join("rom-media.json")
}

pub fn load_settings(base: &Path) -> RomMediaSettings {
    let path = settings_path(base);
    let mut s: RomMediaSettings = std::fs::read(&path)
        .ok()
        .and_then(|d| serde_json::from_slice(&d).ok())
        .unwrap_or_default();
    if s.media_root.trim().is_empty() {
        s.media_root = default_media_root().to_string_lossy().into_owned();
    }
    s
}

pub fn save_settings(base: &Path, settings: &RomMediaSettings) -> Result<(), String> {
    let path = settings_path(base);
    let tmp = base.join("rom-media.json.part");
    let data = serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, data).map_err(|e| format!("write settings: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("save settings: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Secrets — the OS keyring is the only place credentials exist.
// Service = the app id; one entry per source. Values for the two
// account sources are JSON {user, pass}; API keys are raw strings.
// ---------------------------------------------------------------------------

pub const KEYRING_SERVICE: &str = "com.garrisonr.fablefiles";
pub const SECRET_KINDS: [&str; 5] = [
    "screenscraper",
    "screenscraper-dev",
    "emumovies",
    "thegamesdb",
    "steamgriddb",
];

fn keyring_entry(kind: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(KEYRING_SERVICE, &format!("rommedia/{kind}"))
        .map_err(|e| format!("system keyring unavailable: {e}"))
}

pub fn secret_get(kind: &str) -> Option<String> {
    let entry = keyring_entry(kind).ok()?;
    match entry.get_password() {
        Ok(v) if !v.is_empty() => Some(v),
        _ => None,
    }
}

/// Prove a just-set secret actually persisted: read it back through
/// a FRESH Entry and compare. Production constructs a new Entry for
/// every call, so a store that keeps values only inside one Entry
/// instance (the keyring crate's mock fallback) passes `set` and
/// fails here — that failure must surface as an honest error, never
/// a false "Saved" (2026-10-15 TMDB keyring incident).
pub fn verify_readback(user: &str, expected: &str) -> Result<(), String> {
    let fresh = keyring::Entry::new(KEYRING_SERVICE, user)
        .map_err(|e| format!("system keyring unavailable: {e}"))?;
    match fresh.get_password() {
        Ok(v) if v == expected => Ok(()),
        Ok(_) => Err(
            "the system keyring kept a different value than the one just saved"
                .to_string(),
        ),
        Err(e) => Err(format!(
            "the value could not be read back from the system keyring ({e}) — it was NOT stored"
        )),
    }
}

pub fn secret_set(kind: &str, value: Option<&str>) -> Result<(), String> {
    let entry = keyring_entry(kind)?;
    match value {
        Some(v) if !v.trim().is_empty() => {
            entry
                .set_password(v)
                .map_err(|e| format!("could not save to the system keyring: {e}"))?;
            verify_readback(&format!("rommedia/{kind}"), v)
        }
        _ => match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(format!("could not clear the saved secret: {e}")),
        },
    }
}

pub fn keyring_available() -> bool {
    // A get on a never-written entry returns NoEntry when the store
    // itself is reachable, and a different error when it is not.
    match keyring_entry("probe") {
        Ok(entry) => match entry.get_password() {
            Ok(_) => true,
            Err(keyring::Error::NoEntry) => true,
            Err(_) => false,
        },
        Err(_) => false,
    }
}

#[derive(Debug, Clone, Default)]
pub struct AccountCreds {
    pub user: String,
    pub pass: String,
}

pub fn account_creds(kind: &str) -> Option<AccountCreds> {
    let raw = secret_get(kind)?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    Some(AccountCreds {
        user: v.get("user")?.as_str()?.to_string(),
        pass: v.get("pass")?.as_str()?.to_string(),
    })
}

// ---------------------------------------------------------------------------
// HTTP — one small blocking client wrapper. NEVER logs URLs (SS media
// URLs embed credentials). Errors distinguish "not there" (a normal
// cascade miss) from "could not ask" (a source error worth caching
// as an error row, not a miss).
// ---------------------------------------------------------------------------

pub const USER_AGENT: &str = "FableFiles (ROM media enrichment; +https://github.com/garrisonr/fable-files)";

#[derive(Debug)]
pub enum FetchErr {
    NotFound,
    Http(u16),
    Net(String),
}

impl std::fmt::Display for FetchErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchErr::NotFound => write!(f, "not found"),
            FetchErr::Http(code) => write!(f, "the source answered with an error (HTTP {code})"),
            FetchErr::Net(e) => write!(f, "could not reach the source ({e})"),
        }
    }
}

pub fn http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(15))
        .timeout_read(std::time::Duration::from_secs(120))
        .user_agent(USER_AGENT)
        .build()
}

fn map_ureq_err(e: ureq::Error) -> FetchErr {
    match e {
        ureq::Error::Status(404, _) => FetchErr::NotFound,
        ureq::Error::Status(code, _) => FetchErr::Http(code),
        ureq::Error::Transport(t) => FetchErr::Net(t.to_string()),
    }
}

/// GET a small body as text (API calls).
pub fn http_get_string(agent: &ureq::Agent, url: &str) -> Result<String, FetchErr> {
    let resp = agent.get(url).call().map_err(map_ureq_err)?;
    resp.into_string().map_err(|e| FetchErr::Net(e.to_string()))
}

/// GET JSON.
pub fn http_get_json(agent: &ureq::Agent, url: &str) -> Result<serde_json::Value, FetchErr> {
    let text = http_get_string(agent, url)?;
    serde_json::from_str(&text).map_err(|e| FetchErr::Net(format!("unreadable answer: {e}")))
}

/// Stream a download to `part_path` (created/truncated). `stop` is
/// polled between chunks: returning true aborts, deletes the partial
/// file, and reports Cancelled. Returns the byte count.
pub enum DownloadOutcome {
    Done(u64),
    Cancelled,
}

pub fn http_download(
    agent: &ureq::Agent,
    url: &str,
    part_path: &Path,
    stop: &dyn Fn() -> bool,
) -> Result<DownloadOutcome, FetchErr> {
    use std::io::Write;
    let resp = agent.get(url).call().map_err(map_ureq_err)?;
    let mut reader = resp.into_reader();
    let mut file = std::fs::File::create(part_path)
        .map_err(|e| FetchErr::Net(format!("cannot write the download file: {e}")))?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        if stop() {
            drop(file);
            let _ = std::fs::remove_file(part_path);
            return Ok(DownloadOutcome::Cancelled);
        }
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                file.write_all(&buf[..n])
                    .map_err(|e| FetchErr::Net(format!("write failed mid-download: {e}")))?;
                total += n as u64;
            }
            Err(e) => {
                drop(file);
                let _ = std::fs::remove_file(part_path);
                return Err(FetchErr::Net(format!("download interrupted: {e}")));
            }
        }
    }
    file.flush().ok();
    if total == 0 {
        let _ = std::fs::remove_file(part_path);
        return Err(FetchErr::NotFound);
    }
    Ok(DownloadOutcome::Done(total))
}

// ---------------------------------------------------------------------------
// Names — the transforms every source keying depends on.
// ---------------------------------------------------------------------------

/// Libretro's substitution: every character in &*/:`<>?\|" becomes _.
/// Applied to the DAT name for BOTH the server path and the RetroArch
/// mirror file name (same rule, two consumers).
pub fn libretro_name(dat_name: &str) -> String {
    dat_name
        .chars()
        .map(|c| match c {
            '&' | '*' | '/' | ':' | '`' | '<' | '>' | '?' | '\\' | '|' | '"' => '_',
            _ => c,
        })
        .collect()
}

/// The clean title for name-search sources: DAT name minus its
/// parenthetical/bracketed tags ("Contra (USA) (Rev 1)" -> "Contra").
pub fn clean_title(dat_name: &str) -> String {
    let mut out = String::new();
    let mut depth = 0i32;
    for c in dat_name.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = (depth - 1).max(0),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Normalized comparison key for exact-match acceptance: lowercase,
/// alphanumerics and single spaces only. Two titles are "the same
/// game" only when these keys are EQUAL — anything weaker is a fuzzy
/// match and goes to the human (see module rules).
pub fn title_key(title: &str) -> String {
    let lowered = title.to_lowercase();
    let mut out = String::new();
    let mut last_space = true;
    for c in lowered.chars() {
        if c.is_alphanumeric() {
            out.push(c);
            last_space = false;
        } else if !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    out.trim().to_string()
}

/// Region preference from the DAT name's tags, ScreenScraper style
/// (us | eu | jp | wor). Used to pick among a game's regional media.
pub fn region_hint(dat_name: &str) -> &'static str {
    let n = dat_name;
    if n.contains("(Japan)") || n.contains("(JP)") {
        "jp"
    } else if n.contains("(Europe)") || n.contains("(EU)") {
        "eu"
    } else if n.contains("(World)") {
        "wor"
    } else {
        "us"
    }
}

/// The ROM filename stem: the storage name in the media tree. The
/// tree is stem-keyed so ES-DE finds media by pure convention; the
/// stem is the ROM's own file stem (ES-DE's rule), which for a
/// DAT-verified library is the DAT name minus any archive dressing.
pub fn file_stem(file_name: &str) -> String {
    match file_name.rfind('.') {
        Some(i) if i > 0 => file_name[..i].to_string(),
        _ => file_name.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Layout — canonical ES-DE tree + per-type overrides.
// ---------------------------------------------------------------------------

/// Where a type's files REALLY live: an override folder replaces the
/// type level of the canonical tree (the override dir IS the type
/// folder). Without an override, real == canonical.
pub fn real_path(
    settings: &RomMediaSettings,
    sys: &str,
    media_type: &str,
    stem: &str,
    ext: &str,
) -> PathBuf {
    let map = system_map(sys);
    let esde = map.as_ref().map(|m| m.esde).unwrap_or("unknown");
    match &settings.type_settings(media_type).dir_override {
        Some(dir) if !dir.trim().is_empty() => {
            PathBuf::from(dir).join(esde).join(format!("{stem}.{ext}"))
        }
        _ => canonical_path(settings, sys, media_type, stem, ext),
    }
}

pub fn canonical_path(
    settings: &RomMediaSettings,
    sys: &str,
    media_type: &str,
    stem: &str,
    ext: &str,
) -> PathBuf {
    let map = system_map(sys);
    let esde = map.as_ref().map(|m| m.esde).unwrap_or("unknown");
    settings
        .root_path()
        .join(esde)
        .join(media_type_dir(media_type))
        .join(format!("{stem}.{ext}"))
}

/// The extension a stored file should carry, from what a source
/// actually sent (content type / URL suffix), defaulting by kind.
pub fn ext_from(url_or_name: &str, fallback: &str) -> String {
    let clean = url_or_name.split(['?', '#']).next().unwrap_or("");
    if let Some(i) = clean.rfind('.') {
        let ext = &clean[i + 1..];
        if !ext.is_empty() && ext.len() <= 5 && ext.chars().all(|c| c.is_alphanumeric()) {
            return ext.to_lowercase();
        }
    }
    fallback.to_string()
}

/// Try a hardlink, then a symlink, then a copy. Returns which
/// happened ("link" | "copy") so the caller can be honest in the
/// report: on exFAT (his Desk SSD) links do not exist and the mirror
/// costs real bytes.
pub fn link_or_copy(from: &Path, to: &Path) -> std::io::Result<&'static str> {
    if to.exists() {
        return Ok("link");
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::hard_link(from, to).is_ok() {
        return Ok("link");
    }
    #[cfg(unix)]
    {
        if std::os::unix::fs::symlink(from, to).is_ok() {
            return Ok("link");
        }
    }
    std::fs::copy(from, to)?;
    Ok("copy")
}

// ---------------------------------------------------------------------------
// The cascade. Order per MEMO §2.3: free-and-keyless first, hash-exact
// second, allowance-metered late, his paid source where it is uniquely
// deep. Availability is decided per run from settings + keyring.
// ---------------------------------------------------------------------------

pub fn cascade_for(media_type: &str) -> Vec<&'static str> {
    match media_type {
        "trailer" => vec!["emumovies", "screenscraper"],
        "boxart" => vec!["libretro", "screenscraper", "thegamesdb", "steamgriddb"],
        _ => vec!["libretro", "screenscraper", "thegamesdb"],
    }
}

pub fn source_label(source: &str) -> &'static str {
    match source {
        "libretro" => "Libretro",
        "screenscraper" => "ScreenScraper",
        "thegamesdb" => "TheGamesDB",
        "steamgriddb" => "SteamGridDB",
        "emumovies" => "EmuMovies",
        "manual" => "your own files",
        _ => "a source",
    }
}

/// Process-level source health: a source that failed at the
/// connection level is not retried for every game in a long job —
/// the first failure's note is remembered and later steps skip the
/// rung, landing in the report as errors the user can retry.
static SOURCE_DOWN: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

fn source_down(source: &str) -> Option<String> {
    SOURCE_DOWN
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(source).cloned())
}

fn mark_source_down(source: &str, note: &str) {
    let mut guard = SOURCE_DOWN.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    map.entry(source.to_string()).or_insert_with(|| note.to_string());
}

/// One downloadable candidate offered to the human in the report.
/// Only ever produced by name-search sources; confirming one is an
/// explicit user act, never an automatic acceptance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub source: String,
    pub title: String,
    pub url: String,
}

#[derive(Debug)]
pub enum FetchResult {
    /// Bytes are sitting at the part path; `ext` is the real file
    /// extension the final name should carry.
    Downloaded { source: &'static str, ext: String },
    Missing,
    Ambiguous { candidates: Vec<Candidate> },
    Error { note: String },
    Cancelled,
}

/// Everything one FetchMedia step carries, restated as a struct.
#[derive(Debug, Clone)]
pub struct FetchSpec {
    pub item_id: String,
    pub system: String,
    pub dat_name: String,
    pub rom_stem: String,
    pub media_type: String,
    pub crc: Option<String>,
    pub md5: Option<String>,
    pub sha1: Option<String>,
    pub size: Option<u64>,
}

// ---------------------------------------------------------------------------
// URL helpers (query strings carry names with spaces, parens, commas).
// ---------------------------------------------------------------------------

fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn query(params: &[(&str, String)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{k}={}", pct_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

// ---------------------------------------------------------------------------
// Libretro — the free backbone. Name-keyed, no account, no key.
// ---------------------------------------------------------------------------

fn libretro_folder(media_type: &str) -> Option<&'static str> {
    match media_type {
        "boxart" => Some("Named_Boxarts"),
        "screenshot" => Some("Named_Snaps"),
        "titleshot" => Some("Named_Titles"),
        _ => None,
    }
}

fn libretro_fetch(
    agent: &ureq::Agent,
    spec: &FetchSpec,
    part_path: &Path,
    stop: &dyn Fn() -> bool,
) -> FetchResult {
    let Some(folder) = libretro_folder(&spec.media_type) else {
        return FetchResult::Missing;
    };
    let Some(map) = system_map(&spec.system) else {
        return FetchResult::Missing;
    };
    let file = format!("{}.png", libretro_name(&spec.dat_name));
    let mut last_err: Option<FetchErr> = None;
    for dir in map.libretro {
        let url = format!(
            "https://thumbnails.libretro.com/{}/{}/{}",
            pct_encode(dir),
            folder,
            pct_encode(&file)
        );
        match http_download(agent, &url, part_path, stop) {
            Ok(DownloadOutcome::Done(_)) => {
                return FetchResult::Downloaded { source: "libretro", ext: "png".into() };
            }
            Ok(DownloadOutcome::Cancelled) => return FetchResult::Cancelled,
            Err(FetchErr::NotFound) => continue,
            Err(e) => last_err = Some(e),
        }
    }
    match last_err {
        Some(e) => FetchResult::Error { note: format!("Libretro: {e}") },
        None => FetchResult::Missing,
    }
}

// ---------------------------------------------------------------------------
// ScreenScraper — hash-exact, the deep well. The API identifies the
// SOFTWARE with developer credentials; Fable's own registration is
// pending (see SS_DEV_ID), and a user's own free developer
// credentials can be supplied in Settings (Advanced) meanwhile.
// ---------------------------------------------------------------------------

/// Fable's ScreenScraper developer credentials land here when the
/// registration completes (the ecosystem norm: every app ships its
/// own; they identify the software, never the user). Empty = the
/// rung activates only via user-supplied dev credentials.
const SS_DEV_ID: &str = "";
const SS_DEV_PASSWORD: &str = "";

fn ss_dev_creds() -> Option<(String, String)> {
    if !SS_DEV_ID.is_empty() {
        return Some((SS_DEV_ID.to_string(), SS_DEV_PASSWORD.to_string()));
    }
    let c = account_creds("screenscraper-dev")?;
    if c.user.is_empty() {
        None
    } else {
        Some((c.user, c.pass))
    }
}

const SS_API: &str = "https://api.screenscraper.fr/api2";

fn ss_base_params(dev: &(String, String), user: &Option<AccountCreds>) -> Vec<(String, String)> {
    let mut p = vec![
        ("devid".to_string(), dev.0.clone()),
        ("devpassword".to_string(), dev.1.clone()),
        ("softname".to_string(), "FableFiles".to_string()),
        ("output".to_string(), "json".to_string()),
    ];
    if let Some(u) = user {
        p.push(("ssid".to_string(), u.user.clone()));
        p.push(("sspassword".to_string(), u.pass.clone()));
    }
    p
}

/// Resolved system ids, cached per process from the service's own
/// systemesListe (name-matched, never guessed statically).
static SS_SYSTEM_IDS: Mutex<Option<HashMap<String, u32>>> = Mutex::new(None);

fn ss_system_id(agent: &ureq::Agent, dev: &(String, String), sys: &str) -> Option<u32> {
    if let Some(map) = SS_SYSTEM_IDS.lock().unwrap().as_ref() {
        return map.get(sys).copied();
    }
    let map = system_map(sys)?;
    let url = format!(
        "{SS_API}/systemesListe.php?{}",
        query(&ss_base_params(dev, &None).iter().map(|(k, v)| (k.as_str(), v.clone())).collect::<Vec<_>>())
    );
    let json = http_get_json(agent, &url).ok()?;
    let systems = json.pointer("/response/systemes")?.as_array()?.clone();
    let mut resolved: HashMap<String, u32> = HashMap::new();
    // Resolve every known Fable system in one pass.
    for key in known_systems() {
        let Some(m) = system_map(key) else { continue };
        'outer: for entry in &systems {
            let Some(id) = entry.get("id").and_then(|v| v.as_u64()) else { continue };
            let mut names: Vec<String> = Vec::new();
            if let Some(noms) = entry.get("noms") {
                collect_strings(noms, &mut names);
            }
            if let Some(nom) = entry.get("nom").and_then(|v| v.as_str()) {
                names.push(nom.to_string());
            }
            for n in &names {
                let nl = n.to_lowercase();
                if m.ss_names.iter().any(|a| nl == *a || nl.contains(a)) {
                    resolved.insert(key.to_string(), id as u32);
                    break 'outer;
                }
            }
        }
    }
    let out = resolved.get(sys).copied();
    *SS_SYSTEM_IDS.lock().unwrap() = Some(resolved);
    let _ = map;
    out
}

fn collect_strings(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(a) => a.iter().for_each(|x| collect_strings(x, out)),
        serde_json::Value::Object(o) => o.values().for_each(|x| collect_strings(x, out)),
        _ => {}
    }
}

/// The game's media list from one jeuInfos call, cached per process
/// by item id: one call serves every media type (their daily
/// allowance makes re-asking the cardinal sin).
static SS_GAME_CACHE: Mutex<Option<HashMap<String, SsGame>>> = Mutex::new(None);

#[derive(Clone)]
enum SsGame {
    Found(Vec<serde_json::Value>),
    NotFound,
}

fn ss_game(
    agent: &ureq::Agent,
    spec: &FetchSpec,
    dev: &(String, String),
    user: &Option<AccountCreds>,
) -> Result<SsGame, FetchErr> {
    {
        let guard = SS_GAME_CACHE.lock().unwrap();
        if let Some(cache) = guard.as_ref() {
            if let Some(g) = cache.get(&spec.item_id) {
                return Ok(g.clone());
            }
        }
    }
    let mut params = ss_base_params(dev, user);
    if let Some(id) = ss_system_id(agent, dev, &spec.system) {
        params.push(("systemeid".to_string(), id.to_string()));
    }
    // romnom is the chaperone; the hashes are the identity.
    params.push(("romnom".to_string(), format!("{}.zip", spec.rom_stem)));
    if let Some(crc) = &spec.crc {
        params.push(("romcrc".to_string(), crc.clone()));
    }
    if let Some(md5) = &spec.md5 {
        params.push(("rommd5".to_string(), md5.clone()));
    }
    if let Some(sha1) = &spec.sha1 {
        params.push(("romsha1".to_string(), sha1.clone()));
    }
    if let Some(size) = spec.size {
        params.push(("romtaille".to_string(), size.to_string()));
    }
    let url = format!(
        "{SS_API}/jeuInfos.php?{}",
        query(&params.iter().map(|(k, v)| (k.as_str(), v.clone())).collect::<Vec<_>>())
    );
    let json = http_get_json(agent, &url)?;
    let game = match json.pointer("/response/jeu") {
        Some(jeu) if !jeu.is_null() => {
            let medias = jeu
                .get("medias")
                .and_then(|m| m.as_array())
                .cloned()
                .unwrap_or_default();
            SsGame::Found(medias)
        }
        _ => SsGame::NotFound,
    };
    let mut guard = SS_GAME_CACHE.lock().unwrap();
    guard
        .get_or_insert_with(HashMap::new)
        .insert(spec.item_id.clone(), game.clone());
    Ok(game)
}

fn ss_media_type(media_type: &str) -> Option<&'static str> {
    match media_type {
        "boxart" => Some("box-2D"),
        "screenshot" => Some("ss"),
        "titleshot" => Some("sstitle"),
        "trailer" => Some("video"),
        _ => None,
    }
}

/// Pick one media entry: right type, region preferred (the DAT
/// name's region, then worldwide, then whatever exists).
pub fn ss_pick_media(
    medias: &[serde_json::Value],
    media_type: &str,
    region: &str,
) -> Option<String> {
    let want = ss_media_type(media_type)?;
    let typed: Vec<&serde_json::Value> = medias
        .iter()
        .filter(|m| m.get("type").and_then(|t| t.as_str()) == Some(want))
        .collect();
    let url_of = |m: &serde_json::Value| {
        m.get("url").and_then(|u| u.as_str()).map(|s| s.to_string())
    };
    for pref in [region, "wor", "us", "eu", "jp"] {
        for m in &typed {
            if m.get("region").and_then(|r| r.as_str()) == Some(pref) {
                if let Some(u) = url_of(m) {
                    return Some(u);
                }
            }
        }
    }
    typed.first().and_then(|m| url_of(m))
}

fn ss_fetch(
    agent: &ureq::Agent,
    spec: &FetchSpec,
    part_path: &Path,
    stop: &dyn Fn() -> bool,
) -> FetchResult {
    let Some(dev) = ss_dev_creds() else {
        return FetchResult::Missing;
    };
    let user = account_creds("screenscraper");
    match ss_game(agent, spec, &dev, &user) {
        Ok(SsGame::NotFound) => FetchResult::Missing,
        Ok(SsGame::Found(medias)) => {
            let Some(url) = ss_pick_media(&medias, &spec.media_type, region_hint(&spec.dat_name)) else {
                return FetchResult::Missing;
            };
            // The URL embeds credentials: never logged, never stored.
            match http_download(agent, &url, part_path, stop) {
                Ok(DownloadOutcome::Done(_)) => FetchResult::Downloaded {
                    source: "screenscraper",
                    ext: ext_from(&url, if spec.media_type == "trailer" { "mp4" } else { "png" }),
                },
                Ok(DownloadOutcome::Cancelled) => FetchResult::Cancelled,
                Err(FetchErr::NotFound) => FetchResult::Missing,
                Err(e) => FetchResult::Error { note: format!("ScreenScraper: {e}") },
            }
        }
        Err(e) => FetchResult::Error { note: format!("ScreenScraper: {e}") },
    }
}

// ---------------------------------------------------------------------------
// TheGamesDB — name-keyed second opinion with a monthly allowance.
// Exact normalized-title matches only; everything else is a human
// decision in the report.
// ---------------------------------------------------------------------------

fn tgdb_search(
    agent: &ureq::Agent,
    key: &str,
    title: &str,
) -> Result<Vec<serde_json::Value>, FetchErr> {
    let url = format!(
        "https://api.thegamesdb.net/v1/Games/ByGameName?{}",
        query(&[("apikey", key.to_string()), ("name", title.to_string())])
    );
    let json = http_get_json(agent, &url)?;
    Ok(json
        .pointer("/data/games")
        .and_then(|g| g.as_array())
        .cloned()
        .unwrap_or_default())
}

fn tgdb_images(
    agent: &ureq::Agent,
    key: &str,
    game_id: u64,
    media_type: &str,
) -> Result<Option<String>, FetchErr> {
    let url = format!(
        "https://api.thegamesdb.net/v1/Games/Images?{}",
        query(&[("apikey", key.to_string()), ("games_id", game_id.to_string())])
    );
    let json = http_get_json(agent, &url)?;
    let want = match media_type {
        "boxart" => "boxart",
        "screenshot" => "screenshot",
        "titleshot" => "titlescreen",
        _ => return Ok(None),
    };
    let base = json
        .pointer("/data/base_url/original")
        .and_then(|b| b.as_str())
        .unwrap_or("")
        .to_string();
    let images = json.pointer("/data/images").and_then(|i| i.as_object()).cloned();
    let Some(images) = images else { return Ok(None) };
    let list = images
        .get(&game_id.to_string())
        .and_then(|l| l.as_array())
        .cloned()
        .unwrap_or_default();
    for img in &list {
        if img.get("type").and_then(|t| t.as_str()) != Some(want) {
            continue;
        }
        if want == "boxart" && img.get("side").and_then(|s| s.as_str()) == Some("back") {
            continue;
        }
        if let Some(filename) = img.get("filename").and_then(|f| f.as_str()) {
            if filename.starts_with("http") {
                return Ok(Some(filename.to_string()));
            }
            return Ok(Some(format!("{base}{filename}")));
        }
    }
    Ok(None)
}

/// Classify a name-search result set: exact matches are the only
/// auto-accept; near matches become candidates. Pure function —
/// the fixture tests pin this behavior (the fuzzy law).
pub fn classify_title_matches(
    clean: &str,
    games: &[(String, String)], // (id, title)
) -> TitleMatch {
    let want = title_key(clean);
    let exact: Vec<&(String, String)> =
        games.iter().filter(|(_, t)| title_key(t) == want).collect();
    match exact.len() {
        1 => TitleMatch::Exact(exact[0].0.clone()),
        n if n > 1 => TitleMatch::Ambiguous(
            exact.iter().map(|(id, t)| (id.clone(), t.clone())).collect(),
        ),
        _ => {
            let near: Vec<(String, String)> = games
                .iter()
                .filter(|(_, t)| {
                    let k = title_key(t);
                    k.starts_with(&want) || want.starts_with(&k)
                })
                .take(3)
                .cloned()
                .collect();
            if near.is_empty() {
                TitleMatch::None
            } else {
                TitleMatch::Ambiguous(near)
            }
        }
    }
}

pub enum TitleMatch {
    Exact(String),
    Ambiguous(Vec<(String, String)>),
    None,
}

fn tgdb_fetch(
    agent: &ureq::Agent,
    spec: &FetchSpec,
    part_path: &Path,
    stop: &dyn Fn() -> bool,
) -> FetchResult {
    let Some(key) = secret_get("thegamesdb") else {
        return FetchResult::Missing;
    };
    let clean = clean_title(&spec.dat_name);
    let games = match tgdb_search(agent, &key, &clean) {
        Ok(g) => g,
        Err(e) => return FetchResult::Error { note: format!("TheGamesDB: {e}") },
    };
    let pairs: Vec<(String, String)> = games
        .iter()
        .filter_map(|g| {
            Some((
                g.get("id")?.as_u64()?.to_string(),
                g.get("game_title")?.as_str()?.to_string(),
            ))
        })
        .collect();
    match classify_title_matches(&clean, &pairs) {
        TitleMatch::None => FetchResult::Missing,
        TitleMatch::Ambiguous(list) => {
            // Offer the art itself as the disambiguator (capped:
            // the monthly allowance is precious).
            let mut candidates = Vec::new();
            for (id, title) in list.into_iter().take(3) {
                if let Ok(Some(url)) =
                    tgdb_images(agent, &key, id.parse().unwrap_or(0), &spec.media_type)
                {
                    candidates.push(Candidate {
                        source: "thegamesdb".into(),
                        title,
                        url,
                    });
                }
            }
            if candidates.is_empty() {
                FetchResult::Missing
            } else {
                FetchResult::Ambiguous { candidates }
            }
        }
        TitleMatch::Exact(id) => {
            match tgdb_images(agent, &key, id.parse().unwrap_or(0), &spec.media_type) {
                Ok(Some(url)) => match http_download(agent, &url, part_path, stop) {
                    Ok(DownloadOutcome::Done(_)) => FetchResult::Downloaded {
                        source: "thegamesdb",
                        ext: ext_from(&url, "jpg"),
                    },
                    Ok(DownloadOutcome::Cancelled) => FetchResult::Cancelled,
                    Err(FetchErr::NotFound) => FetchResult::Missing,
                    Err(e) => FetchResult::Error { note: format!("TheGamesDB: {e}") },
                },
                Ok(None) => FetchResult::Missing,
                Err(e) => FetchResult::Error { note: format!("TheGamesDB: {e}") },
            }
        }
    }
}

// ---------------------------------------------------------------------------
// SteamGridDB — the pretty-posters polish layer (box art only).
// ---------------------------------------------------------------------------

fn sgdb_fetch(
    agent: &ureq::Agent,
    spec: &FetchSpec,
    part_path: &Path,
    stop: &dyn Fn() -> bool,
) -> FetchResult {
    if spec.media_type != "boxart" {
        return FetchResult::Missing;
    }
    let Some(key) = secret_get("steamgriddb") else {
        return FetchResult::Missing;
    };
    let clean = clean_title(&spec.dat_name);
    let call = |url: &str| {
        agent
            .get(url)
            .set("Authorization", &format!("Bearer {key}"))
            .call()
            .map_err(map_ureq_err)
    };
    let search_url = format!(
        "https://www.steamgriddb.com/api/v2/search/autocomplete/{}",
        pct_encode(&clean)
    );
    let resp = match call(&search_url) {
        Ok(r) => r,
        Err(e) => return FetchResult::Error { note: format!("SteamGridDB: {e}") },
    };
    let json: serde_json::Value = match resp.into_string() {
        Ok(t) => match serde_json::from_str(&t) {
            Ok(j) => j,
            Err(e) => {
                return FetchResult::Error {
                    note: format!("SteamGridDB: unreadable answer ({e})"),
                }
            }
        },
        Err(e) => return FetchResult::Error { note: format!("SteamGridDB: {e}") },
    };
    let games: Vec<(String, String)> = json
        .get("data")
        .and_then(|d| d.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|g| {
                    Some((
                        g.get("id")?.as_u64()?.to_string(),
                        g.get("name")?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let grid_url = |id: &str| -> Option<String> {
        let url = format!(
            "https://www.steamgriddb.com/api/v2/grids/game/{id}?dimensions=600x900&limit=5"
        );
        let resp = call(&url).ok()?;
        let text = resp.into_string().ok()?;
        let j: serde_json::Value = serde_json::from_str(&text).ok()?;
        let grids = j.get("data")?.as_array()?;
        for g in grids {
            if g.get("animated").and_then(|a| a.as_bool()) == Some(true) {
                continue;
            }
            if let Some(u) = g.get("url").and_then(|u| u.as_str()) {
                return Some(u.to_string());
            }
        }
        grids
            .first()
            .and_then(|g| g.get("url"))
            .and_then(|u| u.as_str())
            .map(|s| s.to_string())
    };
    match classify_title_matches(&clean, &games) {
        TitleMatch::None => FetchResult::Missing,
        TitleMatch::Ambiguous(list) => {
            let mut candidates = Vec::new();
            for (id, title) in list.into_iter().take(3) {
                if let Some(url) = grid_url(&id) {
                    candidates.push(Candidate {
                        source: "steamgriddb".into(),
                        title,
                        url,
                    });
                }
            }
            if candidates.is_empty() {
                FetchResult::Missing
            } else {
                FetchResult::Ambiguous { candidates }
            }
        }
        TitleMatch::Exact(id) => match grid_url(&id) {
            Some(url) => match http_download(agent, &url, part_path, stop) {
                Ok(DownloadOutcome::Done(_)) => FetchResult::Downloaded {
                    source: "steamgriddb",
                    ext: ext_from(&url, "png"),
                },
                Ok(DownloadOutcome::Cancelled) => FetchResult::Cancelled,
                Err(FetchErr::NotFound) => FetchResult::Missing,
                Err(e) => FetchResult::Error { note: format!("SteamGridDB: {e}") },
            },
            None => FetchResult::Missing,
        },
    }
}

// ---------------------------------------------------------------------------
// EmuMovies — the trailer vault, reached by FTP (their file server
// is the integration channel; their layout is not publicly
// documented, so the client walks directories defensively: bounded
// depth, bounded calls, honest misses). A minimal passive-mode FTP
// client over TcpStream — the protocol needs nothing more.
//
// HOST (verified against the live service 2026-10-16): the file
// server is files.emumovies.com — root holds Official/ and
// Submissions/; the library lives under Official/Video Snaps
// (<tier>)/<system>/. ftp.emumovies.com is a DIFFERENT server
// (Pure-FTPd) that refuses the same account credentials, plain and
// over FTPS alike — pointing there made every stored login look
// like a bad password. Do not "fix" this back.
// ---------------------------------------------------------------------------

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

const EMU_HOST: &str = "files.emumovies.com";
const VIDEO_EXTS: [&str; 7] = ["mp4", "avi", "mkv", "webm", "flv", "wmv", "mov"];

struct Ftp {
    ctrl: TcpStream,
    reader: BufReader<TcpStream>,
}

impl Ftp {
    fn connect(host: &str) -> Result<Ftp, String> {
        let stream = TcpStream::connect((host, 21))
            .map_err(|e| format!("could not reach the EmuMovies file server ({e})"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(45)))
            .ok();
        let reader = BufReader::new(
            stream
                .try_clone()
                .map_err(|e| format!("file server connection failed ({e})"))?,
        );
        let mut ftp = Ftp { ctrl: stream, reader };
        let (code, _) = ftp.read_reply()?;
        if code != 220 {
            return Err("the EmuMovies file server did not greet us as expected".into());
        }
        Ok(ftp)
    }

    fn read_reply(&mut self) -> Result<(u16, String), String> {
        let mut text = String::new();
        loop {
            let mut line = String::new();
            let n = self
                .reader
                .read_line(&mut line)
                .map_err(|e| format!("file server stopped answering ({e})"))?;
            if n == 0 {
                return Err("the file server closed the connection".into());
            }
            text.push_str(&line);
            let code: u16 = line.get(0..3).and_then(|c| c.parse().ok()).unwrap_or(0);
            // "NNN " ends a reply; "NNN-" continues it.
            if line.chars().nth(3) == Some(' ') || line.len() < 4 {
                return Ok((code, text));
            }
        }
    }

    fn cmd(&mut self, cmd: &str) -> Result<(u16, String), String> {
        // Commands may carry the password; never log them.
        self.ctrl
            .write_all(format!("{cmd}\r\n").as_bytes())
            .map_err(|e| format!("file server write failed ({e})"))?;
        self.read_reply()
    }

    fn login(&mut self, user: &str, pass: &str) -> Result<(), String> {
        let (code, text) = self.cmd(&format!("USER {user}"))?;
        if code == 230 {
            return Ok(());
        }
        if code != 331 {
            return Err(format!(
                "the file server did not accept that user name ({})",
                reply_tail(&text)
            ));
        }
        let (code, text) = self.cmd(&format!("PASS {pass}"))?;
        if code == 230 {
            Ok(())
        } else {
            Err(format!(
                "sign-in refused by the file server ({})",
                reply_tail(&text)
            ))
        }
    }

    fn pasv(&mut self) -> Result<(String, u16), String> {
        let (code, text) = self.cmd("PASV")?;
        if code != 227 {
            return Err("the file server refused a data connection".into());
        }
        parse_pasv(&text).ok_or_else(|| "could not understand the file server's data address".into())
    }

    fn data_stream(&mut self) -> Result<TcpStream, String> {
        let (host, port) = self.pasv()?;
        let s = TcpStream::connect((host.as_str(), port))
            .map_err(|e| format!("data connection failed ({e})"))?;
        s.set_read_timeout(Some(Duration::from_secs(90))).ok();
        Ok(s)
    }

    /// Unix LIST output -> (name, is_dir). Names may contain spaces;
    /// the name is everything after the 8th field.
    fn list(&mut self, path: Option<&str>) -> Result<Vec<(String, bool)>, String> {
        let mut data = self.data_stream()?;
        let arg = path.map(|p| format!(" {p}")).unwrap_or_default();
        let (code, _) = self.cmd(&format!("LIST{arg}"))?;
        if !(code == 150 || code == 125) {
            return Err("the file server would not list that folder".into());
        }
        // Listings carry file names in whatever encoding the source
        // used (the live library has Latin-1 names) — decode lossy
        // rather than failing the whole listing on one bad byte.
        let mut bytes = Vec::new();
        data.read_to_end(&mut bytes)
            .map_err(|e| format!("listing download failed ({e})"))?;
        let _ = self.read_reply();
        Ok(parse_list(&String::from_utf8_lossy(&bytes)))
    }

    fn retr_to_file(
        &mut self,
        path: &str,
        part_path: &Path,
        stop: &dyn Fn() -> bool,
    ) -> Result<DownloadOutcome, FetchErr> {
        self.cmd("TYPE I").map_err(FetchErr::Net)?;
        let mut data = self.data_stream().map_err(FetchErr::Net)?;
        let (code, _) = self
            .cmd(&format!("RETR {path}"))
            .map_err(FetchErr::Net)?;
        if code != 150 && code != 125 {
            return Err(FetchErr::NotFound);
        }
        let mut file = std::fs::File::create(part_path)
            .map_err(|e| FetchErr::Net(format!("cannot write the download file: {e}")))?;
        let mut buf = vec![0u8; 64 * 1024];
        let mut total: u64 = 0;
        loop {
            if stop() {
                drop(file);
                let _ = std::fs::remove_file(part_path);
                let _ = self.cmd("ABOR");
                return Ok(DownloadOutcome::Cancelled);
            }
            match data.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    file.write_all(&buf[..n])
                        .map_err(|e| FetchErr::Net(format!("write failed mid-download: {e}")))?;
                    total += n as u64;
                }
                Err(e) => {
                    drop(file);
                    let _ = std::fs::remove_file(part_path);
                    return Err(FetchErr::Net(format!("download interrupted: {e}")));
                }
            }
        }
        let _ = self.read_reply();
        if total == 0 {
            let _ = std::fs::remove_file(part_path);
            return Err(FetchErr::NotFound);
        }
        Ok(DownloadOutcome::Done(total))
    }
}

/// The last non-empty line of a server reply, trimmed — the part a
/// human would quote back. Server text only: replies answer our
/// commands, they never echo them, so this cannot carry credentials.
fn reply_tail(text: &str) -> String {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

/// "(227 Entering Passive Mode (192,168,1,1,195,80)" -> host, port.
pub fn parse_pasv(text: &str) -> Option<(String, u16)> {
    let start = text.find('(')?;
    let end = text.find(')')?;
    let nums: Vec<u16> = text[start + 1..end]
        .split(',')
        .filter_map(|n| n.trim().parse().ok())
        .collect();
    if nums.len() != 6 {
        return None;
    }
    Some((
        format!("{}.{}.{}.{}", nums[0], nums[1], nums[2], nums[3]),
        nums[4] * 256 + nums[5],
    ))
}

pub fn parse_list(text: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    for line in text.lines() {
        // MS-DOS / IIS LIST: "MM-DD-YY  hh:mmAM  <DIR>|<size>  name".
        // The live file server speaks this dialect; without this
        // branch every entry silently dropped and the walk saw an
        // empty tree.
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() >= 4 && is_dos_date(fields[0]) && is_dos_time(fields[1]) {
            let is_dir = fields[2] == "<DIR>";
            let name = fields[3..].join(" ");
            if name != "." && name != ".." {
                out.push((name, is_dir));
            }
            continue;
        }
        let is_dir = line.starts_with('d');
        // Unix LIST: 8 whitespace-separated fields, the rest of the
        // line is the name (names may contain spaces).
        let name_parts: Vec<&str> = line.split_whitespace().skip(8).collect();
        if name_parts.is_empty() {
            continue;
        }
        let name = name_parts.join(" ");
        if name == "." || name == ".." {
            continue;
        }
        out.push((name, is_dir));
    }
    out
}

/// "04-11-26" / "10-29-2025" — the date token of a DOS-style listing.
fn is_dos_date(tok: &str) -> bool {
    let parts: Vec<&str> = tok.split('-').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// "02:58AM" / "12:13PM" — the time token of a DOS-style listing.
fn is_dos_time(tok: &str) -> bool {
    (tok.ends_with("AM") || tok.ends_with("PM")) && tok.contains(':')
}

/// Score a folder for a trailer tier: the user's quality appetite
/// should win when the same game exists at several tiers.
fn tier_score(dir_name: &str, tier: &str) -> i32 {
    let d = dir_name.to_lowercase();
    let mut score = 0;
    let tokens: &[&str] = match tier {
        "standard" => &["hq", "480", "standard", "sq"],
        "hd" => &["hd", "1080", "hq"],
        _ => &["sq", "240", "small"],
    };
    for (i, t) in tokens.iter().enumerate() {
        if d.split(|c: char| !c.is_alphanumeric()).any(|w| w == *t) {
            score += 10 - i as i32;
            break;
        }
    }
    if d.contains("video") {
        score += 1;
    }
    score
}

/// A system's EmuMovies video inventory: (full remote path, stem),
/// walked once per process and cached — one walk serves the job.
static EMU_INVENTORY: Mutex<Option<HashMap<String, Result<Vec<(String, String)>, String>>>> =
    Mutex::new(None);

fn emu_inventory(
    sys: &str,
    creds: &AccountCreds,
) -> Result<Vec<(String, String)>, String> {
    {
        let guard = EMU_INVENTORY.lock().unwrap();
        if let Some(cache) = guard.as_ref() {
            if let Some(v) = cache.get(sys) {
                return v.clone();
            }
        }
    }
    let result = emu_walk(sys, creds);
    let mut guard = EMU_INVENTORY.lock().unwrap();
    guard
        .get_or_insert_with(HashMap::new)
        .insert(sys.to_string(), result.clone());
    result
}

/// How promising a folder is for this system's trailers. The live
/// layout is root → Official → Video Snaps (<tier>) → <system> →
/// files, and the root also carries deep branches that hold no
/// trailers (Artwork, Bezels, Themes…). A naive stack dive spends
/// the whole call budget in those branches and never reaches a
/// snap folder (proven against the live server 2026-10-16: 48/48
/// calls, zero systems found), so the walk descends content-first.
fn dir_rank(name: &str, tokens: &[&str]) -> usize {
    let d = name.to_lowercase();
    if d == "official" {
        0
    } else if tokens.iter().any(|t| d.contains(t)) {
        1
    } else if d.contains("video snaps") {
        2
    } else {
        3
    }
}

fn emu_walk(sys: &str, creds: &AccountCreds) -> Result<Vec<(String, String)>, String> {
    let map = system_map(sys)
        .ok_or_else(|| "no sources cover this system".to_string())?;
    let mut ftp = Ftp::connect(EMU_HOST)?;
    ftp.login(&creds.user, &creds.pass)
        .map_err(|e| format!("EmuMovies {e}"))?;
    let mut calls = 0usize;
    // Guided pass first: descend only through the container chain
    // and folders naming this system — the known live layout, ~17
    // calls for a mainstream system. If it finds nothing, the
    // layout has changed: fall back to the defensive full walk with
    // whatever budget remains.
    let mut found = emu_walk_pass(&mut ftp, &map, true, &mut calls);
    if found.is_empty() {
        found = emu_walk_pass(&mut ftp, &map, false, &mut calls);
    }
    let _ = ftp.cmd("QUIT");
    Ok(found)
}

/// One breadth-first pass to depth 3 under the shared call budget,
/// best-first at every level (see dir_rank). Guided passes skip
/// rank-3 folders outright; defensive passes enter everything but
/// still prune the last level to the folders naming this system
/// when any do — the budget belongs to the system being
/// inventoried, not its hundred neighbours.
fn emu_walk_pass(
    ftp: &mut Ftp,
    map: &SystemMap,
    guided: bool,
    calls: &mut usize,
) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = Vec::new();
    let mut queue: Vec<(String, usize)> = vec![(String::new(), 0usize)];
    let mut head = 0usize;
    while head < queue.len() {
        if *calls >= 48 {
            break;
        }
        let (dir, depth) = queue[head].clone();
        head += 1;
        *calls += 1;
        let entries = match ftp.list(if dir.is_empty() { None } else { Some(dir.as_str()) }) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let mut child_dirs: Vec<(String, String)> = Vec::new();
        for (name, is_dir) in entries {
            let path = if dir.is_empty() {
                name.clone()
            } else {
                format!("{dir}/{name}")
            };
            if is_dir {
                if depth < 3 {
                    child_dirs.push((name, path));
                }
            } else {
                let lower = name.to_lowercase();
                let ext = lower.rsplit('.').next().unwrap_or("");
                if VIDEO_EXTS.contains(&ext) {
                    // Attribute the file to this system when any
                    // ancestor folder carries a system token.
                    let hay = path.to_lowercase();
                    if map.emu_tokens.iter().any(|t| hay.contains(t)) {
                        found.push((path, file_stem(&name)));
                    }
                }
            }
        }
        child_dirs.sort_by_cached_key(|(name, _)| {
            (dir_rank(name, map.emu_tokens), name.to_lowercase())
        });
        let any_system = child_dirs
            .iter()
            .any(|(name, _)| dir_rank(name, map.emu_tokens) == 1);
        for (name, path) in child_dirs {
            let r = dir_rank(&name, map.emu_tokens);
            if r == 3 && (guided || (depth == 2 && any_system)) {
                continue;
            }
            queue.push((path, depth + 1));
        }
    }
    found
}

fn emumovies_fetch(
    spec: &FetchSpec,
    settings: &RomMediaSettings,
    part_path: &Path,
    stop: &dyn Fn() -> bool,
) -> FetchResult {
    if spec.media_type != "trailer" {
        return FetchResult::Missing;
    }
    let Some(creds) = account_creds("emumovies") else {
        return FetchResult::Missing;
    };
    let inventory = match emu_inventory(&spec.system, &creds) {
        Ok(i) => i,
        Err(note) => return FetchResult::Error { note },
    };
    let want = spec.rom_stem.to_lowercase();
    let matches: Vec<&(String, String)> = inventory
        .iter()
        .filter(|(_, stem)| stem.to_lowercase() == want)
        .collect();
    if matches.is_empty() {
        return FetchResult::Missing;
    }
    // Tier appetite breaks ties between folders.
    let best = matches
        .iter()
        .max_by_key(|(path, _)| {
            let dir = path.rsplit('/').nth(1).unwrap_or("");
            tier_score(dir, &settings.trailer_tier)
        })
        .unwrap();
    let remote = best.0.clone();
    let mut ftp = match Ftp::connect(EMU_HOST) {
        Ok(f) => f,
        Err(note) => return FetchResult::Error { note },
    };
    if let Err(e) = ftp.login(&creds.user, &creds.pass) {
        return FetchResult::Error {
            note: format!("EmuMovies {e}"),
        };
    }
    let ext = ext_from(&remote, "mp4");
    let result = ftp.retr_to_file(&remote, part_path, stop);
    let _ = ftp.cmd("QUIT");
    match result {
        Ok(DownloadOutcome::Done(_)) => {
            FetchResult::Downloaded { source: "emumovies", ext }
        }
        Ok(DownloadOutcome::Cancelled) => FetchResult::Cancelled,
        Err(FetchErr::NotFound) => FetchResult::Missing,
        Err(e) => FetchResult::Error { note: format!("EmuMovies: {e}") },
    }
}

// ---------------------------------------------------------------------------
// Storage finalization: part file -> real path -> canonical mirror
// (when a type lives elsewhere) -> RetroArch mirror for images.
// ---------------------------------------------------------------------------

/// Does a file for this stem already sit in the type's folders (the
/// user's own drop-ins count, whatever their extension)?
fn existing_file_for_stem(
    settings: &RomMediaSettings,
    sys: &str,
    media_type: &str,
    stem: &str,
) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(c) = canonical_path(settings, sys, media_type, stem, "x").parent().map(|p| p.to_path_buf()) {
        dirs.push(c);
    }
    if let Some(r) = real_path(settings, sys, media_type, stem, "x").parent().map(|p| p.to_path_buf()) {
        if !dirs.contains(&r) {
            dirs.push(r);
        }
    }
    for dir in dirs {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if file_stem(&name) == stem && e.path().is_file() {
                    return Some(e.path());
                }
            }
        }
    }
    None
}

/// The RetroArch thumbnails folder for a type (None for trailers —
/// RetroArch has no video thumbnails).
fn ra_folder(media_type: &str) -> Option<&'static str> {
    match media_type {
        "boxart" => Some("Named_Boxarts"),
        "screenshot" => Some("Named_Snaps"),
        "titleshot" => Some("Named_Titles"),
        _ => None,
    }
}

/// Mirror one stored file into the RetroArch tree. PNG stays a link
/// (or copy); other formats convert to PNG for the mirror only —
/// RetroArch's default thumbnail loader speaks PNG.
fn ra_mirror(
    settings: &RomMediaSettings,
    spec: &FetchSpec,
    stored: &Path,
) -> Result<(), String> {
    let (Some(ra_dir), Some(folder)) = (&settings.retroarch_dir, ra_folder(&spec.media_type)) else {
        return Ok(());
    };
    let Some(map) = system_map(&spec.system) else {
        return Ok(());
    };
    let playlist = map.libretro.first().copied().unwrap_or("");
    if playlist.is_empty() {
        return Ok(());
    }
    let target = PathBuf::from(ra_dir)
        .join(playlist)
        .join(folder)
        .join(format!("{}.png", libretro_name(&spec.dat_name)));
    if target.exists() {
        return Ok(());
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let is_png = stored
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase() == "png")
        .unwrap_or(false);
    if is_png {
        link_or_copy(stored, &target).map_err(|e| e.to_string())?;
    } else {
        let img = image::open(stored).map_err(|e| e.to_string())?;
        img.save_with_format(&target, image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Move a completed download home: real path first (atomic rename —
/// part files live in the same directory), canonical mirror when the
/// type lives elsewhere, RetroArch mirror for images. Returns the
/// path the catalog should record (the canonical path when it
/// exists, so every frontend and the report point at one place).
fn finalize_download(
    settings: &RomMediaSettings,
    spec: &FetchSpec,
    part_path: &Path,
    ext: &str,
) -> Result<PathBuf, String> {
    let real = real_path(settings, &spec.system, &spec.media_type, &spec.rom_stem, ext);
    if let Some(parent) = real.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create the media folder: {e}"))?;
    }
    if real.exists() {
        // Never overwrite: someone (or an earlier step) filed one.
        let _ = std::fs::remove_file(part_path);
    } else {
        std::fs::rename(part_path, &real)
            .map_err(|e| format!("cannot file the download: {e}"))?;
    }
    let canonical = canonical_path(settings, &spec.system, &spec.media_type, &spec.rom_stem, ext);
    let recorded = if canonical != real {
        match link_or_copy(&real, &canonical) {
            Ok(_) => canonical,
            Err(_) => real.clone(),
        }
    } else {
        real.clone()
    };
    // The RetroArch mirror is a bonus view: its failure never fails
    // a fetch that already landed.
    let _ = ra_mirror(settings, spec, &recorded);
    Ok(recorded)
}

// ---------------------------------------------------------------------------
// The step executor body — called by commands.rs's FetchMedia arm.
// Verdicts map to engine StepOutcome in the caller: Done, Skipped
// (missing / ambiguous / source error — all recorded), Failed (local
// IO only), Cancelled.
// ---------------------------------------------------------------------------

pub enum FetchVerdict {
    Done,
    Skipped,
    Failed(String),
    Cancelled,
}

pub fn run_fetch(
    spec: &FetchSpec,
    base: &Path,
    catalog_path: &Path,
    stop: &dyn Fn() -> bool,
    wait_pause: &dyn Fn() -> bool,
    progress: &dyn Fn(&str),
) -> FetchVerdict {
    let settings = load_settings(base);
    let catalog = match ufm_catalog::Catalog::open(catalog_path) {
        Ok(c) => c,
        Err(e) => return FetchVerdict::Failed(format!("catalog open failed: {e}")),
    };
    let record = |status: &str, source: Option<&str>, path: Option<String>, detail: serde_json::Value| {
        let _ = catalog.upsert_rom_media(
            &spec.item_id,
            &spec.media_type,
            status,
            source,
            path.as_deref(),
            &detail.to_string(),
        );
    };
    if system_map(&spec.system).is_none() {
        record("missing", None, None, serde_json::json!({"note": "No media sources cover this system yet."}));
        return FetchVerdict::Skipped;
    }
    // The user's own files win before any network is touched.
    if let Some(existing) = existing_file_for_stem(&settings, &spec.system, &spec.media_type, &spec.rom_stem) {
        let _ = ra_mirror(&settings, spec, &existing);
        record(
            "found",
            Some("manual"),
            Some(existing.to_string_lossy().into_owned()),
            serde_json::json!({}),
        );
        return FetchVerdict::Done;
    }
    let part_parent = real_path(&settings, &spec.system, &spec.media_type, &spec.rom_stem, "bin")
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| settings.root_path());
    if let Err(e) = std::fs::create_dir_all(&part_parent) {
        return FetchVerdict::Failed(format!("cannot create the media folder: {e}"));
    }
    let part_path = part_parent.join(format!(
        ".fable-part-{}-{}",
        spec.item_id.replace(['/', '\\'], "_"),
        spec.media_type
    ));
    let agent = http_agent();
    let mut remembered_ambiguous: Option<Vec<Candidate>> = None;
    let mut remembered_error: Option<String> = None;
    for source in cascade_for(&spec.media_type) {
        if stop() {
            return FetchVerdict::Cancelled;
        }
        if !wait_pause() {
            return FetchVerdict::Cancelled;
        }
        let available = match source {
            "libretro" => true,
            "screenscraper" => ss_dev_creds().is_some(),
            "emumovies" => account_creds("emumovies").is_some(),
            "thegamesdb" => secret_get("thegamesdb").is_some(),
            "steamgriddb" => secret_get("steamgriddb").is_some(),
            _ => false,
        };
        if !available {
            continue;
        }
        if let Some(note) = source_down(source) {
            if remembered_error.is_none() {
                remembered_error = Some(note);
            }
            continue;
        }
        progress(&format!(
            "{} \u{b7} {} \u{b7} {}",
            spec.dat_name,
            media_type_label(&spec.media_type),
            source_label(source)
        ));
        let result = match source {
            "libretro" => libretro_fetch(&agent, spec, &part_path, stop),
            "screenscraper" => ss_fetch(&agent, spec, &part_path, stop),
            "thegamesdb" => tgdb_fetch(&agent, spec, &part_path, stop),
            "steamgriddb" => sgdb_fetch(&agent, spec, &part_path, stop),
            "emumovies" => emumovies_fetch(spec, &settings, &part_path, stop),
            _ => FetchResult::Missing,
        };
        match result {
            FetchResult::Downloaded { source: src, ext } => {
                match finalize_download(&settings, spec, &part_path, &ext) {
                    Ok(recorded) => {
                        record(
                            "found",
                            Some(src),
                            Some(recorded.to_string_lossy().into_owned()),
                            serde_json::json!({}),
                        );
                        return FetchVerdict::Done;
                    }
                    Err(e) => return FetchVerdict::Failed(e),
                }
            }
            FetchResult::Missing => continue,
            FetchResult::Ambiguous { candidates } => {
                if remembered_ambiguous.is_none() {
                    remembered_ambiguous = Some(candidates);
                }
                continue;
            }
            FetchResult::Error { note } => {
                if note.contains("could not reach") || note.contains("could not connect") {
                    mark_source_down(source, &note);
                }
                if remembered_error.is_none() {
                    remembered_error = Some(note);
                }
                continue;
            }
            FetchResult::Cancelled => return FetchVerdict::Cancelled,
        }
    }
    let _ = std::fs::remove_file(&part_path);
    if let Some(candidates) = remembered_ambiguous {
        record(
            "ambiguous",
            None,
            None,
            serde_json::json!({ "candidates": candidates }),
        );
        return FetchVerdict::Skipped;
    }
    if let Some(note) = remembered_error {
        record("error", None, None, serde_json::json!({ "note": note }));
        return FetchVerdict::Skipped;
    }
    record("missing", None, None, serde_json::json!({}));
    FetchVerdict::Skipped
}

// ---------------------------------------------------------------------------
// Planner — a job's steps from the catalog + the media cache.
// ---------------------------------------------------------------------------

struct RomEntry {
    item_id: String,
    dat_name: String,
    rom_stem: String,
    crc: Option<String>,
    md5: Option<String>,
    sha1: Option<String>,
    size: Option<u64>,
}

fn rom_entries(
    catalog: &ufm_catalog::Catalog,
    system: &str,
) -> Vec<RomEntry> {
    let rows = catalog.rom_collection_items(system, None).unwrap_or_default();
    let mut out = Vec::with_capacity(rows.len());
    for (item, title) in rows {
        let mut entry = RomEntry {
            item_id: item.id.to_string(),
            dat_name: title.clone(),
            rom_stem: file_stem(&item.name),
            crc: None,
            md5: None,
            sha1: None,
            size: item.size_bytes.map(|s| s as u64),
        };
        if let Ok(Some(row)) = catalog.media_identity_for(&item.id) {
            entry.sha1 = row.sha1.clone();
            if let Ok(ev) = serde_json::from_str::<serde_json::Value>(&row.evidence_json) {
                if let Some(crc) = ev.get("crc").and_then(|v| v.as_u64()) {
                    entry.crc = Some(format!("{crc:08x}"));
                }
                if let Some(md5) = ev.get("md5").and_then(|v| v.as_str()) {
                    entry.md5 = Some(md5.to_string());
                }
                if entry.size.is_none() {
                    entry.size = ev.get("size").and_then(|v| v.as_u64());
                }
            }
        }
        out.push(entry);
    }
    out
}

/// Is a cached negative still fresh enough to trust? (30 days —
/// source catalogs grow, but not daily; errors never count as fresh
/// and are replanned every run.)
fn negative_is_fresh(row: &ufm_catalog::RomMediaRow) -> bool {
    if row.status != "missing" {
        return false;
    }
    let Ok(checked) = chrono::DateTime::parse_from_rfc3339(&row.checked_at) else {
        return false;
    };
    let age = chrono::Utc::now() - checked.with_timezone(&chrono::Utc);
    age.num_days() < 30
}

/// Build the download job. `systems` None = every known system with
/// identified ROMs. `retry` = explicit (item_id, media_type) pairs
/// from the report — replanned regardless of the cache (except when
/// the file is already on disk).
pub fn plan_download(
    catalog: &ufm_catalog::Catalog,
    settings: &RomMediaSettings,
    systems: Option<Vec<String>>,
    retry: &[(String, String)],
    system_label: Option<&str>,
) -> Result<ufm_jobs::Job, String> {
    let enabled_types: Vec<&str> = MEDIA_TYPES
        .iter()
        .copied()
        .filter(|t| settings.type_settings(t).enabled)
        .collect();
    let mut steps: Vec<ufm_jobs::Step> = Vec::new();
    let mut add_step = |entry: &RomEntry, system: &str, media_type: &str| {
        steps.push(ufm_jobs::Step {
            id: format!("{}:{}", entry.item_id, media_type),
            label: format!(
                "{} \u{b7} {}",
                entry.dat_name,
                media_type_label(media_type)
            ),
            op: ufm_jobs::StepOp::FetchMedia {
                item_id: entry.item_id.clone(),
                system: system.to_string(),
                dat_name: entry.dat_name.clone(),
                rom_stem: entry.rom_stem.clone(),
                media_type: media_type.to_string(),
                crc: entry.crc.clone(),
                md5: entry.md5.clone(),
                sha1: entry.sha1.clone(),
                size: entry.size,
            },
            depends_on: vec![],
            state: ufm_jobs::StepState::Pending,
            compensation: None,
            outcome: false,
            outcome_path: None,
        });
    };
    // Captured before `systems` is consumed by the planning below.
    let systems_count = systems.as_ref().map(|v| v.len()).unwrap_or(usize::MAX);
    if !retry.is_empty() {
        // Resolve specs for the retried items from their identities.
        let mut by_item: HashMap<String, (String, RomEntry)> = HashMap::new();
        for (item_id, _) in retry {
            if by_item.contains_key(item_id) {
                continue;
            }
            let Ok(uuid) = uuid::Uuid::parse_str(item_id) else { continue };
            let id = ufm_core::ItemId(uuid);
            if let Ok(Some(row)) = catalog.media_identity_for(&id) {
                let system = row.platform.clone().unwrap_or_default();
                if system.is_empty() {
                    continue;
                }
                // The file stem comes from the report's own rows
                // (provider key); fall back to the DAT name, which is
                // the stem whenever files carry DAT names.
                let mut stem = row.title.clone().unwrap_or_default();
                if let Ok(rows) = catalog.rom_media_for_system(&system) {
                    for (r, _, pk) in &rows {
                        if r.item_id == *item_id {
                            if let Some(pk) = pk {
                                stem = file_stem(
                                    Path::new(pk)
                                        .file_name()
                                        .map(|n| n.to_string_lossy().into_owned())
                                        .unwrap_or_default()
                                        .as_str(),
                                );
                            }
                            break;
                        }
                    }
                }
                let mut entry = RomEntry {
                    item_id: item_id.clone(),
                    dat_name: row.title.clone().unwrap_or_default(),
                    rom_stem: stem,
                    crc: None,
                    md5: None,
                    sha1: row.sha1.clone(),
                    size: None,
                };
                if let Ok(ev) = serde_json::from_str::<serde_json::Value>(&row.evidence_json) {
                    if let Some(crc) = ev.get("crc").and_then(|v| v.as_u64()) {
                        entry.crc = Some(format!("{crc:08x}"));
                    }
                    entry.md5 = ev
                        .get("md5")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    entry.size = ev.get("size").and_then(|v| v.as_u64());
                }
                by_item.insert(item_id.clone(), (system, entry));
            }
        }
        for (item_id, media_type) in retry {
            if let Some((system, entry)) = by_item.get(item_id) {
                if existing_file_for_stem(settings, system, media_type, &entry.rom_stem).is_none() {
                    add_step(entry, system, media_type);
                }
            }
        }
    } else {
        let systems: Vec<String> = match systems {
            Some(s) => s,
            None => known_systems().iter().map(|s| s.to_string()).collect(),
        };
        for system in &systems {
            for entry in rom_entries(catalog, system) {
                for media_type in &enabled_types {
                    match catalog.rom_media_for(&entry.item_id, media_type) {
                        Ok(Some(row)) => match row.status.as_str() {
                            "found" => {
                                let exists = row
                                    .local_path
                                    .as_deref()
                                    .map(|p| Path::new(p).exists())
                                    .unwrap_or(false);
                                if exists {
                                    continue;
                                }
                            }
                            "missing" if negative_is_fresh(&row) => continue,
                            "ambiguous" => continue,
                            _ => {}
                        },
                        _ => {}
                    }
                    if let Some(p) =
                        existing_file_for_stem(settings, system, media_type, &entry.rom_stem)
                    {
                        // File the user's own copy into the cache so
                        // the report tells the truth.
                        { let p = &p;
                            let _ = catalog.upsert_rom_media(
                                &entry.item_id,
                                media_type,
                                "found",
                                Some("manual"),
                                Some(&p.to_string_lossy()),
                                "{}",
                            );
                        }
                        continue;
                    }
                    add_step(&entry, system, media_type);
                }
            }
        }
    }
    if steps.is_empty() {
        return Err(
            "Nothing to fetch: everything switched on is already on disk, still fresh in the not-found cache, or waiting for a pick in the media report."
                .into(),
        );
    }
    let title = match system_label {
        Some(label) if systems_count <= 1 && retry.is_empty() => {
            format!("Game media \u{2014} {label}")
        }
        _ if !retry.is_empty() => "Game media \u{2014} retry".to_string(),
        _ => "Game media \u{2014} all systems".to_string(),
    };
    let mut preview = ufm_jobs::JobPreview::default();
    preview.files_affected = steps.len() as u64;
    let mut job = ufm_jobs::Job::new(title, preview);
    for step in steps {
        job.add_step(step);
    }
    Ok(job)
}

// ---------------------------------------------------------------------------
// Confirming an ambiguous match (the report's one-click human act):
// download the chosen candidate now and file it like any fetch.
// ---------------------------------------------------------------------------

pub fn confirm_match(
    catalog: &ufm_catalog::Catalog,
    base: &Path,
    item_id: &str,
    media_type: &str,
    source: &str,
    url: &str,
) -> Result<String, String> {
    let settings = load_settings(base);
    let Ok(uuid) = uuid::Uuid::parse_str(item_id) else {
        return Err("that game is no longer in the library".into());
    };
    let id = ufm_core::ItemId(uuid);
    let row = catalog
        .media_identity_for(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "that game is no longer identified".to_string())?;
    let system = row.platform.clone().unwrap_or_default();
    let dat_name = row.title.clone().unwrap_or_default();
    // Recover the ROM's file stem from the report rows when possible.
    let mut stem = dat_name.clone();
    if let Ok(rows) = catalog.rom_media_for_system(&system) {
        for (r, _, pk) in &rows {
            if r.item_id == item_id {
                if let Some(pk) = pk {
                    stem = file_stem(
                        Path::new(pk)
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default()
                            .as_str(),
                    );
                }
                break;
            }
        }
    }
    let spec = FetchSpec {
        item_id: item_id.to_string(),
        system: system.clone(),
        dat_name,
        rom_stem: stem.clone(),
        media_type: media_type.to_string(),
        crc: None,
        md5: None,
        sha1: row.sha1.clone(),
        size: None,
    };
    let part_parent = real_path(&settings, &system, media_type, &stem, "bin")
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| settings.root_path());
    std::fs::create_dir_all(&part_parent).map_err(|e| e.to_string())?;
    let part_path = part_parent.join(format!(".fable-part-{item_id}-{media_type}"));
    let agent = http_agent();
    match http_download(&agent, url, &part_path, &|| false) {
        Ok(DownloadOutcome::Done(_)) => {}
        Ok(DownloadOutcome::Cancelled) => return Err("download cancelled".into()),
        Err(e) => return Err(format!("that download did not land: {e}")),
    }
    let ext = ext_from(url, if media_type == "trailer" { "mp4" } else { "jpg" });
    let recorded = finalize_download(&settings, &spec, &part_path, &ext)?;
    catalog
        .upsert_rom_media(
            item_id,
            media_type,
            "found",
            Some(source),
            Some(&recorded.to_string_lossy()),
            "{}",
        )
        .map_err(|e| e.to_string())?;
    Ok(format!(
        "Filed under {} \u{b7} {}.",
        system_display(&system),
        spec.dat_name
    ))
}

// ---------------------------------------------------------------------------
// The missing-media report.
// ---------------------------------------------------------------------------

pub fn system_display(sys: &str) -> String {
    match sys {
        "3do" => "3DO", "amiga" => "Amiga", "amigacd32" => "Amiga CD32",
        "arcade" => "Arcade (MAME)", "arcadia2001" => "Arcadia 2001",
        "atari2600" => "Atari 2600", "atari5200" => "Atari 5200",
        "atari7800" => "Atari 7800", "atari8bit" => "Atari 8-Bit",
        "c64" => "Commodore 64", "cdi" => "CD-i", "channelf" => "Channel F",
        "colecovision" => "ColecoVision", "dreamcast" => "Dreamcast",
        "gamecom" => "Game.com", "gamecube" => "GameCube",
        "gamegear" => "Game Gear", "gb" => "Game Boy", "gba" => "Game Boy Advance",
        "gbc" => "Game Boy Color", "genesis" => "Genesis", "gx4000" => "GX4000",
        "intellivision" => "Intellivision", "jaguar" => "Atari Jaguar",
        "lynx" => "Atari Lynx", "mastersystem" => "Master System", "msx" => "MSX",
        "n3ds" => "Nintendo 3DS", "n64" => "Nintendo 64", "nds" => "Nintendo DS",
        "neogeo" => "Neo Geo", "neogeocd" => "Neo Geo CD",
        "neogeopocket" => "Neo Geo Pocket", "nes" => "Nintendo Entertainment System",
        "odyssey2" => "Odyssey\u{b2}", "pcfx" => "PC-FX", "ps2" => "PlayStation 2",
        "ps3" => "PlayStation 3", "psp" => "PlayStation Portable",
        "psvita" => "PS Vita", "psx" => "PlayStation", "pv1000" => "PV-1000",
        "saturn" => "Saturn", "scv" => "Super Cassette Vision",
        "sega32x" => "Sega 32X", "segacd" => "Sega CD", "sg1000" => "SG-1000",
        "snes" => "Super Nintendo Entertainment System", "studio2" => "Studio II",
        "superacan" => "Super A'can", "tg16" => "TurboGrafx-16",
        "vectrex" => "Vectrex", "vic20" => "VIC-20", "virtualboy" => "Virtual Boy",
        "wii" => "Wii", "wiiu" => "Wii U", "wonderswan" => "WonderSwan",
        "xbox" => "Xbox", "xbox360" => "Xbox 360", "zxspectrum" => "ZX Spectrum",
        other => other,
    }
    .to_string()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeCoverage {
    pub media_type: String,
    pub have: usize,
    pub total: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MissingRow {
    pub item_id: String,
    pub title: String,
    pub media_type: String,
    /// missing | ambiguous | error
    pub status: String,
    pub reason: String,
    pub candidates: Vec<Candidate>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemReport {
    pub system: String,
    pub display: String,
    pub total_games: usize,
    pub coverage: Vec<TypeCoverage>,
    pub missing: Vec<MissingRow>,
    pub truncated_missing: usize,
    /// Where this system's files of a type belong (DIY drop-ins).
    pub diy_folder: String,
    pub diy_pattern: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportData {
    pub systems: Vec<SystemReport>,
    pub generated_at: String,
}

const REPORT_ROW_CAP: usize = 400;

pub fn build_report(
    catalog: &ufm_catalog::Catalog,
    settings: &RomMediaSettings,
    only_system: Option<&str>,
) -> ReportData {
    let systems: Vec<String> = match only_system {
        Some(s) => vec![s.to_string()],
        None => known_systems().iter().map(|s| s.to_string()).collect(),
    };
    let mut out = Vec::new();
    for system in &systems {
        let identified = catalog.rom_collection_items(system, None).unwrap_or_default();
        if identified.is_empty() && only_system.is_none() {
            continue;
        }
        let total_games = identified.len();
        let rows = catalog.rom_media_for_system(system).unwrap_or_default();
        let mut by_key: HashMap<(String, String), &ufm_catalog::RomMediaRow> = HashMap::new();
        for (row, _, _) in &rows {
            by_key.insert((row.item_id.clone(), row.media_type.clone()), row);
        }
        let mut coverage = Vec::new();
        for t in MEDIA_TYPES {
            let have = rows
                .iter()
                .filter(|(r, _, _)| r.media_type == t && r.status == "found")
                .count();
            coverage.push(TypeCoverage {
                media_type: t.to_string(),
                have,
                total: total_games,
            });
        }
        let mut missing: Vec<MissingRow> = Vec::new();
        // Attempted rows that did not land.
        for (row, title, _) in &rows {
            if row.status == "found" {
                continue;
            }
            let detail: serde_json::Value =
                serde_json::from_str(&row.detail_json).unwrap_or_default();
            let (reason, candidates) = match row.status.as_str() {
                "ambiguous" => {
                    let cands: Vec<Candidate> = detail
                        .get("candidates")
                        .and_then(|c| serde_json::from_value(c.clone()).ok())
                        .unwrap_or_default();
                    (
                        "A few possible matches \u{2014} pick the right one and it is filed for good."
                            .to_string(),
                        cands,
                    )
                }
                "error" => (
                    detail
                        .get("note")
                        .and_then(|n| n.as_str())
                        .map(|n| format!("{n} Retry when you like."))
                        .unwrap_or_else(|| "A source did not answer. Retry when you like.".into()),
                    vec![],
                ),
                _ => (
                    "Not in any source we checked. Art for hacks, translations and prototypes is rare \u{2014} the do-it-yourself note below is the way for those."
                        .to_string(),
                    vec![],
                ),
            };
            missing.push(MissingRow {
                item_id: row.item_id.clone(),
                title: title.clone(),
                media_type: row.media_type.clone(),
                status: row.status.clone(),
                reason,
                candidates,
            });
        }
        // Never-attempted (item, enabled type) pairs.
        for (item, title) in &identified {
            for t in MEDIA_TYPES {
                if !settings.type_settings(t).enabled {
                    continue;
                }
                if by_key.contains_key(&(item.id.to_string(), t.to_string())) {
                    continue;
                }
                missing.push(MissingRow {
                    item_id: item.id.to_string(),
                    title: title.clone(),
                    media_type: t.to_string(),
                    status: "missing".into(),
                    reason: "Not downloaded yet.".into(),
                    candidates: vec![],
                });
            }
        }
        missing.sort_by(|a, b| {
            let rank = |s: &str| match s {
                "ambiguous" => 0,
                "error" => 1,
                _ => 2,
            };
            rank(&a.status)
                .cmp(&rank(&b.status))
                .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
                .then_with(|| a.media_type.cmp(&b.media_type))
        });
        let truncated = missing.len().saturating_sub(REPORT_ROW_CAP);
        missing.truncate(REPORT_ROW_CAP);
        let diy_folder = canonical_path(settings, system, "boxart", "x", "x")
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        out.push(SystemReport {
            system: system.clone(),
            display: system_display(system),
            total_games,
            coverage,
            missing,
            truncated_missing: truncated,
            diy_folder,
            diy_pattern: "<the game's name, exactly as listed>.png".into(),
        });
    }
    ReportData {
        systems: out,
        generated_at: chrono::Utc::now().to_rfc3339(),
    }
}

// ---------------------------------------------------------------------------
// Account validation — plain-language verdicts, run on Save.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateResult {
    pub ok: bool,
    pub message: String,
}

pub fn validate_source(base: &Path, kind: &str) -> ValidateResult {
    let _ = base; // (settings-free checks; secrets come from the keyring)
    let agent = http_agent();
    match kind {
        "libretro" => ValidateResult {
            ok: true,
            message: "Always on. No account, no key \u{2014} box art, screenshots and title screens for most games, free.".into(),
        },
        "screenscraper" => {
            let Some(dev) = ss_dev_creds() else {
                return ValidateResult {
                    ok: false,
                    message: "ScreenScraper identifies each app with free developer credentials, and Fable's own registration is still in progress \u{2014} so this source cannot switch on yet. If you have your own free ScreenScraper developer credentials, paste them under Advanced below and this lights up today. Everything else on this page already works.".into(),
                };
            };
            let Some(creds) = account_creds("screenscraper") else {
                return ValidateResult {
                    ok: false,
                    message: "Add your ScreenScraper user name and password, then Connect. No account yet? It is free \u{2014} screenscraper.fr, create an account, come back with the name and password.".into(),
                };
            };
            let mut params = ss_base_params(&dev, &Some(creds.clone()));
            params.retain(|(k, _)| k != "output");
            params.push(("output".into(), "json".into()));
            let url = format!(
                "{SS_API}/ssuserInfos.php?{}",
                query(&params.iter().map(|(k, v)| (k.as_str(), v.clone())).collect::<Vec<_>>())
            );
            match http_get_json(&agent, &url) {
                Ok(json) => {
                    if let Some(user) = json.pointer("/response/ssuser") {
                        let threads = user
                            .get("maxthreads")
                            .and_then(|v| v.as_u64())
                            .map(|t| format!(" {t} download threads."))
                            .unwrap_or_default();
                        ValidateResult {
                            ok: true,
                            message: format!(
                                "Signed in as {} \u{2014} ScreenScraper is ready.{threads}",
                                creds.user
                            ),
                        }
                    } else {
                        ValidateResult {
                            ok: false,
                            message: "ScreenScraper did not accept that sign-in. Check the user name and password \u{2014} they are the ones you use on screenscraper.fr.".into(),
                        }
                    }
                }
                Err(e) => ValidateResult {
                    ok: false,
                    message: format!("Could not check the account: {e}"),
                },
            }
        }
        "emumovies" => {
            let Some(creds) = account_creds("emumovies") else {
                return ValidateResult {
                    ok: false,
                    message: "Add your EmuMovies user name and password, then Connect.".into(),
                };
            };
            match Ftp::connect(EMU_HOST) {
                Ok(mut ftp) => match ftp.login(&creds.user, &creds.pass) {
                    Ok(()) => {
                        let _ = ftp.cmd("QUIT");
                        ValidateResult {
                            ok: true,
                            message: format!(
                                "Signed in as {} \u{2014} the EmuMovies file server answered, so trailers can come from the deepest video library in the hobby.",
                                creds.user
                            ),
                        }
                    }
                    Err(note) => ValidateResult {
                        ok: false,
                        message: format!(
                            "The EmuMovies file server refused that sign-in \u{2014} its own words: \u{201c}{note}\u{201d}. That is the server's answer about the file-server door, not proof the password is wrong: if the same name and password sign you in on emumovies.com, the account is real, and whether it also opens the file server is set by the membership on it (Fable cannot see your tier). Trailers will come from ScreenScraper clips until then \u{2014} nothing else is affected."
                        ),
                    },
                },
                Err(note) => ValidateResult { ok: false, message: note },
            }
        }
        "thegamesdb" => {
            let Some(key) = secret_get("thegamesdb") else {
                return ValidateResult {
                    ok: false,
                    message: "Paste your TheGamesDB API key, then Connect.".into(),
                };
            };
            let url = format!(
                "https://api.thegamesdb.net/v1/Games/ByGameName?{}",
                query(&[("apikey", key), ("name", "Contra".into())])
            );
            match http_get_json(&agent, &url) {
                Ok(json) => {
                    let code = json.get("code").and_then(|c| c.as_u64()).unwrap_or(0);
                    if code == 200 {
                        let allowance = json
                            .get("remaining_monthly_allowance")
                            .and_then(|a| a.as_u64())
                            .map(|a| format!(" {a} requests left this month \u{2014} Fable shows it as it spends."))
                            .unwrap_or_default();
                        ValidateResult {
                            ok: true,
                            message: format!("Key accepted \u{2014} TheGamesDB is ready.{allowance}"),
                        }
                    } else {
                        ValidateResult {
                            ok: false,
                            message: "TheGamesDB did not accept that key. Keys come from a free request on their forums (Developers \u{2192} API requests) and can take a little while to arrive.".into(),
                        }
                    }
                }
                Err(e) => ValidateResult {
                    ok: false,
                    message: format!("Could not check the key: {e}"),
                },
            }
        }
        "steamgriddb" => {
            let Some(key) = secret_get("steamgriddb") else {
                return ValidateResult {
                    ok: false,
                    message: "Paste your SteamGridDB API key, then Connect.".into(),
                };
            };
            let url = "https://www.steamgriddb.com/api/v2/search/autocomplete/contra";
            match agent
                .get(url)
                .set("Authorization", &format!("Bearer {key}"))
                .call()
            {
                Ok(resp) => {
                    let text = resp.into_string().unwrap_or_default();
                    let json: serde_json::Value =
                        serde_json::from_str(&text).unwrap_or_default();
                    if json.get("success").and_then(|s| s.as_bool()) == Some(true) {
                        ValidateResult {
                            ok: true,
                            message: "Key accepted \u{2014} SteamGridDB posters are ready for the games the other sources miss.".into(),
                        }
                    } else {
                        ValidateResult {
                            ok: false,
                            message: "SteamGridDB did not accept that key. Yours lives at steamgriddb.com \u{2192} your profile \u{2192} Preferences \u{2192} API.".into(),
                        }
                    }
                }
                Err(ureq::Error::Status(code, _)) => ValidateResult {
                    ok: false,
                    message: format!(
                        "SteamGridDB answered with an error (HTTP {code}) \u{2014} that usually means the key is not right."
                    ),
                },
                Err(e) => ValidateResult {
                    ok: false,
                    message: format!("Could not check the key: {e}"),
                },
            }
        }
        _ => ValidateResult { ok: false, message: "Unknown source.".into() },
    }
}

// ---------------------------------------------------------------------------
// Settings view for the frontend: settings + which secrets exist
// (presence only — secrets themselves never cross this boundary).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    pub settings: RomMediaSettings,
    pub secret_present: HashMap<String, bool>,
    pub keyring_ok: bool,
    pub ss_dev_ready: bool,
}

pub fn settings_view(base: &Path) -> SettingsView {
    let mut present = HashMap::new();
    for kind in SECRET_KINDS {
        present.insert(kind.to_string(), secret_get(kind).is_some());
    }
    SettingsView {
        settings: load_settings(base),
        secret_present: present,
        keyring_ok: keyring_available(),
        ss_dev_ready: ss_dev_creds().is_some(),
    }
}

// ---------------------------------------------------------------------------
// Tests — fixtures and pure functions only. (CI law: no live network
// in the suite. The one live smoke test is #[ignore]d and run by
// hand; see the build checkpoint.)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression (2026-10-15 TMDB keyring incident): production
    /// constructs a FRESH keyring Entry for every call. A store that
    /// keeps values only inside one Entry instance — the keyring
    /// crate's mock fallback, compiled in when no credential-store
    /// feature is enabled — passes `set` and loses the value, so
    /// this set-through-one-entry / read-through-a-second-one
    /// roundtrip fails against the mock EXACTLY as production did.
    /// Where no real store is reachable at all (headless CI without
    /// a Secret Service), `set` itself errors and the test stands
    /// down — the invariant under test is "a successful save is
    /// readable from a fresh Entry", not "a store exists here".
    #[test]
    fn keyring_roundtrip_through_fresh_entries() {
        const KIND: &str = "test-roundtrip";
        const VALUE: &str = "roundtrip-probe-value";
        if secret_set(KIND, Some(VALUE)).is_err() {
            return; // no reachable store in this environment
        }
        let read_back = secret_get(KIND);
        let _ = secret_set(KIND, None);
        assert_eq!(
            read_back.as_deref(),
            Some(VALUE),
            "a saved credential must read back through a fresh Entry"
        );
    }

    #[test]
    fn libretro_names_substitute_the_forbidden_ten() {
        assert_eq!(
            libretro_name("Rockman & Forte (Japan)"),
            "Rockman _ Forte (Japan)"
        );
        assert_eq!(
            libretro_name("A/B:C*D?E\"F<G>H|I`J"),
            "A_B_C_D_E_F_G_H_I_J"
        );
        // Apostrophes and parens survive — only the ten change.
        assert_eq!(
            libretro_name("Chip's Challenge (USA, Europe)"),
            "Chip's Challenge (USA, Europe)"
        );
    }

    #[test]
    fn clean_titles_strip_dat_tags() {
        assert_eq!(clean_title("Contra (USA) (Rev 1)"), "Contra");
        assert_eq!(
            clean_title("Super Mario Bros. 3 (USA) (Rev 1) [!]"),
            "Super Mario Bros. 3"
        );
        assert_eq!(clean_title("1942 (Japan)"), "1942");
    }

    #[test]
    fn title_keys_decide_exactness() {
        assert_eq!(title_key("Contra"), title_key("CONTRA "));
        assert_eq!(
            title_key("Super Mario Bros. 3"),
            title_key("Super Mario Bros 3")
        );
        assert_ne!(title_key("Contra"), title_key("Contra Force"));
        // The fuzzy law: prefix resemblance is NOT equality.
        let games = vec![
            ("1".to_string(), "Contra".to_string()),
            ("2".to_string(), "Contra Force".to_string()),
        ];
        assert!(matches!(
            classify_title_matches("Contra", &games),
            TitleMatch::Exact(ref id) if id == "1"
        ));
        let games = vec![
            ("1".to_string(), "Contra (NES)".to_string()),
            ("2".to_string(), "Contra (Arcade)".to_string()),
        ];
        // Both differ after normalization (parentheticals in the
        // SEARCH RESULT are not stripped) -> no forced choice.
        assert!(matches!(
            classify_title_matches("Contra", &games),
            TitleMatch::Ambiguous(_) | TitleMatch::None
        ));
        let games = vec![("7".to_string(), "Gradius".to_string())];
        assert!(matches!(
            classify_title_matches("Contra", &games),
            TitleMatch::None
        ));
    }

    #[test]
    fn regions_come_from_dat_tags() {
        assert_eq!(region_hint("Game (Japan)"), "jp");
        assert_eq!(region_hint("Game (Europe)"), "eu");
        assert_eq!(region_hint("Game (World)"), "wor");
        assert_eq!(region_hint("Game (USA)"), "us");
        assert_eq!(region_hint("Game"), "us");
    }

    #[test]
    fn layout_paths_follow_the_esde_tree() {
        let mut s = RomMediaSettings::default();
        s.media_root = "/media/root".into();
        assert_eq!(
            canonical_path(&s, "nes", "boxart", "Contra (USA)", "png"),
            PathBuf::from("/media/root/nes/covers/Contra (USA).png")
        );
        assert_eq!(
            canonical_path(&s, "gamecube", "trailer", "X", "mp4"),
            PathBuf::from("/media/root/gc/videos/X.mp4")
        );
        assert_eq!(
            canonical_path(&s, "superacan", "screenshot", "X", "png"),
            PathBuf::from("/media/root/supracan/screenshots/X.png")
        );
        // Override: the override dir IS the type folder.
        s.trailers.dir_override = Some("/bigdrive/trailers".into());
        assert_eq!(
            real_path(&s, "nes", "trailer", "Contra (USA)", "mp4"),
            PathBuf::from("/bigdrive/trailers/nes/Contra (USA).mp4")
        );
        // No override -> real == canonical.
        assert_eq!(
            real_path(&s, "nes", "boxart", "Contra (USA)", "png"),
            canonical_path(&s, "nes", "boxart", "Contra (USA)", "png")
        );
    }

    #[test]
    fn cascade_orders_are_the_memos() {
        assert_eq!(cascade_for("trailer"), vec!["emumovies", "screenscraper"]);
        assert_eq!(
            cascade_for("boxart"),
            vec!["libretro", "screenscraper", "thegamesdb", "steamgriddb"]
        );
        assert_eq!(
            cascade_for("screenshot"),
            vec!["libretro", "screenscraper", "thegamesdb"]
        );
    }

    #[test]
    fn ss_media_pick_prefers_type_then_region() {
        let medias = serde_json::json!([
            {"type": "box-2D", "region": "eu", "url": "https://x/eu.png"},
            {"type": "box-2D", "region": "us", "url": "https://x/us.png"},
            {"type": "ss", "region": "us", "url": "https://x/ss.png"},
            {"type": "video", "region": "wor", "url": "https://x/v.mp4"}
        ]);
        let arr = medias.as_array().unwrap();
        assert_eq!(
            ss_pick_media(arr, "boxart", "us").as_deref(),
            Some("https://x/us.png")
        );
        assert_eq!(
            ss_pick_media(arr, "boxart", "jp").as_deref(),
            Some("https://x/us.png"),
            "unknown region falls back through wor/us"
        );
        assert_eq!(
            ss_pick_media(arr, "screenshot", "us").as_deref(),
            Some("https://x/ss.png")
        );
        assert_eq!(
            ss_pick_media(arr, "trailer", "us").as_deref(),
            Some("https://x/v.mp4")
        );
        assert_eq!(ss_pick_media(arr, "titleshot", "us"), None);
    }

    #[test]
    fn pasv_replies_parse() {
        assert_eq!(
            parse_pasv("227 Entering Passive Mode (192,168,1,20,195,80)"),
            Some(("192.168.1.20".to_string(), 195 * 256 + 80))
        );
        assert_eq!(parse_pasv("227 no parens here"), None);
    }

    #[test]
    fn ftp_listings_parse_dirs_and_spaced_names() {
        let text = "drwxr-xr-x 2 ftp ftp 4096 Jan 01 2020 NES Video Snaps\r\n-rw-r--r-- 1 ftp ftp 1234 Jan 01 2020 Contra (USA).mp4\r\n";
        let entries = parse_list(text);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], ("NES Video Snaps".to_string(), true));
        assert_eq!(entries[1], ("Contra (USA).mp4".to_string(), false));
    }

    #[test]
    fn ftp_listings_parse_dos_style() {
        // The live file server (files.emumovies.com, IIS) answers
        // LIST in MS-DOS format — without this dialect the walk saw
        // an empty tree even with a working sign-in.
        let text = "10-29-25  12:13PM       <DIR>          Official\r\n04-11-26  02:58AM       <DIR>          Video Snaps (HQ)\r\n01-22-26  09:41PM            3805457 Contra (USA).mp4\r\n";
        let entries = parse_list(text);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0], ("Official".to_string(), true));
        assert_eq!(entries[1], ("Video Snaps (HQ)".to_string(), true));
        assert_eq!(entries[2], ("Contra (USA).mp4".to_string(), false));
    }

    #[test]
    fn reply_tail_quotes_the_servers_last_word() {
        assert_eq!(
            reply_tail("530 Login authentication failed\r\n"),
            "530 Login authentication failed"
        );
        assert_eq!(
            reply_tail("221-Goodbye.\r\n221 Logout.\r\n"),
            "221 Logout."
        );
        assert_eq!(reply_tail(""), "");
    }

    #[test]
    fn walk_visits_content_before_containers() {
        let nes: &[&str] = &["nes", "nintendo entertainment"];
        assert!(dir_rank("Official", nes) < dir_rank("Video Snaps (HQ)", nes));
        assert!(
            dir_rank("Nintendo Entertainment System (Video Snaps)(HQ)", nes)
                < dir_rank("Video Snaps (HQ)", nes)
        );
        assert!(
            dir_rank("Nintendo Entertainment System (Video Snaps)(HQ)", nes)
                < dir_rank("Artwork", nes)
        );
    }

    #[test]
    fn tier_scores_follow_the_appetite() {
        assert!(tier_score("Video Snaps (SQ)", "small") > tier_score("Video Snaps (HD)", "small"));
        assert!(tier_score("Video Snaps (HD)", "hd") > tier_score("Video Snaps (SQ)", "hd"));
        assert!(tier_score("HQ Videos", "standard") > tier_score("SQ Videos", "standard"));
    }

    #[test]
    fn settings_round_trip_and_defaults() {
        let dir = std::env::temp_dir().join(format!("fable-rommedia-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let s = load_settings(&dir);
        assert!(s.boxart.enabled);
        assert!(s.screenshots.enabled);
        assert!(s.titles.enabled);
        assert!(!s.trailers.enabled, "trailers default OFF: the expensive one");
        assert_eq!(s.trailer_tier, "small");
        assert!(s.media_root.ends_with("Fable Media"));
        let mut s2 = s.clone();
        s2.trailers.enabled = true;
        s2.trailer_tier = "hd".into();
        save_settings(&dir, &s2).unwrap();
        let back = load_settings(&dir);
        assert!(back.trailers.enabled);
        assert_eq!(back.trailer_tier, "hd");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_dataset_system_has_a_map_or_a_reason() {
        // All 60 keys parsed out of the shipped rom-dats.bin header
        // (2026-10-15). A system missing here has no rungs: the
        // planner must still name it in the report as uncovered.
        let keys = [
            "3do", "amiga", "amigacd32", "arcade", "arcadia2001", "atari2600",
            "atari5200", "atari7800", "atari8bit", "c64", "cdi", "channelf",
            "colecovision", "dreamcast", "gamecom", "gamecube", "gamegear",
            "gb", "gba", "gbc", "genesis", "gx4000", "intellivision", "jaguar",
            "lynx", "mastersystem", "msx", "n3ds", "n64", "nds", "neogeo",
            "neogeocd", "neogeopocket", "nes", "odyssey2", "pcfx", "ps2",
            "ps3", "psp", "psvita", "psx", "pv1000", "saturn", "scv",
            "sega32x", "segacd", "sg1000", "snes", "studio2", "superacan",
            "tg16", "vectrex", "vic20", "virtualboy", "wii", "wiiu",
            "wonderswan", "xbox", "xbox360", "zxspectrum",
        ];
        assert_eq!(keys.len(), 60);
        for k in keys {
            assert!(system_map(k).is_some(), "no system map for {k}");
        }
    }

    #[test]
    fn stems_and_exts() {
        assert_eq!(file_stem("Contra (USA).nes"), "Contra (USA)");
        assert_eq!(file_stem("archive.zip"), "archive");
        assert_eq!(file_stem("noext"), "noext");
        assert_eq!(ext_from("https://x/y/box.png?dl=1", "jpg"), "png");
        assert_eq!(ext_from("https://x/y/box", "jpg"), "jpg");
        assert_eq!(ext_from("https://x/y/box.JPEG", "jpg"), "jpeg");
    }

    /// LIVE SMOKE (manual, #[ignore]d): a handful of real Libretro
    /// fetches for two small systems. Run with:
    ///   cargo test -p fable-files rommedia -- --ignored live_libretro_smoke
    /// Proves the name transform + server layout end-to-end against
    /// the real thumbnail server. Not part of the suite (CI law).
    #[test]
    #[ignore]
    fn live_libretro_smoke() {
        let dir = std::env::temp_dir().join("fable-rommedia-smoke");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let agent = http_agent();
        let cases = [
            ("gb", "Tetris (World) (Rev 1)", "boxart"),
            ("gb", "Tetris (World) (Rev 1)", "screenshot"),
            ("lynx", "Chip's Challenge (USA, Europe)", "boxart"),
        ];
        for (sys, name, media_type) in cases {
            let spec = FetchSpec {
                item_id: "smoke".into(),
                system: sys.into(),
                dat_name: name.into(),
                rom_stem: name.into(),
                media_type: media_type.into(),
                crc: None,
                md5: None,
                sha1: None,
                size: None,
            };
            let part = dir.join("part.bin");
            match libretro_fetch(&agent, &spec, &part, &|| false) {
                FetchResult::Downloaded { ext, .. } => {
                    let bytes = std::fs::read(&part).unwrap();
                    assert_eq!(ext, "png");
                    assert!(
                        bytes.starts_with(&[0x89, b'P', b'N', b'G']),
                        "{name} {media_type}: not a PNG"
                    );
                    assert!(bytes.len() > 1000, "{name}: suspiciously small");
                }
                other => panic!("{name} {media_type}: expected a download, got {other:?}"),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
