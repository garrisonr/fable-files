//! Indexer v2 scale evidence: a first index of a fixture volume is
//! a full walk with a catalog write per entry; a re-index of the
//! UNCHANGED volume must be dramatically faster because the walk
//! diffs against the catalog manifest and writes nothing.
//!
//! Fixture size via FABLE_FIXTURE_N (default 3000 files). Run on
//! the target box with FABLE_FIXTURE_N=20000 for the ship numbers:
//!   cargo test -p ufm-search --test incremental_fixture -- --nocapture

use std::path::PathBuf;
use std::time::Instant;

use ufm_search::Indexer;

fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ufm-fixture-{}-{}",
        tag,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn location() -> ufm_vfs::Location {
    ufm_vfs::Location {
        id: ufm_core::LocationId::new(),
        provider: "local".into(),
        display_name: "Fixture Volume".into(),
        capabilities: ufm_vfs::Capabilities {
            read: true,
            ..Default::default()
        },
        protected: ufm_vfs::ProtectionTier::None,
    }
}

#[test]
fn incremental_reindex_is_dramatically_faster() {
    let n: usize = std::env::var("FABLE_FIXTURE_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000);
    let work = scratch_dir("perf");
    let root = work.join("volume");
    // 50 subdirs x n/50 files each.
    let per_dir = (n / 50).max(1);
    let total_files = per_dir * 50;
    for d in 0..50 {
        let dir = root.join(format!("dir{d:02}"));
        std::fs::create_dir_all(&dir).unwrap();
        for f in 0..per_dir {
            std::fs::write(
                dir.join(format!("file-{f:05}.dat")),
                format!("fixture content {d} {f}").as_bytes(),
            )
            .unwrap();
        }
    }
    let catalog_path = work.join("catalog.db");
    let checkpoint_path = work.join("checkpoint.json");
    let loc = location();

    // First index: full.
    let t0 = Instant::now();
    let s1 = {
        let mut ix = Indexer::open(&catalog_path, &checkpoint_path).unwrap();
        ix.index_root(&root, &loc).unwrap()
    };
    let first = t0.elapsed();
    assert_eq!(s1.files_done as usize, total_files);
    assert_eq!(s1.files_unchanged, 0);

    // Re-index unchanged (fresh Indexer, like a fresh app launch).
    let t1 = Instant::now();
    let s2 = {
        let mut ix = Indexer::open(&catalog_path, &checkpoint_path).unwrap();
        ix.index_root(&root, &loc).unwrap()
    };
    let second = t1.elapsed();
    let expected_entries = (total_files + 50 + 1) as u64; // files + dirs + root
    assert_eq!(s2.files_done, 0, "nothing rewritten");
    assert_eq!(s2.files_unchanged, expected_entries);
    assert_eq!(s2.files_removed, 0);

    eprintln!(
        "FIXTURE n={} first_index={:?} reindex_unchanged={:?} speedup={:.1}x",
        total_files,
        first,
        second,
        first.as_secs_f64() / second.as_secs_f64().max(0.000001)
    );
    assert!(
        second < first,
        "incremental re-index ({second:?}) must beat the first index ({first:?})"
    );

    // Mutate 1%: changed + added + removed files are all picked up.
    let changed = (total_files / 100).max(1);
    for f in 0..changed {
        std::fs::write(
            root.join("dir00").join(format!("file-{f:05}.dat")),
            b"changed content with a different length",
        )
        .unwrap();
    }
    for f in 0..changed {
        std::fs::remove_file(root.join("dir01").join(format!("file-{f:05}.dat"))).unwrap();
    }
    for f in 0..changed {
        std::fs::write(root.join("dir02").join(format!("added-{f:05}.dat")), b"new").unwrap();
    }
    let s3 = {
        let mut ix = Indexer::open(&catalog_path, &checkpoint_path).unwrap();
        ix.index_root(&root, &loc).unwrap()
    };
    assert_eq!(s3.files_updated as usize, changed);
    assert_eq!(s3.files_removed as usize, changed);
    assert_eq!(s3.files_done as usize, changed * 2, "changed + added were written");

    let _ = std::fs::remove_dir_all(&work);
}
