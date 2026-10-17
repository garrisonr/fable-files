//! On-demand proof harness for the Hyperbase #2 container readers
//! (GCZ / CSO / WBFS) against REAL discs from the Hyperbase drive
//! and the bundled DAT dataset. Ignored by default (it reads
//! production media); run explicitly:
//!
//!   FABLE_GCZ_HUNTER=/path/to/game.gcz FABLE_CSO_NAMCO=... \
//!   cargo test -p ufm-media --test discimg_real_proof -- --ignored --nocapture
//!
//! GCZ and CSO files must identify to their DAT names through the
//! exact code path the app ships (identify_file_all over the
//! bundled rom-dats.bin). A WBFS file must do the opposite: its
//! embedded disc header parses (game ID + title are the disc's own
//! bytes), and identification honestly yields NOTHING — a WBFS
//! payload cannot reconstruct the full-ISO hashes the Wii DAT
//! carries, and this harness pins that refusal.

use std::path::PathBuf;

use ufm_media::gcz::GczFile;
use ufm_media::romdat::{identify_file_all, RomDataset};
use ufm_media::wbfs;

fn dataset() -> RomDataset {
    let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../src-tauri/resources/rom-dats.bin");
    let data = std::fs::read(p).expect("bundled rom-dats.bin");
    RomDataset::load(data).expect("dataset loads")
}

fn check_gcz(env_key: &str, expect_name: &str) {
    let Ok(path) = std::env::var(env_key) else {
        println!("{env_key} not set — skipping");
        return;
    };
    let path = PathBuf::from(path);
    // Reader-level: the whole decompressed image hashes cleanly
    // (every block passed its stored Adler-32 to get here).
    let mut gcz = GczFile::open(&path).expect("gcz opens");
    let head = gcz.disc_header().expect("disc header");
    assert!(head.is_gamecube, "inner disc must carry the GC magic");
    let (crc, md5, sha1, bytes) = gcz.hash_image().expect("image hashes");
    assert_eq!(bytes, gcz.disc_size(), "stream length");
    // Identification through the shipped path must agree exactly.
    let ds = dataset();
    let ids = identify_file_all(&ds, &path).expect("identify runs");
    assert_eq!(ids.len(), 1, "exactly one identification");
    assert_eq!(ids[0].dat_name, expect_name);
    assert_eq!(ids[0].sys, "gamecube");
    assert_eq!(ids[0].container, "gcz");
    assert_eq!(ids[0].crc, crc);
    assert_eq!(ids[0].md5, md5);
    assert_eq!(ids[0].sha1, sha1);
    println!(
        "PROOF {env_key}: {} -> [gamecube] {expect_name} (id {}, title {:?}, stream hashes agree)",
        path.display(),
        head.game_id,
        head.title
    );
}

fn check_cso(env_key: &str, expect_names: &[&str]) {
    let Ok(path) = std::env::var(env_key) else {
        println!("{env_key} not set — skipping");
        return;
    };
    let path = PathBuf::from(path);
    let ds = dataset();
    let ids = identify_file_all(&ds, &path).expect("identify runs");
    assert!(!ids.is_empty(), "at least one identification");
    assert_eq!(ids[0].sys, "psp");
    assert_eq!(ids[0].container, "cso");
    assert!(
        expect_names.contains(&ids[0].dat_name.as_str()),
        "dat_name {} not in expected {expect_names:?}",
        ids[0].dat_name
    );
    println!(
        "PROOF {env_key}: {} -> [psp] {} (cso stream hash matched)",
        path.display(),
        ids[0].dat_name
    );
}

fn check_wbfs(env_key: &str, expect_id: &str, expect_title: &str) {
    let Ok(path) = std::env::var(env_key) else {
        println!("{env_key} not set — skipping");
        return;
    };
    let path = PathBuf::from(path);
    let head = wbfs::disc_header(&path)
        .expect("wbfs parses")
        .expect("disc header recognized");
    assert_eq!(head.game_id, expect_id);
    assert_eq!(head.title, expect_title);
    assert_eq!(head.platform, "wii");
    // The honesty pin: no DAT identification may come out of a WBFS.
    let ds = dataset();
    let ids = identify_file_all(&ds, &path).expect("identify runs");
    assert!(ids.is_empty(), "WBFS must not fabricate a DAT match");
    println!(
        "PROOF {env_key}: {} -> header identity {} {:?}, DAT identification: none (honest)",
        path.display(),
        head.game_id,
        head.title
    );
}

#[test]
#[ignore]
fn real_discimg_proofs() {
    check_gcz("FABLE_GCZ_HUNTER", "Hunter - The Reckoning (USA)");
    check_gcz("FABLE_GCZ_ANIMAL", "Animal Crossing (USA, Canada)");
    check_cso(
        "FABLE_CSO_NAMCO",
        &[
            "Namco Museum - Battle Collection (USA) (v1.01)",
            "Namco Museum Battle Collection (USA)",
        ],
    );
    check_wbfs("FABLE_WBFS_DKCR", "SF8E01", "Donkey Kong Country Returns");
    check_wbfs("FABLE_WBFS_LEGO", "SC4E64", "LEGO CloneWars");
}
