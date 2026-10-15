//! Loads the real compiled rom-dats.bin (path from ROM_DATASET_PATH).
//! Ignored by default — the dataset is a build resource, not a fixture.
//! Run: ROM_DATASET_PATH=../../src-tauri/resources/rom-dats.bin \
//!      cargo test -p ufm-media --test real_dataset -- --ignored
use ufm_media::romdat::RomDataset;

#[test]
#[ignore]
fn real_dataset_loads_and_answers() {
    let path = std::env::var("ROM_DATASET_PATH").expect("ROM_DATASET_PATH set");
    let bytes = std::fs::read(&path).expect("dataset readable");
    let ds = RomDataset::load(bytes).expect("dataset loads");
    assert_eq!(ds.systems().len(), 60);
    assert!(ds.row_count() > 100_000, "rows: {}", ds.row_count());
    assert!(ds.set_count() > 30_000, "sets: {}", ds.set_count());
    // Super Mario Bros. (World) headerless NES data per the No-Intro
    // DAT: 40960 bytes, CRC32 D445F698 (the .unh row; the headered
    // .nes file is 40976 bytes, CRC 393A432F).
    let nes = ds.sys_index("nes").unwrap();
    let hits = ds.lookup(0xD445_F698, 40_960, None, None, &[nes]);
    assert!(
        hits.iter().any(|h| h.name.contains("Super Mario Bros")),
        "SMB lookup: {hits:?}"
    );
    // The MAME set for Metal Slug exists and needs >5 ROMs.
    let arcade = ds.sys_index("arcade").unwrap();
    let set = ds.find_set(arcade, "mslug").expect("mslug set");
    assert!(set.display.contains("Metal Slug"), "{}", set.display);
    assert!(set.roms.len() >= 5);
    // …and the same set is in the Neo Geo family.
    let neogeo = ds.sys_index("neogeo").unwrap();
    assert!(ds.find_set(neogeo, "mslug").is_some());
    // Size gate sanity: no NES ROM is 999,999,999 bytes.
    assert!(!ds.size_known(nes, 999_999_999));
    // Restored systems (ROM organization pass, 2026-10-15): the
    // handhelds Dickles owns that were missing despite DATs existing.
    // Game.com "Batman & Robin (USA)": 2,097,152 bytes, CRC32 B336E530
    // (Tiger - Game.com.dat). Odyssey2 "4 en 1 Ligne (France)":
    // 4,096 bytes, CRC32 9B5E9356 (Magnavox - Odyssey2.dat).
    let gamecom = ds.sys_index("gamecom").expect("gamecom in dataset");
    let hits = ds.lookup(0xB336_E530, 2_097_152, None, None, &[gamecom]);
    assert!(
        hits.iter().any(|h| h.name.contains("Batman & Robin")),
        "Game.com lookup: {hits:?}"
    );
    let odyssey2 = ds.sys_index("odyssey2").expect("odyssey2 in dataset");
    let hits = ds.lookup(0x9B5E_9356, 4_096, None, None, &[odyssey2]);
    assert!(
        hits.iter().any(|h| h.name.contains("4 en 1 Ligne")),
        "Odyssey2 lookup: {hits:?}"
    );
    for key in [
        "channelf", "studio2", "arcadia2001", "scv", "pv1000", "superacan", "sg1000", "vic20",
        "psvita", "cdi", "pcfx", "neogeocd", "amigacd32", "gx4000",
    ] {
        assert!(ds.sys_index(key).is_some(), "{key} in dataset");
    }
}
