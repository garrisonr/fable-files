//! Projectionist landing proof: the bundled dataset carries the 13 new
//! list-governed console systems (Projectionist, 2026-10-18) and the 7
//! DAT-governed gaming computers (computer completion pass,
//! 2026-10-19), each resolving by key with real rows, and the splits
//! plus single-source computers hold their exact manifest counts.
//! Appended systems (indices shift never; appends only) keep this
//! green as the dataset grows.

use ufm_media::romdat::RomDataset;

fn load() -> RomDataset {
    let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../src-tauri/resources/rom-dats.bin");
    let data = std::fs::read(p).expect("bundled rom-dats.bin");
    RomDataset::load(data).expect("dataset parses")
}

#[test]
fn projectionist_systems_resolve_with_rows() {
    let ds = load();
    assert!(
        ds.systems().len() >= 80,
        "dataset must carry >= 80 systems, has {}",
        ds.systems().len()
    );
    let new_keys = [
        "cassettevision", "loopy", "cdtv", "pokemonmini", "supervision",
        "gp32", "vsmile", "leapster", "pico", "supergrafx", "n64dd",
        "tgcd", "jaguarcd",
    ];
    let computer_keys = [
        "atarist", "apple2", "cpc", "pc88", "pc98", "x68000", "x1",
    ];
    let by_sys = ds.rows_by_system();
    for key in new_keys.iter().chain(computer_keys.iter()) {
        let idx = ds.sys_index(key).unwrap_or_else(|| panic!("system key {key} must resolve"));
        let rows = &by_sys[idx as usize];
        assert!(!rows.is_empty(), "system {key} must carry DAT rows");
        assert!(!rows[0].name.is_empty(), "system {key} rows carry names");
    }
    // Exact manifest counts (DAT-MANIFEST.md / ROM-DATS.md).
    let exact = [
        ("tgcd", 502usize),     // the TurboGrafx-CD split
        ("jaguarcd", 27usize),  // the Jaguar CD split
        ("atarist", 404usize),  // No-Intro
        ("apple2", 9227usize),  // TOSEC
        ("cpc", 5058usize),     // TOSEC
        ("pc88", 4138usize),    // TOSEC
        ("pc98", 5426usize),    // TOSEC 5316 + Redump 110
        ("x68000", 3151usize),  // No-Intro 1 + TOSEC 3150
        ("x1", 251usize),       // No-Intro 8 + TOSEC 243
    ];
    for (key, want) in exact {
        let idx = ds.sys_index(key).unwrap();
        assert_eq!(by_sys[idx as usize].len(), want, "{key} rows");
    }
}
