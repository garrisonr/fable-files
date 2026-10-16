//! ROM organization layout (Organize parity, Dickles 2026-10-16):
//! where an identified game file belongs on disk.
//!
//! The layout is deliberately boring and matches how ROM managers
//! (and Fable's own DAT shelves) present a collection:
//!
//! ```text
//! <root>/<Platform Display>/<DAT Title>.<ext>
//! ```
//!
//! Only *identified* games are ever filed: the title is the DAT
//! display name stored in the catalog identity (hash-verified), the
//! platform is the identity's primary DAT system, and the folder is
//! that system's display name from the bundled DAT dataset. Nothing
//! in this module guesses from a filename.

use std::path::PathBuf;

use crate::videoid::sanitize_component;

/// Folder name for a DAT system key, given the dataset's
/// `(key, display)` pairs (`RomDataset::systems()`). Unknown keys
/// (custom systems, future datasets) fall back to the key itself —
/// sanitized like every other component — never to a guess.
pub fn platform_display_for(key: &str, systems: &[(String, String)]) -> String {
    for (k, display) in systems {
        if k == key {
            let d = sanitize_component(display);
            if !d.is_empty() {
                return d;
            }
        }
    }
    sanitize_component(key)
}

/// Relative target path for one identified game under its library
/// root: `<Platform Display>/<DAT Title>.<ext>`. The extension is the
/// file's own, lowercased by the caller or here — the archive IS the
/// game for container matches, so it keeps its archive extension.
pub fn rom_target_rel(platform_display: &str, dat_title: &str, ext: &str) -> PathBuf {
    let dir = sanitize_component(platform_display);
    let stem = sanitize_component(dat_title);
    let ext = ext.trim_start_matches('.').to_lowercase();
    let name = if ext.is_empty() {
        stem
    } else {
        format!("{stem}.{ext}")
    };
    PathBuf::from(dir).join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn systems() -> Vec<(String, String)> {
        vec![
            ("nes".into(), "Nintendo Entertainment System".into()),
            ("snes".into(), "Super Nintendo Entertainment System".into()),
            ("neogeo".into(), "Neo Geo".into()),
        ]
    }

    #[test]
    fn display_comes_from_the_dataset() {
        assert_eq!(
            platform_display_for("nes", &systems()),
            "Nintendo Entertainment System"
        );
        assert_eq!(platform_display_for("neogeo", &systems()), "Neo Geo");
    }

    #[test]
    fn unknown_key_falls_back_to_the_key_itself() {
        assert_eq!(platform_display_for("vectrex", &systems()), "vectrex");
    }

    #[test]
    fn target_is_platform_over_dat_title() {
        let rel = rom_target_rel("Nintendo Entertainment System", "Klax (USA)", "nes");
        assert_eq!(
            rel,
            PathBuf::from("Nintendo Entertainment System").join("Klax (USA).nes")
        );
    }

    #[test]
    fn archives_keep_their_extension_and_case_is_normalized() {
        let rel = rom_target_rel("Neo Geo", "3 Count Bout", "ZIP");
        assert_eq!(rel, PathBuf::from("Neo Geo").join("3 Count Bout.zip"));
    }

    #[test]
    fn hostile_names_are_sanitized_like_video_names() {
        // DAT titles and dataset display names are data, not gospel:
        // path separators must never survive into a component.
        let rel = rom_target_rel("Sega / Genesis", "Foo: Bar?", "bin");
        assert_eq!(rel, PathBuf::from("Sega Genesis").join("Foo Bar.bin"));
    }

    #[test]
    fn missing_extension_yields_a_bare_title() {
        let rel = rom_target_rel("Arcade", "pacman", "");
        assert_eq!(rel, PathBuf::from("Arcade").join("pacman"));
    }
}
