//! BIOS / system-file requirements (amendment §3's BIOS column,
//! §6's System-files card; file inventory from RESEARCH §2.4).
//!
//! Some systems can't play a game until a small file dumped from
//! the owner's own console sits in the emulator's system folder.
//! Fable **never downloads and never bundles** these files — it
//! detects their absence *before* a launch (never after a black
//! screen), explains in plain words, and files the man's own copy
//! into the right home when he hands one over (the Add flow).
//!
//! This module is the pure data + matching half: which systems
//! need which files, whether a set is satisfied by a directory
//! listing, and which canonical name a picked file files under.
//! The amendment names no hashes, so verification is: right name,
//! copied, and a size-checked re-read after filing — the glue
//! performs the copy; the facts stay checkable here.
//!
//! Presence home (v1.1): the detected RetroArch flavor's `system`
//! directory — every v1.1 gated default is a libretro core, so
//! that is where launches look. The gate applies to libretro-core
//! options only; a standalone keeps its own home and its own rules
//! (the standalone wave, v2, brings its folders with it).

use serde::{Deserialize, Serialize};

/// How much a system needs its files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BiosNeed {
    /// Games won't boot without them.
    Required,
    /// Games play without them, better with them (Dreamcast).
    Recommended,
    /// A faithfulness plus, never a gate (GBA / DS).
    Optional,
}

impl BiosNeed {
    pub fn word(self) -> &'static str {
        match self {
            BiosNeed::Required => "Needed to play",
            BiosNeed::Recommended => "Recommended",
            BiosNeed::Optional => "Optional",
        }
    }
}

/// One set of files that belongs together: satisfied when **any**
/// accepted name is present (region variants) or, with `all`,
/// when **every** name is present (Channel F's trio, Dreamcast's
/// boot+flash pair).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BiosSet {
    /// What this set is, for humans ("PlayStation BIOS").
    pub what: &'static str,
    /// Accepted exact file names (matched case-insensitively).
    pub names: &'static [&'static str],
    /// Glob alternative: a file whose lowercase name starts with
    /// the first element and ends with the second also counts
    /// (Amiga Kickstarts: `kick….CD32`). When set, `names` is
    /// empty and a picked file keeps its own name when filed.
    pub glob: Option<(&'static str, &'static str)>,
    /// Every name must be present (vs any one of them).
    pub all: bool,
    /// Subdirectory of the system folder ("" = the folder itself,
    /// "dc" = Dreamcast's corner, "fbneo" = FinalBurn Neo's).
    pub subdir: &'static str,
}

impl BiosSet {
    /// The names to show a human (the glob renders as a pattern).
    pub fn display_names(&self) -> Vec<String> {
        if let Some((pre, suf)) = self.glob {
            return vec![format!("{pre}…{suf}")];
        }
        self.names.iter().map(|s| s.to_string()).collect()
    }

    fn name_matches(&self, file_name: &str) -> bool {
        let lower = file_name.to_lowercase();
        if self.names.iter().any(|n| n.to_lowercase() == lower) {
            return true;
        }
        if let Some((pre, suf)) = self.glob {
            return lower.starts_with(pre) && lower.ends_with(suf) && lower.len() > pre.len() + suf.len();
        }
        false
    }

    /// Is this set satisfied by a listing of file names from its
    /// home directory?
    pub fn satisfied_by(&self, listing: &[String]) -> bool {
        if self.all {
            self.names
                .iter()
                .all(|n| listing.iter().any(|f| f.eq_ignore_ascii_case(n)))
        } else {
            listing.iter().any(|f| self.name_matches(f))
        }
    }

    /// When a man hands Fable a file, does it belong to this set —
    /// and under which name should it be filed? Exact names file
    /// under the spec's canonical spelling; glob matches keep the
    /// picked file's own name (every Kickstart is named for its
    /// version, and the name is the information).
    pub fn match_pick(&self, file_name: &str) -> Option<String> {
        let lower = file_name.to_lowercase();
        for n in self.names {
            if n.to_lowercase() == lower {
                return Some(n.to_string());
            }
        }
        if self.glob.is_some() && self.name_matches(file_name) {
            return Some(file_name.to_string());
        }
        None
    }

    /// The names still missing given a listing (for "what's
    /// missing" copy): for any-of sets, the whole set's names when
    /// unsatisfied; for all-of sets, just the absent ones.
    pub fn missing_names(&self, listing: &[String]) -> Vec<String> {
        if self.satisfied_by(listing) {
            return Vec::new();
        }
        if self.all {
            self.names
                .iter()
                .filter(|n| !listing.iter().any(|f| f.eq_ignore_ascii_case(n)))
                .map(|s| s.to_string())
                .collect()
        } else {
            self.display_names()
        }
    }
}

/// One system's system-file requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BiosRequirement {
    /// Table system id ("psx", "saturn", …).
    pub system: &'static str,
    /// Card heading ("PlayStation").
    pub title: &'static str,
    pub need: BiosNeed,
    /// True: Play gates on these files (the ask-once sheet says so
    /// before any launch). False: the card and the sheet inform,
    /// but a missing file never blocks a launch (files needed by
    /// only some games — Neo Geo sets, TurboGrafx-CD on a HuCard
    /// shelf — or merely recommended ones).
    pub gate_play: bool,
    pub sets: &'static [BiosSet],
    /// One plain line of truth for the card.
    pub note: &'static str,
}

impl BiosRequirement {
    /// The sets not satisfied, given a per-set listing lookup
    /// (the glue lists each set's home directory).
    pub fn missing_sets<'a>(
        &'a self,
        mut listing_of: impl FnMut(&BiosSet) -> Vec<String>,
    ) -> Vec<&'a BiosSet> {
        self.sets
            .iter()
            .filter(|s| !s.satisfied_by(&listing_of(s)))
            .collect()
    }

    /// Every file name this requirement accepts, spec spelling —
    /// what an option's `bios` display list carries.
    pub fn all_names(&self) -> Vec<String> {
        let mut out = Vec::new();
        for s in self.sets {
            out.extend(s.display_names());
        }
        out
    }
}

const fn set(
    what: &'static str,
    names: &'static [&'static str],
    all: bool,
    subdir: &'static str,
) -> BiosSet {
    BiosSet {
        what,
        names,
        glob: None,
        all,
        subdir,
    }
}

const fn any(what: &'static str, names: &'static [&'static str]) -> BiosSet {
    set(what, names, false, "")
}

const fn all_of(what: &'static str, names: &'static [&'static str], subdir: &'static str) -> BiosSet {
    set(what, names, true, subdir)
}

/// The v1.1 system-file table (amendment §7: the BIOS card ships
/// with the core families — PS1/Saturn/Lynx/Odyssey²/Channel F
/// named, the rest of the amendment's BIOS column with them).
pub static BIOS_REQUIREMENTS: &[BiosRequirement] = &[
    BiosRequirement {
        system: "psx",
        title: "PlayStation",
        need: BiosNeed::Required,
        gate_play: true,
        sets: &[any(
            "PlayStation BIOS",
            &["scph5500.bin", "scph5501.bin", "scph5502.bin"],
        )],
        note: "One file from a PlayStation you own — any region works.",
    },
    BiosRequirement {
        system: "saturn",
        title: "Saturn",
        need: BiosNeed::Required,
        gate_play: true,
        sets: &[any(
            "Saturn BIOS",
            &["sega_101.bin", "mpr-17933.bin", "saturn_bios.bin"],
        )],
        note: "One file from a Saturn you own — any region works.",
    },
    BiosRequirement {
        system: "segacd",
        title: "Sega CD",
        need: BiosNeed::Required,
        gate_play: true,
        sets: &[any(
            "Sega CD BIOS",
            &["bios_CD_U.bin", "bios_CD_E.bin", "bios_CD_J.bin"],
        )],
        note: "One file from a Sega CD you own — any region works.",
    },
    BiosRequirement {
        system: "pcenginecd",
        title: "TurboGrafx-CD",
        need: BiosNeed::Required,
        gate_play: true,
        sets: &[any("Super System Card", &["syscard3.pce"])],
        note: "The System Card file from a TurboGrafx-CD or PC Engine CD setup you own.",
    },
    BiosRequirement {
        system: "atarilynx",
        title: "Atari Lynx",
        need: BiosNeed::Required,
        gate_play: true,
        sets: &[any("Lynx boot ROM", &["lynxboot.img"])],
        note: "One small file from a Lynx you own.",
    },
    BiosRequirement {
        system: "odyssey2",
        title: "Odyssey²",
        need: BiosNeed::Required,
        gate_play: true,
        sets: &[any("Odyssey² system ROM", &["o2rom.bin"])],
        note: "One small file from an Odyssey² you own.",
    },
    BiosRequirement {
        system: "channelf",
        title: "Channel F",
        need: BiosNeed::Required,
        gate_play: true,
        sets: &[all_of(
            "Channel F system files",
            &["sl31253.bin", "sl31254.bin", "sl90025.bin"],
            "",
        )],
        note: "All three files — they come as a set from a Channel F you own.",
    },
    BiosRequirement {
        system: "colecovision",
        title: "ColecoVision",
        need: BiosNeed::Required,
        gate_play: true,
        sets: &[any("ColecoVision BIOS", &["colecovision.rom"])],
        note: "One file from a ColecoVision you own.",
    },
    BiosRequirement {
        system: "atari5200",
        title: "Atari 5200",
        need: BiosNeed::Required,
        gate_play: true,
        sets: &[any("Atari 5200 BIOS", &["5200.rom"])],
        note: "One file from an Atari 5200 you own.",
    },
    BiosRequirement {
        system: "pcfx",
        title: "PC-FX",
        need: BiosNeed::Required,
        gate_play: true,
        sets: &[any("PC-FX BIOS", &["pcfx.rom"])],
        note: "One file from a PC-FX you own.",
    },
    BiosRequirement {
        system: "neogeocd",
        title: "Neo Geo CD",
        need: BiosNeed::Required,
        gate_play: true,
        sets: &[any("Neo Geo CD BIOS", &["neocd.bin"])],
        note: "One file from a Neo Geo CD you own.",
    },
    BiosRequirement {
        system: "3do",
        title: "3DO",
        need: BiosNeed::Required,
        gate_play: true,
        sets: &[any("3DO system ROM", &["panafz10.bin", "panafz1.bin"])],
        note: "One file from a 3DO you own.",
    },
    BiosRequirement {
        system: "amigacd32",
        title: "Amiga CD32",
        need: BiosNeed::Required,
        gate_play: true,
        sets: &[BiosSet {
            what: "CD32 Kickstart ROM",
            names: &[],
            glob: Some(("kick", ".cd32")),
            all: false,
            subdir: "",
        }],
        note: "The CD32's own system software — a file named like kick40060.CD32, from an Amiga you own.",
    },
    BiosRequirement {
        system: "arcade",
        title: "Arcade (Neo Geo games)",
        need: BiosNeed::Required,
        gate_play: false,
        sets: &[BiosSet {
            what: "Neo Geo BIOS set",
            names: &["neogeo.zip"],
            glob: None,
            all: false,
            subdir: "fbneo",
        }],
        note: "Only Neo Geo games need it. It's usually kept with the game sets themselves; Fable also checks RetroArch's system folder.",
    },
    BiosRequirement {
        system: "neogeo",
        title: "Neo Geo",
        need: BiosNeed::Required,
        gate_play: false,
        sets: &[BiosSet {
            what: "Neo Geo BIOS set",
            names: &["neogeo.zip"],
            glob: None,
            all: false,
            subdir: "fbneo",
        }],
        note: "Neo Geo game sets usually carry neogeo.zip alongside them; Fable also checks RetroArch's system folder.",
    },
    BiosRequirement {
        system: "dreamcast",
        title: "Dreamcast",
        need: BiosNeed::Recommended,
        gate_play: false,
        sets: &[all_of(
            "Dreamcast BIOS",
            &["dc_boot.bin", "dc_flash.bin"],
            "dc",
        )],
        note: "Most games play without these (Flycast has a fallback). With them, everything boots the way it did on the console.",
    },
    BiosRequirement {
        system: "gba",
        title: "Game Boy Advance",
        need: BiosNeed::Optional,
        gate_play: false,
        sets: &[any("Game Boy Advance BIOS", &["gba_bios.bin"])],
        note: "Optional — mGBA's built-in replacement is very good. The real file, from a Game Boy Advance you own, is a hair more faithful.",
    },
    BiosRequirement {
        system: "nds",
        title: "Nintendo DS",
        need: BiosNeed::Optional,
        gate_play: false,
        sets: &[all_of(
            "Nintendo DS system files",
            &["bios7.bin", "bios9.bin", "firmware.bin"],
            "",
        )],
        note: "Optional for DS games; DSi software needs them. From a Nintendo DS you own.",
    },
    BiosRequirement {
        system: "tg16",
        title: "TurboGrafx-16",
        need: BiosNeed::Required,
        gate_play: false,
        sets: &[any("Super System Card", &["syscard3.pce"])],
        note: "Only CD games need the System Card — HuCard games play without it.",
    },
];

/// The requirement for one table system id, when it has one.
pub fn requirement_for(system: &str) -> Option<&'static BiosRequirement> {
    BIOS_REQUIREMENTS.iter().find(|r| r.system == system)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn any_of_sets_accept_any_region_case_insensitively() {
        let req = requirement_for("psx").unwrap();
        let set = &req.sets[0];
        assert!(set.satisfied_by(&listing(&["SCPH5501.BIN"])));
        assert!(set.satisfied_by(&listing(&["scph5500.bin"])));
        assert!(!set.satisfied_by(&listing(&["something_else.bin"])));
        assert_eq!(set.missing_names(&listing(&[])).len(), 3);
    }

    #[test]
    fn all_of_sets_need_every_file() {
        let req = requirement_for("channelf").unwrap();
        let set = &req.sets[0];
        assert!(!set.satisfied_by(&listing(&["sl31253.bin", "sl31254.bin"])));
        assert!(set.satisfied_by(&listing(&[
            "sl31253.bin",
            "sl31254.bin",
            "sl90025.bin"
        ])));
        assert_eq!(
            set.missing_names(&listing(&["sl31253.bin"])),
            vec!["sl31254.bin".to_string(), "sl90025.bin".to_string()]
        );
    }

    #[test]
    fn glob_sets_match_kickstarts_and_keep_their_names() {
        let req = requirement_for("amigacd32").unwrap();
        let set = &req.sets[0];
        assert!(set.satisfied_by(&listing(&["kick40060.CD32"])));
        assert!(!set.satisfied_by(&listing(&["kick33180.A500"])));
        assert_eq!(
            set.match_pick("kick40060.CD32").as_deref(),
            Some("kick40060.CD32")
        );
    }

    #[test]
    fn picks_file_under_the_canonical_name() {
        let req = requirement_for("atarilynx").unwrap();
        assert_eq!(
            req.sets[0].match_pick("LYNXBOOT.IMG").as_deref(),
            Some("lynxboot.img")
        );
        assert_eq!(req.sets[0].match_pick("readme.txt"), None);
    }

    #[test]
    fn every_requirement_system_exists_in_the_table() {
        // The BIOS table must never name a system the resolution
        // table doesn't carry (the card would promise a row for a
        // system Play can't reach).
        for req in BIOS_REQUIREMENTS {
            assert!(
                crate::table::system_by_id(req.system).is_some(),
                "bios requirement for unknown system {}",
                req.system
            );
        }
    }

    #[test]
    fn gated_systems_are_the_amendments_required_rows() {
        for id in [
            "psx",
            "saturn",
            "segacd",
            "pcenginecd",
            "atarilynx",
            "odyssey2",
            "channelf",
            "colecovision",
            "atari5200",
            "pcfx",
            "neogeocd",
            "3do",
            "amigacd32",
        ] {
            let req = requirement_for(id).unwrap_or_else(|| panic!("{id} requirement"));
            assert!(req.gate_play, "{id} must gate play");
            assert_eq!(req.need, BiosNeed::Required);
        }
        // Informed-but-never-blocked rows.
        for id in ["arcade", "neogeo", "dreamcast", "gba", "nds", "tg16"] {
            assert!(!requirement_for(id).unwrap().gate_play, "{id} must not gate");
        }
    }
}
