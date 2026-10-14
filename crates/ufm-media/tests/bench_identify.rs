//! Throughput bench for the ROM identifier (manual, ignored by the gate).
//!
//! Run on the target box with:
//!   ROM_DATASET_PATH=<rom-dats.bin> ROM_BENCH_DIR=<folder of ROM files> \
//!   cargo test --release -p ufm-media --test bench_identify -- --ignored --nocapture
//!
//! Identifies every candidate file directly inside ROM_BENCH_DIR (no
//! recursion) and prints identified/total + files/sec. This measures the
//! identifier itself (magic gates, size gate, hashing, DAT lookup) — the
//! same code path the app's identify_roms command drives.

use std::time::Instant;
use ufm_media::romdat::{identify_file, systems_for_extension, RomDataset};

#[test]
#[ignore]
fn bench_identify_folder() {
    let ds_path = std::env::var("ROM_DATASET_PATH").expect("ROM_DATASET_PATH");
    let dir = std::env::var("ROM_BENCH_DIR").expect("ROM_BENCH_DIR");
    let t0 = Instant::now();
    let bytes = std::fs::read(&ds_path).expect("dataset readable");
    let ds = RomDataset::load(bytes).expect("dataset loads");
    let load_ms = t0.elapsed().as_millis();
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    for e in std::fs::read_dir(&dir).expect("read bench dir") {
        let e = e.unwrap();
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let ext = p
            .extension()
            .map(|x| x.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if !systems_for_extension(&ext).is_empty() {
            files.push(p);
        }
    }
    files.sort();
    let t1 = Instant::now();
    let mut identified = 0usize;
    let mut bytes: u64 = 0;
    for f in &files {
        bytes += std::fs::metadata(f).map(|m| m.len()).unwrap_or(0);
        if identify_file(&ds, f).map(|r| r.is_some()).unwrap_or(false) {
            identified += 1;
        }
    }
    let secs = t1.elapsed().as_secs_f64();
    println!(
        "BENCH dir={} candidates={} identified={} bytes={} load_ms={} identify_secs={:.2} files_per_sec={:.1}",
        dir,
        files.len(),
        identified,
        bytes,
        load_ms,
        secs,
        files.len() as f64 / secs
    );
    assert!(!files.is_empty(), "bench dir should hold ROM files");
}
