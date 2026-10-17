//! Index scope: the per-location admission contract the safety
//! gate writes at add time (Hyperbase Solution #3, 2026-10-16).
//!
//! A scope answers one question for every entry the walker meets:
//! does this entry belong in the catalog? Two independent filters,
//! applied together:
//!
//! - **Folder pick.** Only the chosen top-level directories are
//!   walked at all (unchosen subtrees are pruned whole); files
//!   sitting at the root ride on their own flag. `None` = the whole
//!   tree, which is every pre-gate location's scope.
//! - **ROM focus** ("this is a ROM drive"). Files are admitted only
//!   when their extension is a ROM-identification candidate — the
//!   exact set and semantics the identification pass feeds on
//!   ([`crate::census::is_candidate`]). Directories are ALWAYS
//!   admitted: folders are cataloged so the tree stays navigable,
//!   and non-candidate files remain browsable live on disk — they
//!   are simply never cataloged.
//!
//! The scope is deliberately a pure value: no I/O, no globals. The
//! Tauri layer persists it (location-scopes sidecar); the indexer
//! consumes it; the harness proves it against fixture trees.

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::census::is_candidate;

/// What of a location's tree belongs in the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexScope {
    /// ROM focus: admit only candidate-extension files.
    #[serde(default)]
    pub rom_focus: bool,
    /// Folder pick: the admitted top-level directory names.
    /// `None` admits the whole tree (no pick was made).
    #[serde(default)]
    pub include_dirs: Option<HashSet<String>>,
    /// With a folder pick in force: are files sitting at the root
    /// itself admitted? (They have no top-level directory; the
    /// census sheet offers them as their own row.)
    #[serde(default = "default_true")]
    pub include_root_files: bool,
}

fn default_true() -> bool {
    true
}

impl Default for IndexScope {
    fn default() -> Self {
        Self {
            rom_focus: false,
            include_dirs: None,
            include_root_files: true,
        }
    }
}

impl IndexScope {
    /// The unscoped scope: everything, as before the gate existed.
    pub const ALL: IndexScope = IndexScope {
        rom_focus: false,
        include_dirs: None,
        include_root_files: true,
    };

    /// True when this scope filters nothing — the scoped walk then
    /// behaves exactly like the historic `index_root`.
    pub fn is_unrestricted(&self) -> bool {
        !self.rom_focus && self.include_dirs.is_none()
    }

    /// Should the walker enter (or record) this entry?
    ///
    /// `rel` is the entry's path relative to the walk root;
    /// `depth` is its walkdir depth (root = 0). Directories the
    /// folder pick excludes return false at depth 1, which is what
    /// lets the walk prune the subtree instead of filtering leaves.
    pub fn admits(&self, rel: &Path, depth: usize, is_dir: bool, candidates: &HashSet<String>) -> bool {
        if depth == 0 {
            return true; // the root is context, always admitted
        }
        // Folder pick: the depth-1 component decides. A depth-1
        // directory must be named in the pick; anything deeper
        // inherits its top directory's verdict; a depth-1 file is a
        // root file and rides on its own flag.
        if let Some(pick) = &self.include_dirs {
            let first = rel
                .components()
                .next()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .unwrap_or_default();
            if depth == 1 && !is_dir {
                if !self.include_root_files {
                    return false;
                }
            } else if !pick.contains(&first) {
                return false;
            }
        }
        // ROM focus: directories always pass (folders are cataloged
        // and traversal must continue); files pass on candidacy.
        if self.rom_focus && !is_dir {
            let name = rel
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            return is_candidate(&name, candidates);
        }
        true
    }

    /// A stable fingerprint of this scope plus the candidate set it
    /// was interpreted against. The indexer stamps it into its
    /// checkpoint: a resumed walk whose fingerprint differs was
    /// pruned under different rules and must not be trusted (the
    /// P0-2 poison class — directories marked complete under one
    /// scope would hide subtrees from another). FNV-1a over a
    /// canonical encoding: deterministic across processes, which
    /// `DefaultHasher` is not.
    pub fn tag(&self, candidates: &HashSet<String>) -> String {
        let mut canon = String::new();
        canon.push_str(if self.rom_focus { "rom1" } else { "rom0" });
        match &self.include_dirs {
            None => canon.push_str("|all"),
            Some(pick) => {
                let mut names: Vec<&String> = pick.iter().collect();
                names.sort();
                canon.push_str("|pick:");
                for n in names {
                    canon.push_str(n);
                    canon.push('\u{1}');
                }
            }
        }
        if self.rom_focus {
            let mut exts: Vec<&String> = candidates.iter().collect();
            exts.sort();
            canon.push_str("|ext:");
            for e in exts {
                canon.push_str(e);
                canon.push('\u{1}');
            }
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in canon.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("{hash:016x}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn candidates() -> HashSet<String> {
        ["zip", "sfc", "chd"].iter().map(|s| s.to_string()).collect()
    }

    fn rel(p: &str) -> PathBuf {
        PathBuf::from(p)
    }

    #[test]
    fn unrestricted_scope_admits_everything() {
        let s = IndexScope::ALL;
        assert!(s.is_unrestricted());
        assert!(s.admits(&rel("games/Elden Ring/asset.pak"), 3, false, &candidates()));
        assert!(s.admits(&rel("games"), 1, true, &candidates()));
        assert!(s.admits(&rel("loose.txt"), 1, false, &candidates()));
    }

    #[test]
    fn folder_pick_prunes_unpicked_top_dirs_and_root_files() {
        let s = IndexScope {
            rom_focus: false,
            include_dirs: Some(["roms".to_string()].into_iter().collect()),
            include_root_files: false,
        };
        assert!(s.admits(&rel("roms"), 1, true, &candidates()));
        assert!(s.admits(&rel("roms/nes/a.zip"), 3, false, &candidates()));
        assert!(!s.admits(&rel("games"), 1, true, &candidates()));
        assert!(!s.admits(&rel("games/x/y.bin"), 3, false, &candidates()));
        assert!(!s.admits(&rel("loose.sfc"), 1, false, &candidates()));
        let with_root = IndexScope {
            include_root_files: true,
            ..s.clone()
        };
        assert!(with_root.admits(&rel("loose.sfc"), 1, false, &candidates()));
    }

    #[test]
    fn rom_focus_admits_candidates_and_all_dirs_only() {
        let s = IndexScope {
            rom_focus: true,
            ..IndexScope::ALL
        };
        assert!(s.admits(&rel("roms/nes"), 2, true, &candidates()));
        assert!(s.admits(&rel("roms/nes/a.zip"), 3, false, &candidates()));
        assert!(s.admits(&rel("roms/nes/A.ZIP"), 3, false, &candidates()));
        assert!(!s.admits(&rel("roms/nes/boxart.png"), 3, false, &candidates()));
        assert!(!s.admits(&rel("roms/nes/manual.pdf"), 3, false, &candidates()));
        // Both filters together: an unpicked tree loses even its ROMs.
        let both = IndexScope {
            rom_focus: true,
            include_dirs: Some(["roms".to_string()].into_iter().collect()),
            include_root_files: true,
        };
        assert!(!both.admits(&rel("games/a.zip"), 2, false, &candidates()));
        assert!(both.admits(&rel("roms/a.zip"), 2, false, &candidates()));
    }

    #[test]
    fn tag_is_deterministic_and_scope_sensitive() {
        let c = candidates();
        let a = IndexScope::ALL;
        assert_eq!(a.tag(&c), a.tag(&c));
        let rom = IndexScope {
            rom_focus: true,
            ..IndexScope::ALL
        };
        assert_ne!(a.tag(&c), rom.tag(&c), "rom focus changes the tag");
        let pick = IndexScope {
            include_dirs: Some(["roms".to_string()].into_iter().collect()),
            ..IndexScope::ALL
        };
        assert_ne!(a.tag(&c), pick.tag(&c), "a folder pick changes the tag");
        let mut c2 = candidates();
        c2.insert("gcz".into());
        assert_ne!(
            rom.tag(&c),
            rom.tag(&c2),
            "widening the candidate set retags a ROM-focus scope"
        );
        assert_eq!(
            a.tag(&c),
            a.tag(&c2),
            "candidates do not tag scopes that never consult them"
        );
    }
}
