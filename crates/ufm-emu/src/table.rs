//! The systems table: mined ES-DE data + Fable's override layer.
//!
//! Per SPEC-AMENDMENT §2/§3: ES-DE's preference order picks the
//! default (the romm-tender pattern — first usable command wins);
//! Fable's overrides win wherever a product call differs — verdict
//! lines in plain language, install channels, honesty states. In v1
//! the override layer covers NES and SNES (the systems with games on
//! Dickles' shelves); the other 190+ systems ride along as mined
//! data for the rounds that widen the net.

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

fn build_table() -> Vec<SystemEntry> {
    let mut out: Vec<SystemEntry> = vendored_systems().iter().map(mined_entry).collect();
    for entry in &mut out {
        match entry.id.as_str() {
            "nes" => override_nes(entry),
            "snes" => override_snes(entry),
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
        // A system with no override: the default is ES-DE's first
        // usable command, and %INJECT% templates classify manual-only.
        let gb = system_by_id("gb").expect("gb mined");
        assert!(!gb.overridden);
        assert!(gb.default_option.is_some());
        let def = gb.default().unwrap();
        assert_eq!(def.class, OptClass::DefaultAble);
        assert_eq!(def.kind, OptionKind::LibretroCore);
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
