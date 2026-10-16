//! Resolution: what state is each option in on THIS machine, and
//! what does Play do about it (amendment §2 + §6).
//!
//! The app crate probes the machine into an [`EnvProbe`]; everything
//! after that is pure decision logic, tested here.

use crate::table::{EmuOption, InstallSpec, OptClass, OptionKind, SystemEntry};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Which RetroArch the machine has, when it has one. Flatpak wins
/// when both exist — it's the channel Fable installs (§4.1), so it's
/// the one Fable's cores and playlists should follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RaFlavor {
    Flatpak,
    Native,
}

/// A snapshot of the machine, taken by the app crate's probe.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvProbe {
    pub ra: Option<RaFlavor>,
    /// Core files present where RetroArch will find them
    /// ("mesen_libretro.so", …).
    pub cores: BTreeSet<String>,
    /// Standalone option ids whose probe binaries were found.
    pub standalones: BTreeSet<String>,
    /// flatpak exists AND a flathub remote is configured — the
    /// RetroArch install channel is available.
    pub flatpak_available: bool,
}

/// One option's state on this machine — the sheet's state words are
/// rendered from this, and every word is a verified fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OptState {
    /// Installed and (for core options) the core is present.
    Ready,
    /// A libretro core option and RetroArch itself is missing
    /// (installing RetroArch + the core fixes it).
    NeedsRetroArch,
    /// RetroArch is here; this core isn't yet.
    NeedsCore,
    /// A standalone with a Fable install channel that isn't here yet.
    NeedsInstall,
    /// BIOS/firmware files are missing (checked by the glue, which
    /// knows the system directory; the pure layer only combines).
    NeedsBios,
    /// Fable can't install this one (yet); usable if present.
    ManualOnly,
    /// No quality path exists; never offered as a launch.
    NotYet,
}

impl OptState {
    /// The sheet's small state word (amendment §6).
    pub fn word(self) -> &'static str {
        match self {
            OptState::Ready => "Ready",
            OptState::NeedsRetroArch | OptState::NeedsCore | OptState::NeedsInstall => {
                "Needs install"
            }
            OptState::NeedsBios => "Needs a file from your console",
            OptState::ManualOnly => "Not installed",
            OptState::NotYet => "Not yet",
        }
    }
    /// True when pressing the primary button must install something
    /// first ("Install & Play").
    pub fn needs_install(self) -> bool {
        matches!(
            self,
            OptState::NeedsRetroArch | OptState::NeedsCore | OptState::NeedsInstall
        )
    }
}

/// Classify one option against the probe. BIOS presence is layered
/// on by the caller via [`with_bios`].
pub fn option_state(opt: &EmuOption, env: &EnvProbe) -> OptState {
    if opt.class == OptClass::NotYet {
        return OptState::NotYet;
    }
    match opt.kind {
        OptionKind::LibretroCore => {
            let core = opt.core_file.as_deref().unwrap_or("");
            match env.ra {
                None => {
                    if opt.class == OptClass::ManualOnly {
                        OptState::ManualOnly
                    } else {
                        OptState::NeedsRetroArch
                    }
                }
                Some(_) => {
                    if env.cores.contains(core) {
                        OptState::Ready
                    } else if opt.class == OptClass::ManualOnly {
                        OptState::ManualOnly
                    } else {
                        OptState::NeedsCore
                    }
                }
            }
        }
        OptionKind::Standalone => {
            if env.standalones.contains(&opt.id) {
                OptState::Ready
            } else {
                match opt.install {
                    InstallSpec::None => OptState::ManualOnly,
                    _ => OptState::NeedsInstall,
                }
            }
        }
        // The ES-DE door and the OS default are handled by the
        // sheet itself; they never gate Play.
        OptionKind::EsDeDoor | OptionKind::OsDefault => OptState::Ready,
    }
}

/// Layer BIOS presence onto a state: an otherwise-ready or
/// installable option whose required files are absent is a
/// BIOS-missing option (the sheet explains before any black screen).
pub fn with_bios(state: OptState, opt: &EmuOption, bios_present: bool) -> OptState {
    if !opt.bios.is_empty() && !bios_present && state != OptState::NotYet {
        return OptState::NeedsBios;
    }
    state
}

/// What Play does (amendment §6): the sheet is a door you walk
/// through once per system, not a tollbooth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum Decision {
    /// A saved choice exists and is ready — just launch it.
    PlayNow { option_id: String },
    /// Show the ask-once sheet with this option pre-selected.
    Sheet { preselect: String },
}

pub fn decide(sys: &SystemEntry, env: &EnvProbe, saved_choice: Option<&str>) -> Decision {
    if let Some(saved) = saved_choice {
        if let Some(opt) = sys.option(saved) {
            if option_state(opt, env) == OptState::Ready {
                return Decision::PlayNow {
                    option_id: saved.to_string(),
                };
            }
            // A saved choice that isn't ready any more (RetroArch
            // removed, core deleted) re-opens the sheet ON it —
            // never a silent swap to a different emulator.
            return Decision::Sheet {
                preselect: saved.to_string(),
            };
        }
    }
    let preselect = sys
        .default()
        .map(|o| o.id.clone())
        .unwrap_or_default();
    Decision::Sheet { preselect }
}

/// The primary button's label for a sheet state (amendment §6):
/// "Install & Play" when the selected option needs an install,
/// "Play" when it's ready. BIOS-missing keeps "Play" disabled in
/// the UI — this function only names the happy paths.
pub fn primary_label(state: OptState) -> &'static str {
    if state.needs_install() {
        "Install & Play"
    } else {
        "Play"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::system_by_id;

    fn env_cold() -> EnvProbe {
        EnvProbe {
            ra: None,
            cores: BTreeSet::new(),
            standalones: BTreeSet::new(),
            flatpak_available: true,
        }
    }

    fn env_ready() -> EnvProbe {
        let mut e = env_cold();
        e.ra = Some(RaFlavor::Flatpak);
        e.cores.insert("mesen_libretro.so".to_string());
        e.cores.insert("snes9x_libretro.so".to_string());
        e
    }

    #[test]
    fn cold_box_shows_sheet_with_default_preselected() {
        let nes = system_by_id("nes").unwrap();
        assert_eq!(
            decide(nes, &env_cold(), None),
            Decision::Sheet {
                preselect: "ra-mesen".to_string()
            }
        );
        let def = nes.option("ra-mesen").unwrap();
        assert_eq!(option_state(def, &env_cold()), OptState::NeedsRetroArch);
        assert_eq!(primary_label(OptState::NeedsRetroArch), "Install & Play");
    }

    #[test]
    fn saved_ready_choice_plays_without_sheet() {
        let nes = system_by_id("nes").unwrap();
        assert_eq!(
            decide(nes, &env_ready(), Some("ra-mesen")),
            Decision::PlayNow {
                option_id: "ra-mesen".to_string()
            }
        );
    }

    #[test]
    fn saved_choice_missing_core_reopens_sheet_on_it() {
        let nes = system_by_id("nes").unwrap();
        let mut env = env_ready();
        env.cores.remove("mesen_libretro.so");
        assert_eq!(
            decide(nes, &env, Some("ra-mesen")),
            Decision::Sheet {
                preselect: "ra-mesen".to_string()
            }
        );
        let def = nes.option("ra-mesen").unwrap();
        assert_eq!(option_state(def, &env), OptState::NeedsCore);
    }

    #[test]
    fn standalone_states_are_honest() {
        let nes = system_by_id("nes").unwrap();
        let mesen_sa = nes.option("mesen-standalone").unwrap();
        // Absent + no Fable install channel → manual-only, worded
        // "Not installed", never "Needs install" (Fable can't).
        assert_eq!(option_state(mesen_sa, &env_ready()), OptState::ManualOnly);
        assert_eq!(OptState::ManualOnly.word(), "Not installed");
        // Present → ready.
        let mut env = env_ready();
        env.standalones.insert("mesen-standalone".to_string());
        assert_eq!(option_state(mesen_sa, &env), OptState::Ready);
    }

    #[test]
    fn snes_default_flows() {
        let snes = system_by_id("snes").unwrap();
        assert_eq!(
            decide(snes, &env_cold(), None),
            Decision::Sheet {
                preselect: "ra-snes9x".to_string()
            }
        );
        assert_eq!(
            decide(snes, &env_ready(), Some("ra-snes9x")),
            Decision::PlayNow {
                option_id: "ra-snes9x".to_string()
            }
        );
    }
}
