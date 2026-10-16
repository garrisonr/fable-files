//! The systems table: mined ES-DE data + Fable's override layer.
//!
//! Per SPEC-AMENDMENT §2/§3: ES-DE's preference order picks the
//! default (the romm-tender pattern — first usable command wins);
//! Fable's overrides win wherever a product call differs — verdict
//! lines in plain language, install channels, honesty states. v1
//! curated NES + SNES; v1.1 (amendment §7) extends the layer to the
//! libretro core families — the handhelds, the Sega line, the
//! Beetle family, Flycast, FBNeo arcade, the Atari/SNK/odd-handheld
//! cores, PlayStation — with the emulator-wave systems (PS2-era
//! standalones and beyond) still riding mined data toward v2, and
//! the no-quality-path systems (PS4, Vita, Xbox 360, Game.com,
//! A'Can) marked NotYet so their shelves stay honest.

use crate::es::{vendored_systems, EsCommand, EsSystem};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OptionKind {
    LibretroCore,
    Standalone,
    EsDeDoor,
    OsDefault,
}

/// How an option gets onto the machine when it isn't there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "channel")]
pub enum InstallSpec {
    /// RetroArch (its own first step) + this option's core file from
    /// the libretro buildbot, into the user cores directory.
    RetroArchCore,
    /// `flatpak install --user <app_id>`.
    Flatpak { app_id: String },
    /// A deps.rs logical tool name (family package map lives there).
    AptLogical { logical: String },
    /// An official AppImage dropped into ~/Applications.
    AppImage { url: String, file_name: String },
    /// Fable does not install this (in this version). If the user
    /// already has it, Fable detects and uses it.
    None,
}

/// The honesty class of an option (amendment §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OptClass {
    /// May be a system's default; Fable installs it on request.
    DefaultAble,
    /// Works, with known caveats the verdict line states.
    BestEffort,
    /// Shown in the picker with its reason; never a default and
    /// never auto-installed (needs sidecar files, or its install
    /// channel belongs to a later round).
    ManualOnly,
    /// No quality path exists yet; the shelf stays honest.
    NotYet,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmuOption {
    pub id: String,
    /// Human label ("RetroArch · Mesen", "Mesen (standalone)").
    pub label: String,
    pub kind: OptionKind,
    /// ES-DE-style launch template (Fable-owned data; §12 non-goal:
    /// users never edit these).
    pub template: String,
    /// For libretro-core options: `mesen_libretro.so`.
    pub core_file: Option<String>,
    /// Display name RetroArch uses for the core ("Mesen") — rides
    /// into playlists.
    pub core_name: Option<String>,
    pub install: InstallSpec,
    /// BIOS/firmware files this option needs (empty for v1 systems).
    pub bios: Vec<String>,
    /// One plain line — where the truth about this option lives.
    pub verdict: String,
    pub class: OptClass,
    /// PATH binaries whose presence means "installed" for a
    /// standalone option.
    pub probe_bins: Vec<String>,
    /// Factual note shown when an option can't be installed by
    /// Fable (yet) — never a dead end for the system's default path.
    pub install_note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemEntry {
    /// ES-DE system id — also the unified-view folder name.
    pub id: String,
    pub fullname: String,
    /// The name the ask-once sheet uses ("Nintendo", "Super
    /// Nintendo" — amendment §6's phrasing).
    pub short_name: String,
    pub extensions: Vec<String>,
    pub options: Vec<EmuOption>,
    pub default_option: Option<String>,
    /// RetroArch playlist/database name (libretro vocabulary), when
    /// the keeper writes playlists for this system.
    pub playlist_name: Option<String>,
    /// True when the Fable override layer curated this system.
    pub overridden: bool,
}

impl SystemEntry {
    pub fn option(&self, id: &str) -> Option<&EmuOption> {
        self.options.iter().find(|o| o.id == id)
    }
    pub fn default(&self) -> Option<&EmuOption> {
        self.default_option
            .as_deref()
            .and_then(|id| self.option(id))
            .or_else(|| self.options.first())
    }
}

/// Slug for mined option ids: "Mesen (Standalone)" → "mesen-standalone".
fn slug(text: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true; // trims leading dashes too
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// The core file a template loads, when it loads one:
/// `%CORE_RETROARCH%/mesen_libretro.so` → `mesen_libretro.so`.
fn core_file_of(template: &str) -> Option<String> {
    let marker = "%CORE_RETROARCH%/";
    let start = template.find(marker)? + marker.len();
    let rest = &template[start..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
        .unwrap_or(rest.len());
    let file = &rest[..end];
    if file.ends_with(".so") {
        Some(file.to_string())
    } else {
        None
    }
}

/// Classify one mined command into an option (amendment §2's rules,
/// following the romm-tender pattern).
fn classify(system: &str, cmd: &EsCommand) -> EmuOption {
    let template = cmd.template.clone();
    let needs_inject = template.contains("%INJECT%");
    let core_file = core_file_of(&template);
    let (kind, install) = if core_file.is_some() {
        (OptionKind::LibretroCore, InstallSpec::RetroArchCore)
    } else {
        (OptionKind::Standalone, InstallSpec::None)
    };
    let class = if needs_inject {
        OptClass::ManualOnly
    } else if kind == OptionKind::LibretroCore {
        OptClass::DefaultAble
    } else {
        // A standalone ES-DE knows about but Fable has no install
        // channel for in the generic table.
        OptClass::ManualOnly
    };
    EmuOption {
        id: format!("{system}-{}", slug(&cmd.label)),
        label: cmd.label.clone(),
        kind,
        template,
        core_file,
        core_name: None,
        install,
        bios: Vec::new(),
        verdict: String::new(),
        class,
        probe_bins: Vec::new(),
        install_note: if needs_inject {
            Some(
                "This one needs files the emulator generates first, so Fable can't launch it directly."
                    .to_string(),
            )
        } else {
            None
        },
    }
}

fn mined_entry(sys: &EsSystem) -> SystemEntry {
    let options: Vec<EmuOption> = sys.commands.iter().map(|c| classify(&sys.name, c)).collect();
    // First usable (default-able) command in ES-DE's preference
    // order is the default — their curation does the picking.
    let default_option = options
        .iter()
        .find(|o| o.class == OptClass::DefaultAble)
        .map(|o| o.id.clone());
    SystemEntry {
        id: sys.name.clone(),
        fullname: if sys.fullname.is_empty() {
            sys.name.clone()
        } else {
            sys.fullname.clone()
        },
        short_name: if sys.fullname.is_empty() {
            sys.name.clone()
        } else {
            sys.fullname.clone()
        },
        extensions: sys.extensions.clone(),
        options,
        default_option,
        playlist_name: None,
        overridden: false,
    }
}

// ---------------------------------------------------------------------------
// The override layer (amendment §3). Curated rows for the systems v1
// ships end-to-end. Verdicts are written plain — one line, the truth.
// ---------------------------------------------------------------------------

fn ra_core(id: &str, label: &str, core_file: &str, core_name: &str, verdict: &str) -> EmuOption {
    EmuOption {
        id: id.to_string(),
        label: label.to_string(),
        kind: OptionKind::LibretroCore,
        template: format!("%EMULATOR_RETROARCH% -L %CORE_RETROARCH%/{core_file} %ROM%"),
        core_file: Some(core_file.to_string()),
        core_name: Some(core_name.to_string()),
        install: InstallSpec::RetroArchCore,
        bios: Vec::new(),
        verdict: verdict.to_string(),
        class: OptClass::DefaultAble,
        probe_bins: Vec::new(),
        install_note: None,
    }
}

fn standalone(
    id: &str,
    label: &str,
    template: &str,
    probe_bins: &[&str],
    verdict: &str,
) -> EmuOption {
    EmuOption {
        id: id.to_string(),
        label: label.to_string(),
        kind: OptionKind::Standalone,
        template: template.to_string(),
        core_file: None,
        core_name: None,
        install: InstallSpec::None,
        bios: Vec::new(),
        verdict: verdict.to_string(),
        class: OptClass::ManualOnly,
        probe_bins: probe_bins.iter().map(|s| s.to_string()).collect(),
        install_note: Some(
            "Fable doesn't install this one yet — if it's already on this PC, Fable will happily use it."
                .to_string(),
        ),
    }
}

fn override_nes(entry: &mut SystemEntry) {
    entry.short_name = "Nintendo".to_string();
    entry.playlist_name = Some("Nintendo - Nintendo Entertainment System".to_string());
    entry.options = vec![
        ra_core(
            "ra-mesen",
            "RetroArch · Mesen",
            "mesen_libretro.so",
            "Mesen",
            "The most accurate Nintendo emulator — plays almost everything, including oddball cartridges.",
        ),
        ra_core(
            "ra-nestopia",
            "RetroArch · Nestopia UE",
            "nestopia_libretro.so",
            "Nestopia UE",
            "A fast, faithful classic — a great second opinion for stubborn games.",
        ),
        ra_core(
            "ra-fceumm",
            "RetroArch · FCEUmm",
            "fceumm_libretro.so",
            "FCEUmm",
            "Light and forgiving — plays the weird bootlegs and mapper hacks others skip.",
        ),
        standalone(
            "mesen-standalone",
            "Mesen (standalone)",
            "%EMULATOR_MESEN% --fullscreen %ROM%",
            &["mesen", "Mesen"],
            "The same Mesen, as its own app with its own toolbox.",
        ),
    ];
    entry.default_option = Some("ra-mesen".to_string());
    entry.overridden = true;
}

fn override_snes(entry: &mut SystemEntry) {
    entry.short_name = "Super Nintendo".to_string();
    entry.playlist_name = Some("Nintendo - Super Nintendo Entertainment System".to_string());
    entry.options = vec![
        ra_core(
            "ra-snes9x",
            "RetroArch · Snes9x",
            "snes9x_libretro.so",
            "Snes9x",
            "Plays almost everything, fast and light — the community favorite.",
        ),
        ra_core(
            "ra-bsnes",
            "RetroArch · bsnes",
            "bsnes_libretro.so",
            "bsnes",
            "Cycle-accurate — the most faithful Super Nintendo, and a little hungrier for it.",
        ),
        standalone(
            "snes9x-standalone",
            "Snes9x (standalone)",
            "%EMULATOR_SNES9X% %ROM%",
            &["snes9x", "snes9x-gtk"],
            "Snes9x as its own app.",
        ),
    ];
    entry.default_option = Some("ra-snes9x".to_string());
    entry.overridden = true;
}

// ---------------------------------------------------------------------------
// v1.1 override helpers.
// ---------------------------------------------------------------------------

/// A libretro-core option whose display BIOS list comes from the
/// system's requirement in `bios.rs` (single source of truth for
/// file names — the sheet shows them, the card manages them).
fn ra_core_req(id: &str, label: &str, core_file: &str, core_name: &str, verdict: &str, system: &str) -> EmuOption {
    let mut opt = ra_core(id, label, core_file, core_name, verdict);
    if let Some(req) = crate::bios::requirement_for(system) {
        opt.bios = req.all_names();
    }
    opt
}

fn classed(mut opt: EmuOption, class: OptClass) -> EmuOption {
    opt.class = class;
    opt
}

fn curate(
    entry: &mut SystemEntry,
    short_name: &str,
    playlist_name: &str,
    options: Vec<EmuOption>,
    default_id: &str,
) {
    entry.short_name = short_name.to_string();
    entry.playlist_name = Some(playlist_name.to_string());
    entry.options = options;
    entry.default_option = Some(default_id.to_string());
    entry.overridden = true;
}

fn mgba_standalone() -> EmuOption {
    standalone(
        "mgba-standalone",
        "mGBA (standalone)",
        "%EMULATOR_MGBA% -f %ROM%",
        &["mgba"],
        "mGBA as its own app — Game Boy, Color and Advance in one.",
    )
}

fn override_gb(entry: &mut SystemEntry) {
    curate(
        entry,
        "Game Boy",
        "Nintendo - Game Boy",
        vec![
            ra_core(
                "ra-gambatte",
                "RetroArch · Gambatte",
                "gambatte_libretro.so",
                "Gambatte",
                "The accuracy favorite — colors, sound and timing the way the hardware did them.",
            ),
            ra_core(
                "ra-mgba",
                "RetroArch · mGBA",
                "mgba_libretro.so",
                "mGBA",
                "Fast and excellent on Game Boy too — the Advance pick moonlighting here.",
            ),
            mgba_standalone(),
            standalone(
                "sameboy-standalone",
                "SameBoy (standalone)",
                "%EMULATOR_SAMEBOY% -f %ROM%",
                &["sameboy"],
                "The perfectionist's Game Boy — beloved for its link-cable and printer tricks.",
            ),
        ],
        "ra-gambatte",
    );
}

fn override_gbc(entry: &mut SystemEntry) {
    curate(
        entry,
        "Game Boy Color",
        "Nintendo - Game Boy Color",
        vec![
            ra_core(
                "ra-gambatte",
                "RetroArch · Gambatte",
                "gambatte_libretro.so",
                "Gambatte",
                "The accuracy favorite — colors, sound and timing the way the hardware did them.",
            ),
            ra_core(
                "ra-mgba",
                "RetroArch · mGBA",
                "mgba_libretro.so",
                "mGBA",
                "Fast and excellent on Game Boy Color too — the Advance pick moonlighting here.",
            ),
            mgba_standalone(),
            standalone(
                "sameboy-standalone",
                "SameBoy (standalone)",
                "%EMULATOR_SAMEBOY% -f %ROM%",
                &["sameboy"],
                "The perfectionist's Game Boy Color — beloved for its link-cable and printer tricks.",
            ),
        ],
        "ra-gambatte",
    );
}

fn override_gba(entry: &mut SystemEntry) {
    curate(
        entry,
        "Game Boy Advance",
        "Nintendo - Game Boy Advance",
        vec![
            ra_core_req(
                "ra-mgba",
                "RetroArch · mGBA",
                "mgba_libretro.so",
                "mGBA",
                "The Game Boy Advance standard — fast, accurate, and kind to this PC.",
                "gba",
            ),
            mgba_standalone(),
        ],
        "ra-mgba",
    );
}

fn override_n64(entry: &mut SystemEntry) {
    curate(
        entry,
        "Nintendo 64",
        "Nintendo - Nintendo 64",
        vec![
            ra_core(
                "ra-mupen64plus-next",
                "RetroArch · Mupen64Plus-Next",
                "mupen64plus_next_libretro.so",
                "Mupen64Plus-Next",
                "Most of the Nintendo 64 library plays well at its original resolution — the pragmatic pick for this PC.",
            ),
            ra_core(
                "ra-parallel-n64",
                "RetroArch · ParaLLEl N64",
                "parallel_n64_libretro.so",
                "ParaLLEl N64",
                "The accuracy angle — leans on the graphics hardware; hungrier and fussier, beautiful when it sings.",
            ),
            standalone(
                "rmg-standalone",
                "RMG (standalone)",
                "%EMULATOR_RMG% %ROM%",
                &["RMG"],
                "Rosalie's Mupen GUI — a friendly Nintendo 64 app built on the same engine.",
            ),
        ],
        "ra-mupen64plus-next",
    );
}

fn override_nds(entry: &mut SystemEntry) {
    curate(
        entry,
        "Nintendo DS",
        "Nintendo - Nintendo DS",
        vec![
            ra_core_req(
                "ra-melonds",
                "RetroArch · melonDS",
                "melonds_libretro.so",
                "melonDS",
                "The DS done right — touch, microphone and the wireless tricks included.",
                "nds",
            ),
            ra_core(
                "ra-desmume",
                "RetroArch · DeSmuME",
                "desmume_libretro.so",
                "DeSmuME",
                "The older hand — fine for many games, slower with the fancy 3D ones.",
            ),
            standalone(
                "melonds-standalone",
                "melonDS (standalone)",
                "%EMULATOR_MELONDS% -f %ROM%",
                &["melonds", "melonDS"],
                "melonDS as its own app.",
            ),
        ],
        "ra-melonds",
    );
}

fn gpgx(verdict: &str) -> EmuOption {
    ra_core(
        "ra-genesisplusgx",
        "RetroArch · Genesis Plus GX",
        "genesis_plus_gx_libretro.so",
        "Genesis Plus GX",
        verdict,
    )
}

fn picodrive(verdict: &str) -> EmuOption {
    ra_core(
        "ra-picodrive",
        "RetroArch · PicoDrive",
        "picodrive_libretro.so",
        "PicoDrive",
        verdict,
    )
}

fn override_genesis(entry: &mut SystemEntry) {
    curate(
        entry,
        "Genesis",
        "Sega - Mega Drive - Genesis",
        vec![
            gpgx("The reference Sega core — Genesis done properly, by the people who know it best."),
            picodrive("The 32X specialist, and a touch lighter on the biggest Genesis games."),
        ],
        "ra-genesisplusgx",
    );
}

fn override_mastersystem(entry: &mut SystemEntry) {
    curate(
        entry,
        "Master System",
        "Sega - Master System - Mark III",
        vec![gpgx(
            "Genesis Plus GX covers the Master System completely — one core for the whole Sega family.",
        )],
        "ra-genesisplusgx",
    );
}

fn override_gamegear(entry: &mut SystemEntry) {
    curate(
        entry,
        "Game Gear",
        "Sega - Game Gear",
        vec![gpgx(
            "Genesis Plus GX handles the Game Gear — the full library, no fuss.",
        )],
        "ra-genesisplusgx",
    );
}

fn override_sg1000(entry: &mut SystemEntry) {
    curate(
        entry,
        "SG-1000",
        "Sega - SG-1000",
        vec![gpgx(
            "Genesis Plus GX reaches all the way back to Sega's first console.",
        )],
        "ra-genesisplusgx",
    );
}

fn override_segacd(entry: &mut SystemEntry) {
    curate(
        entry,
        "Sega CD",
        "Sega - Mega-CD - Sega CD",
        vec![
            ra_core_req(
                "ra-genesisplusgx",
                "RetroArch · Genesis Plus GX",
                "genesis_plus_gx_libretro.so",
                "Genesis Plus GX",
                "Genesis Plus GX plays Sega CD — accurate and light. It needs the console's own BIOS file first.",
                "segacd",
            ),
            ra_core_req(
                "ra-picodrive",
                "RetroArch · PicoDrive",
                "picodrive_libretro.so",
                "PicoDrive",
                "The lighter Sega CD hand — a good second opinion. It needs the BIOS file too.",
                "segacd",
            ),
        ],
        "ra-genesisplusgx",
    );
}

fn override_sega32x(entry: &mut SystemEntry) {
    curate(
        entry,
        "Sega 32X",
        "Sega - 32X",
        vec![picodrive(
            "The core that speaks 32X — PicoDrive carries the whole add-on library.",
        )],
        "ra-picodrive",
    );
}

fn override_pcengine(entry: &mut SystemEntry) {
    curate(
        entry,
        "TurboGrafx-16",
        "NEC - PC Engine - TurboGrafx 16",
        vec![ra_core(
            "ra-beetle-pce",
            "RetroArch · Beetle PCE",
            "mednafen_pce_libretro.so",
            "Beetle PCE",
            "Mednafen's PC Engine — HuCards perfectly, and the CD library too once its System Card is in place.",
        )],
        "ra-beetle-pce",
    );
}

fn override_pcenginecd(entry: &mut SystemEntry) {
    curate(
        entry,
        "TurboGrafx-CD",
        "NEC - PC Engine CD - TurboGrafx-CD",
        vec![ra_core_req(
            "ra-beetle-pce",
            "RetroArch · Beetle PCE",
            "mednafen_pce_libretro.so",
            "Beetle PCE",
            "Mednafen's PC Engine — the CD library's home. It needs the System Card file to start.",
            "pcenginecd",
        )],
        "ra-beetle-pce",
    );
}

fn override_saturn(entry: &mut SystemEntry) {
    curate(
        entry,
        "Saturn",
        "Sega - Saturn",
        vec![
            ra_core_req(
                "ra-beetle-saturn",
                "RetroArch · Beetle Saturn",
                "mednafen_saturn_libretro.so",
                "Beetle Saturn",
                "The accurate Saturn. The 2D library plays beautifully here; heavy 3D will test this PC.",
                "saturn",
            ),
            ra_core_req(
                "ra-kronos",
                "RetroArch · Kronos",
                "kronos_libretro.so",
                "Kronos",
                "Lighter and faster, a little rougher around the edges — kind to modest hardware.",
                "saturn",
            ),
        ],
        "ra-beetle-saturn",
    );
}

fn override_dreamcast(entry: &mut SystemEntry) {
    curate(
        entry,
        "Dreamcast",
        "Sega - Dreamcast",
        vec![
            ra_core(
                "ra-flycast",
                "RetroArch · Flycast",
                "flycast_libretro.so",
                "Flycast",
                "Mature and quick — most of the Dreamcast library plays well.",
            ),
            standalone(
                "flycast-standalone",
                "Flycast (standalone)",
                "%EMULATOR_FLYCAST% %ROM%",
                &["flycast"],
                "Flycast as its own app.",
            ),
        ],
        "ra-flycast",
    );
}

fn arcade_options(system: &str) -> Vec<EmuOption> {
    vec![
        ra_core_req(
            "ra-fbneo",
            "RetroArch · FinalBurn Neo",
            "fbneo_libretro.so",
            "FinalBurn Neo",
            "The arcade crowd-pleaser — Neo Geo, Capcom, Cave and thousands more, and light on this PC.",
            system,
        ),
        ra_core_req(
            "ra-mame",
            "RetroArch · MAME",
            "mame_libretro.so",
            "MAME",
            "MAME itself as a core — the deep bench for sets FinalBurn doesn't carry.",
            system,
        ),
        standalone(
            "mame-standalone",
            "MAME (standalone)",
            "mame -rompath %GAMEDIR% %BASENAME%",
            &["mame"],
            "MAME as its own app — Fable points it at the set's own folder.",
        ),
    ]
}

fn override_arcade(entry: &mut SystemEntry) {
    curate(entry, "Arcade", "FBNeo - Arcade Games", arcade_options("arcade"), "ra-fbneo");
}

fn override_neogeo(entry: &mut SystemEntry) {
    curate(entry, "Neo Geo", "SNK - Neo Geo", arcade_options("neogeo"), "ra-fbneo");
}

fn override_ngpc(entry: &mut SystemEntry) {
    curate(
        entry,
        "Neo Geo Pocket Color",
        "SNK - Neo Geo Pocket Color",
        vec![
            ra_core(
                "ra-beetle-ngp",
                "RetroArch · Beetle NeoPop",
                "mednafen_ngp_libretro.so",
                "Beetle NeoPop",
                "Small, exact and complete — the Pocket Color done right.",
            ),
            ra_core(
                "ra-race",
                "RetroArch · RACE",
                "race_libretro.so",
                "RACE",
                "The lighter hand — quick and cheerful for the whole library.",
            ),
        ],
        "ra-beetle-ngp",
    );
}

fn override_atari2600(entry: &mut SystemEntry) {
    curate(
        entry,
        "Atari 2600",
        "Atari - 2600",
        vec![ra_core(
            "ra-stella",
            "RetroArch · Stella",
            "stella_libretro.so",
            "Stella",
            "The Atari 2600, perfected over decades.",
        )],
        "ra-stella",
    );
}

fn override_atari7800(entry: &mut SystemEntry) {
    curate(
        entry,
        "Atari 7800",
        "Atari - 7800",
        vec![ra_core(
            "ra-prosystem",
            "RetroArch · ProSystem",
            "prosystem_libretro.so",
            "ProSystem",
            "The 7800 specialist — the whole library, backwards-compatible tricks included.",
        )],
        "ra-prosystem",
    );
}

fn override_atari5200(entry: &mut SystemEntry) {
    curate(
        entry,
        "Atari 5200",
        "Atari - 5200",
        vec![
            ra_core_req(
                "ra-a5200",
                "RetroArch · a5200",
                "a5200_libretro.so",
                "a5200",
                "Purpose-built for the 5200 — it needs the console's own BIOS file first.",
                "atari5200",
            ),
            ra_core_req(
                "ra-atari800",
                "RetroArch · Atari800",
                "atari800_libretro.so",
                "Atari800",
                "The Atari 8-bit computer core moonlighting on 5200 duty — it wants the BIOS too.",
                "atari5200",
            ),
        ],
        "ra-a5200",
    );
}

fn override_atarilynx(entry: &mut SystemEntry) {
    curate(
        entry,
        "Lynx",
        "Atari - Lynx",
        vec![
            ra_core_req(
                "ra-beetle-lynx",
                "RetroArch · Beetle Lynx",
                "mednafen_lynx_libretro.so",
                "Beetle Lynx",
                "The accurate Lynx — it needs the console's boot file first.",
                "atarilynx",
            ),
            ra_core_req(
                "ra-handy",
                "RetroArch · Handy",
                "handy_libretro.so",
                "Handy",
                "The original Lynx emulator — same boot file, a lighter touch.",
                "atarilynx",
            ),
        ],
        "ra-beetle-lynx",
    );
}

fn override_colecovision(entry: &mut SystemEntry) {
    curate(
        entry,
        "ColecoVision",
        "Coleco - ColecoVision",
        vec![
            ra_core_req(
                "ra-gearcoleco",
                "RetroArch · Gearcoleco",
                "gearcoleco_libretro.so",
                "Gearcoleco",
                "Purpose-built for ColecoVision — it needs the console's BIOS file first.",
                "colecovision",
            ),
            ra_core_req(
                "ra-bluemsx",
                "RetroArch · blueMSX",
                "bluemsx_libretro.so",
                "blueMSX",
                "The MSX machine wearing a Coleco hat — it also needs its support files, so Gearcoleco is the one to use.",
                "colecovision",
            ),
        ],
        "ra-gearcoleco",
    );
}

fn override_intellivision(entry: &mut SystemEntry) {
    curate(
        entry,
        "Intellivision",
        "Mattel - Intellivision",
        vec![
            ra_core(
                "ra-freeintv",
                "RetroArch · FreeIntv",
                "freeintv_libretro.so",
                "FreeIntv",
                "The whole Intellivision library — no system files needed.",
            ),
            standalone(
                "jzintv-standalone",
                "jzIntv (standalone)",
                "%EMULATOR_JZINTV% %ROM%",
                &["jzintv"],
                "jzIntv as its own app.",
            ),
        ],
        "ra-freeintv",
    );
}

fn override_odyssey2(entry: &mut SystemEntry) {
    curate(
        entry,
        "Odyssey²",
        "Magnavox - Odyssey2",
        vec![ra_core_req(
            "ra-o2em",
            "RetroArch · O2EM",
            "o2em_libretro.so",
            "O2EM",
            "The Odyssey² specialist — it needs the console's system ROM first.",
            "odyssey2",
        )],
        "ra-o2em",
    );
}

fn override_vectrex(entry: &mut SystemEntry) {
    curate(
        entry,
        "Vectrex",
        "GCE - Vectrex",
        vec![ra_core(
            "ra-vecx",
            "RetroArch · vecx",
            "vecx_libretro.so",
            "vecx",
            "Crisp vectors, overlays and all.",
        )],
        "ra-vecx",
    );
}

fn override_virtualboy(entry: &mut SystemEntry) {
    curate(
        entry,
        "Virtual Boy",
        "Nintendo - Virtual Boy",
        vec![ra_core(
            "ra-beetle-vb",
            "RetroArch · Beetle VB",
            "mednafen_vb_libretro.so",
            "Beetle VB",
            "The Virtual Boy in glorious red — easier on the eyes than the original ever was.",
        )],
        "ra-beetle-vb",
    );
}

fn wswan_option(verdict: &str) -> EmuOption {
    ra_core(
        "ra-beetle-wswan",
        "RetroArch · Beetle Cygne",
        "mednafen_wswan_libretro.so",
        "Beetle Cygne",
        verdict,
    )
}

fn override_wonderswan(entry: &mut SystemEntry) {
    curate(
        entry,
        "WonderSwan",
        "Bandai - WonderSwan",
        vec![wswan_option(
            "Mednafen's WonderSwan — the whole library, both orientations.",
        )],
        "ra-beetle-wswan",
    );
}

fn override_wonderswancolor(entry: &mut SystemEntry) {
    curate(
        entry,
        "WonderSwan Color",
        "Bandai - WonderSwan Color",
        vec![wswan_option(
            "The color library through Mednafen's WonderSwan core.",
        )],
        "ra-beetle-wswan",
    );
}

fn override_3do(entry: &mut SystemEntry) {
    curate(
        entry,
        "3DO",
        "The 3DO Company - 3DO",
        vec![ra_core_req(
            "ra-opera",
            "RetroArch · Opera",
            "opera_libretro.so",
            "Opera",
            "The 3DO's second life — it needs the console's system ROM first.",
            "3do",
        )],
        "ra-opera",
    );
}

fn override_pcfx(entry: &mut SystemEntry) {
    curate(
        entry,
        "PC-FX",
        "NEC - PC-FX",
        vec![ra_core_req(
            "ra-beetle-pcfx",
            "RetroArch · Beetle PC-FX",
            "mednafen_pcfx_libretro.so",
            "Beetle PC-FX",
            "Mednafen is the PC-FX — it needs the console's BIOS file first.",
            "pcfx",
        )],
        "ra-beetle-pcfx",
    );
}

fn override_neogeocd(entry: &mut SystemEntry) {
    curate(
        entry,
        "Neo Geo CD",
        "SNK - Neo Geo CD",
        vec![ra_core_req(
            "ra-neocd",
            "RetroArch · NeoCD",
            "neocd_libretro.so",
            "NeoCD",
            "Purpose-built for the Neo Geo CD — it needs the console's BIOS file first.",
            "neogeocd",
        )],
        "ra-neocd",
    );
}

fn override_msx(entry: &mut SystemEntry) {
    curate(
        entry,
        "MSX",
        "Microsoft - MSX",
        vec![
            // blueMSX stays the default (amendment §3) — installable
            // and the community standard; its support-files caveat
            // lives in the verdict line, in the open.
            ra_core(
                "ra-bluemsx",
                "RetroArch · blueMSX",
                "bluemsx_libretro.so",
                "blueMSX",
                "The MSX standard — it needs its machine support files in RetroArch's system folder, which Fable can't fetch yet.",
            ),
            ra_core(
                "ra-fmsx",
                "RetroArch · fMSX",
                "fmsx_libretro.so",
                "fMSX",
                "The lighter MSX — self-contained and cheerful.",
            ),
        ],
        "ra-bluemsx",
    );
}

fn override_c64(entry: &mut SystemEntry) {
    curate(
        entry,
        "Commodore 64",
        "Commodore - 64",
        vec![
            ra_core(
                "ra-vice-x64",
                "RetroArch · VICE x64",
                "vice_x64_libretro.so",
                "VICE x64",
                "VICE, the Commodore institution — the fast x64 build.",
            ),
            ra_core(
                "ra-vice-x64sc",
                "RetroArch · VICE x64sc",
                "vice_x64sc_libretro.so",
                "VICE x64sc",
                "The cycle-exact build — a step slower, a hair truer.",
            ),
        ],
        "ra-vice-x64",
    );
}

fn override_amigacd32(entry: &mut SystemEntry) {
    curate(
        entry,
        "Amiga CD32",
        "Commodore - CD32",
        vec![
            ra_core_req(
                "ra-puae",
                "RetroArch · PUAE",
                "puae_libretro.so",
                "PUAE",
                "The Amiga in a core — it needs a CD32 Kickstart file first.",
                "amigacd32",
            ),
            standalone(
                "fs-uae-standalone",
                "FS-UAE (standalone)",
                "%EMULATOR_FS-UAE% --amiga-model=CD32 %ROM%",
                &["fs-uae"],
                "FS-UAE as its own app.",
            ),
        ],
        "ra-puae",
    );
}

fn override_channelf(entry: &mut SystemEntry) {
    curate(
        entry,
        "Channel F",
        "Fairchild - Channel F",
        vec![ra_core_req(
            "ra-freechaf",
            "RetroArch · FreeChaF",
            "freechaf_libretro.so",
            "FreeChaF",
            "The Channel F lives again — it needs the console's three system files first.",
            "channelf",
        )],
        "ra-freechaf",
    );
}

fn override_atarijaguar(entry: &mut SystemEntry) {
    curate(
        entry,
        "Jaguar",
        "Atari - Jaguar",
        vec![classed(
            ra_core(
                "ra-virtualjaguar",
                "RetroArch · Virtual Jaguar",
                "virtualjaguar_libretro.so",
                "Virtual Jaguar",
                "Best effort — Jaguar emulation is still rough everywhere; some games crawl, some refuse. It's what's on offer.",
            ),
            OptClass::BestEffort,
        )],
        "ra-virtualjaguar",
    );
}

fn override_psx(entry: &mut SystemEntry) {
    curate(
        entry,
        "PlayStation",
        "Sony - PlayStation",
        vec![
            ra_core_req(
                "ra-beetle-psx-hw",
                "RetroArch · Beetle PSX HW",
                "mednafen_psx_hw_libretro.so",
                "Beetle PSX HW",
                "The strong PlayStation core — sharp 3D and deep compatibility. (The DuckStation app joins in a later round.)",
                "psx",
            ),
            ra_core_req(
                "ra-beetle-psx",
                "RetroArch · Beetle PSX",
                "mednafen_psx_libretro.so",
                "Beetle PSX",
                "The software-only Beetle — the patient, exact one.",
                "psx",
            ),
            standalone(
                "duckstation-standalone",
                "DuckStation (standalone)",
                "%EMULATOR_DUCKSTATION% %ROM%",
                &["duckstation", "DuckStation"],
                "The community's favorite PlayStation app — Fable installs it in the standalone round; if it's already here, Fable can drive it.",
            ),
        ],
        "ra-beetle-psx-hw",
    );
}

/// The amendment's designed honesty state for systems with no
/// quality path in this round: every mined option becomes NotYet —
/// the shelf stays truthful instead of implying a default exists.
fn mark_not_yet(entry: &mut SystemEntry) {
    for opt in &mut entry.options {
        opt.class = OptClass::NotYet;
    }
    entry.default_option = None;
}

fn build_table() -> Vec<SystemEntry> {
    let mut out: Vec<SystemEntry> = vendored_systems().iter().map(mined_entry).collect();
    for entry in &mut out {
        match entry.id.as_str() {
            "nes" => override_nes(entry),
            "snes" => override_snes(entry),
            // v1.1 core families (amendment §7).
            "gb" => override_gb(entry),
            "gbc" => override_gbc(entry),
            "gba" => override_gba(entry),
            "n64" => override_n64(entry),
            "nds" => override_nds(entry),
            "genesis" => override_genesis(entry),
            "mastersystem" => override_mastersystem(entry),
            "gamegear" => override_gamegear(entry),
            "sg-1000" => override_sg1000(entry),
            "segacd" => override_segacd(entry),
            "sega32x" => override_sega32x(entry),
            "pcengine" => override_pcengine(entry),
            // ES-DE does carry a "tg16" system (the catalog's word
            // for the same machine) — it gets the pcengine curation.
            "tg16" => override_pcengine(entry),
            "pcenginecd" => override_pcenginecd(entry),
            "saturn" => override_saturn(entry),
            "dreamcast" => override_dreamcast(entry),
            "arcade" => override_arcade(entry),
            "neogeo" => override_neogeo(entry),
            "ngpc" => override_ngpc(entry),
            "atari2600" => override_atari2600(entry),
            "atari7800" => override_atari7800(entry),
            "atari5200" => override_atari5200(entry),
            "atarilynx" => override_atarilynx(entry),
            "colecovision" => override_colecovision(entry),
            "intellivision" => override_intellivision(entry),
            "odyssey2" => override_odyssey2(entry),
            "vectrex" => override_vectrex(entry),
            "virtualboy" => override_virtualboy(entry),
            "wonderswan" => override_wonderswan(entry),
            "wonderswancolor" => override_wonderswancolor(entry),
            "3do" => override_3do(entry),
            "pcfx" => override_pcfx(entry),
            "neogeocd" => override_neogeocd(entry),
            "msx" => override_msx(entry),
            "c64" => override_c64(entry),
            "amigacd32" => override_amigacd32(entry),
            "channelf" => override_channelf(entry),
            "atarijaguar" => override_atarijaguar(entry),
            "psx" => override_psx(entry),
            // Designed not-yet states (amendment §3).
            "ps4" | "psvita" | "xbox360" | "supracan" | "gamecom" => mark_not_yet(entry),
            _ => {}
        }
    }
    out
}

/// The full systems table (mined + overrides), built once.
pub fn systems_table() -> &'static [SystemEntry] {
    static CACHE: std::sync::OnceLock<Vec<SystemEntry>> = std::sync::OnceLock::new();
    CACHE.get_or_init(build_table)
}

pub fn system_by_id(id: &str) -> Option<&'static SystemEntry> {
    systems_table().iter().find(|s| s.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_carry_the_amendments_defaults() {
        let nes = system_by_id("nes").expect("nes in table");
        assert!(nes.overridden);
        assert_eq!(nes.default_option.as_deref(), Some("ra-mesen"));
        let def = nes.default().unwrap();
        assert_eq!(def.core_file.as_deref(), Some("mesen_libretro.so"));
        assert_eq!(def.install, InstallSpec::RetroArchCore);
        assert_eq!(nes.short_name, "Nintendo");
        assert_eq!(
            nes.playlist_name.as_deref(),
            Some("Nintendo - Nintendo Entertainment System")
        );
        // The picker's rows are exactly the amendment §3 rows.
        let ids: Vec<&str> = nes.options.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["ra-mesen", "ra-nestopia", "ra-fceumm", "mesen-standalone"]
        );

        let snes = system_by_id("snes").expect("snes in table");
        assert_eq!(snes.default_option.as_deref(), Some("ra-snes9x"));
        assert_eq!(
            snes.default().unwrap().core_file.as_deref(),
            Some("snes9x_libretro.so")
        );
        let ids: Vec<&str> = snes.options.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(ids, vec!["ra-snes9x", "ra-bsnes", "snes9x-standalone"]);
    }

    #[test]
    fn mined_systems_keep_esde_preference_order() {
        // A system with no override (megadrive is genesis' ES-DE
        // twin and stays mined): the default is ES-DE's first usable
        // command, and %INJECT% templates classify manual-only.
        let md = system_by_id("megadrive").expect("megadrive mined");
        assert!(!md.overridden);
        assert!(md.default_option.is_some());
        let def = md.default().unwrap();
        assert_eq!(def.class, OptClass::DefaultAble);
        assert_eq!(def.kind, OptionKind::LibretroCore);
    }

    #[test]
    fn v11_overrides_carry_the_amendments_defaults() {
        // (system, default option id, default core file)
        let cases: &[(&str, &str, &str)] = &[
            ("gb", "ra-gambatte", "gambatte_libretro.so"),
            ("gbc", "ra-gambatte", "gambatte_libretro.so"),
            ("gba", "ra-mgba", "mgba_libretro.so"),
            ("n64", "ra-mupen64plus-next", "mupen64plus_next_libretro.so"),
            ("nds", "ra-melonds", "melonds_libretro.so"),
            ("genesis", "ra-genesisplusgx", "genesis_plus_gx_libretro.so"),
            ("mastersystem", "ra-genesisplusgx", "genesis_plus_gx_libretro.so"),
            ("gamegear", "ra-genesisplusgx", "genesis_plus_gx_libretro.so"),
            ("sg-1000", "ra-genesisplusgx", "genesis_plus_gx_libretro.so"),
            ("segacd", "ra-genesisplusgx", "genesis_plus_gx_libretro.so"),
            ("sega32x", "ra-picodrive", "picodrive_libretro.so"),
            ("pcengine", "ra-beetle-pce", "mednafen_pce_libretro.so"),
            ("pcenginecd", "ra-beetle-pce", "mednafen_pce_libretro.so"),
            ("saturn", "ra-beetle-saturn", "mednafen_saturn_libretro.so"),
            ("dreamcast", "ra-flycast", "flycast_libretro.so"),
            ("arcade", "ra-fbneo", "fbneo_libretro.so"),
            ("neogeo", "ra-fbneo", "fbneo_libretro.so"),
            ("ngpc", "ra-beetle-ngp", "mednafen_ngp_libretro.so"),
            ("atari2600", "ra-stella", "stella_libretro.so"),
            ("atari7800", "ra-prosystem", "prosystem_libretro.so"),
            ("atari5200", "ra-a5200", "a5200_libretro.so"),
            ("atarilynx", "ra-beetle-lynx", "mednafen_lynx_libretro.so"),
            ("colecovision", "ra-gearcoleco", "gearcoleco_libretro.so"),
            ("intellivision", "ra-freeintv", "freeintv_libretro.so"),
            ("odyssey2", "ra-o2em", "o2em_libretro.so"),
            ("vectrex", "ra-vecx", "vecx_libretro.so"),
            ("virtualboy", "ra-beetle-vb", "mednafen_vb_libretro.so"),
            ("wonderswan", "ra-beetle-wswan", "mednafen_wswan_libretro.so"),
            ("wonderswancolor", "ra-beetle-wswan", "mednafen_wswan_libretro.so"),
            ("3do", "ra-opera", "opera_libretro.so"),
            ("pcfx", "ra-beetle-pcfx", "mednafen_pcfx_libretro.so"),
            ("neogeocd", "ra-neocd", "neocd_libretro.so"),
            ("msx", "ra-bluemsx", "bluemsx_libretro.so"),
            ("c64", "ra-vice-x64", "vice_x64_libretro.so"),
            ("amigacd32", "ra-puae", "puae_libretro.so"),
            ("channelf", "ra-freechaf", "freechaf_libretro.so"),
            ("atarijaguar", "ra-virtualjaguar", "virtualjaguar_libretro.so"),
            ("psx", "ra-beetle-psx-hw", "mednafen_psx_hw_libretro.so"),
        ];
        for (system, want_id, want_core) in cases {
            let sys = system_by_id(system).unwrap_or_else(|| panic!("{system} in table"));
            assert!(sys.overridden, "{system} overridden");
            assert_eq!(
                sys.default_option.as_deref(),
                Some(*want_id),
                "{system} default option"
            );
            let def = sys.default().unwrap();
            assert_eq!(def.core_file.as_deref(), Some(*want_core), "{system} core file");
            assert_eq!(def.install, InstallSpec::RetroArchCore, "{system} install channel");
            assert!(!def.verdict.is_empty(), "{system} default carries a verdict");
            assert!(sys.playlist_name.is_some(), "{system} playlist name");
            assert!(!sys.short_name.is_empty(), "{system} short name");
        }
        // Jaguar is the amendment's best-effort row.
        let jag = system_by_id("atarijaguar").unwrap();
        assert_eq!(jag.default().unwrap().class, OptClass::BestEffort);
        // PSX: DuckStation rides along manual-only in v1.1 (v2 installs it).
        let psx = system_by_id("psx").unwrap();
        let duck = psx.option("duckstation-standalone").unwrap();
        assert_eq!(duck.class, OptClass::ManualOnly);
        assert_eq!(duck.install, InstallSpec::None);
    }

    #[test]
    fn v11_bios_display_lists_come_from_the_requirement_table() {
        let psx = system_by_id("psx").unwrap();
        let def = psx.default().unwrap();
        assert!(def.bios.iter().any(|b| b == "scph5501.bin"));
        let channelf = system_by_id("channelf").unwrap();
        assert_eq!(channelf.default().unwrap().bios.len(), 3);
        let gba = system_by_id("gba").unwrap();
        assert_eq!(gba.default().unwrap().bios, vec!["gba_bios.bin".to_string()]);
        // Core options of gated systems all carry the list.
        let saturn = system_by_id("saturn").unwrap();
        assert!(saturn
            .options
            .iter()
            .filter(|o| o.kind == OptionKind::LibretroCore)
            .all(|o| !o.bios.is_empty()));
    }

    #[test]
    fn tg16_gets_the_pcengine_curation() {
        let tg16 = system_by_id("tg16").expect("tg16 alias exists");
        assert!(tg16.overridden);
        assert_eq!(tg16.default_option.as_deref(), Some("ra-beetle-pce"));
        assert_eq!(
            tg16.default().unwrap().core_file.as_deref(),
            Some("mednafen_pce_libretro.so")
        );
    }

    #[test]
    fn designed_not_yet_systems_offer_no_default() {
        for id in ["ps4", "psvita", "xbox360"] {
            let sys = system_by_id(id).unwrap_or_else(|| panic!("{id} in table"));
            assert!(
                sys.options.iter().all(|o| o.class == OptClass::NotYet),
                "{id} options all not-yet"
            );
            assert!(sys.default_option.is_none(), "{id} has no default");
        }
        // Xbox (xemu) and Switch stay manual-only — offered in the
        // sheet per the amendment, never a silent default.
        for id in ["xbox", "switch"] {
            let sys = system_by_id(id).unwrap_or_else(|| panic!("{id} in table"));
            assert!(
                sys.options
                    .iter()
                    .all(|o| o.class != OptClass::DefaultAble),
                "{id} has no default-able option"
            );
        }
    }

    #[test]
    fn core_file_extraction_and_slugs() {
        assert_eq!(
            core_file_of("%EMULATOR_RETROARCH% -L %CORE_RETROARCH%/mesen_libretro.so %ROM%"),
            Some("mesen_libretro.so".to_string())
        );
        assert_eq!(core_file_of("%EMULATOR_MESEN% --fullscreen %ROM%"), None);
        assert_eq!(slug("Mesen (Standalone)"), "mesen-standalone");
        assert_eq!(slug("Snes9x - Current"), "snes9x-current");
    }
}
