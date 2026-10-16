//! RetroArch `.lpl` playlists (amendment §5.5): for RetroArch, the
//! playlist IS the unified view — absolute REAL paths, DAT-name
//! labels, the system's default core attached. No scanning, no farm
//! dependence, refreshed on the same triggers as the farm.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistEntry {
    /// Absolute real path of the game file.
    pub path: String,
    /// Display label (DAT title, or the user's override).
    pub label: String,
    /// Absolute path of the core .so ("" when no core is set).
    pub core_path: String,
    /// Core display name ("Mesen"; "" with the path).
    pub core_name: String,
}

#[derive(Serialize)]
struct LplItem {
    path: String,
    label: String,
    core_path: String,
    core_name: String,
    crc32: String,
    db_name: String,
}

#[derive(Serialize)]
struct Lpl {
    version: String,
    default_core_path: String,
    default_core_name: String,
    label_display_mode: u32,
    right_thumbnail_mode: u32,
    left_thumbnail_mode: u32,
    sort_mode: u32,
    items: Vec<LplItem>,
}

/// Serialize one system's playlist. Entries are sorted by label so
/// a refresh with no changes produces byte-identical output (the
/// glue skips rewriting unchanged files).
pub fn playlist_json(
    db_name: &str,
    default_core_path: &str,
    default_core_name: &str,
    entries: &[PlaylistEntry],
) -> String {
    let mut sorted: Vec<&PlaylistEntry> = entries.iter().collect();
    sorted.sort_by(|a, b| {
        a.label
            .to_lowercase()
            .cmp(&b.label.to_lowercase())
            .then_with(|| a.path.cmp(&b.path))
    });
    let doc = Lpl {
        version: "1.5".to_string(),
        default_core_path: default_core_path.to_string(),
        default_core_name: default_core_name.to_string(),
        label_display_mode: 0,
        right_thumbnail_mode: 0,
        left_thumbnail_mode: 0,
        sort_mode: 2,
        items: sorted
            .into_iter()
            .map(|e| LplItem {
                path: e.path.clone(),
                label: e.label.clone(),
                core_path: e.core_path.clone(),
                core_name: e.core_name.clone(),
                crc32: String::new(),
                db_name: db_name.to_string(),
            })
            .collect(),
    };
    serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playlist_carries_real_paths_and_db_name() {
        let entries = vec![
            PlaylistEntry {
                path: "/media/Desk SSD/Games/1942.nes".to_string(),
                label: "1942 (Japan, USA)".to_string(),
                core_path: "/home/richard/cores/mesen_libretro.so".to_string(),
                core_name: "Mesen".to_string(),
            },
            PlaylistEntry {
                path: "/home/richard/Games/10-Yard Fight.nes".to_string(),
                label: "10-Yard Fight (USA, Europe)".to_string(),
                core_path: "/home/richard/cores/mesen_libretro.so".to_string(),
                core_name: "Mesen".to_string(),
            },
        ];
        let text = playlist_json(
            "Nintendo - Nintendo Entertainment System.lpl",
            "/home/richard/cores/mesen_libretro.so",
            "Mesen",
            &entries,
        );
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["version"], "1.5");
        assert_eq!(v["default_core_name"], "Mesen");
        let items = v["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        // Sorted by label: 10-Yard Fight before 1942.
        assert_eq!(items[0]["label"], "10-Yard Fight (USA, Europe)");
        assert_eq!(items[0]["path"], "/home/richard/Games/10-Yard Fight.nes");
        assert_eq!(
            items[0]["db_name"],
            "Nintendo - Nintendo Entertainment System.lpl"
        );
        // Deterministic.
        assert_eq!(
            text,
            playlist_json(
                "Nintendo - Nintendo Entertainment System.lpl",
                "/home/richard/cores/mesen_libretro.so",
                "Mesen",
                &entries
            )
        );
    }
}
