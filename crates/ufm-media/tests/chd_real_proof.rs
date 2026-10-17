//! On-demand proof harness for the native CHD reader against REAL
//! discs and the bundled DAT dataset. Ignored by default (it reads
//! production media); run explicitly:
//!
//!   FABLE_CHD_PS2=/path/to/game.chd FABLE_CHD_DC=... FABLE_CHD_PSX=... \
//!   cargo test -p ufm-media --test chd_real_proof -- --ignored --nocapture
//!
//! Each configured file must identify to its DAT name through the
//! exact code path the app ships (identify_file_all over the bundled
//! rom-dats.bin), and its full decompressed stream must match the
//! SHA-1 recorded in its own header at authoring time.

use std::path::PathBuf;

use ufm_media::chd::ChdFile;
use ufm_media::romdat::{identify_file_all, RomDataset};

fn dataset() -> RomDataset {
    let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../src-tauri/resources/rom-dats.bin");
    let data = std::fs::read(p).expect("bundled rom-dats.bin");
    RomDataset::load(data).expect("dataset loads")
}

fn check(env_key: &str, expect_name: &str, expect_sys: &str) {
    let Ok(path) = std::env::var(env_key) else {
        println!("{env_key} not set — skipping");
        return;
    };
    let path = PathBuf::from(path);
    // Full-stream oracle: decompress every hunk through the reader
    // and compare against the header's recorded raw SHA-1.
    let mut chd = ChdFile::open(&path).expect("chd opens");
    let (sha1, bytes) = chd.stream_sha1().expect("stream hashes");
    assert_eq!(bytes, chd.logical_size(), "stream length");
    assert_eq!(
        sha1,
        chd.raw_sha1(),
        "full-stream SHA-1 must match the header's recorded value"
    );
    // Identification through the shipped path.
    let ds = dataset();
    let ids = identify_file_all(&ds, &path).expect("identify runs");
    assert_eq!(ids.len(), 1, "exactly one identification");
    assert_eq!(ids[0].dat_name, expect_name);
    assert_eq!(ids[0].sys, expect_sys);
    assert_eq!(ids[0].container, "chd");
    println!(
        "PROOF {env_key}: {} -> [{expect_sys}] {expect_name} (stream sha1 ok)",
        path.display()
    );
}

#[test]
#[ignore]
fn real_chd_proofs() {
    check("FABLE_CHD_PSX", "Tactics Ogre (USA)", "psx");
    check("FABLE_CHD_DC", "Ikaruga (Japan)", "dreamcast");
    check("FABLE_CHD_PS2", "GrimGrimoire (USA) (En,Ja)", "ps2");
    check(
        "FABLE_CHD_PS2_DVD",
        "Soulcalibur II (USA)",
        "ps2",
    );
}
