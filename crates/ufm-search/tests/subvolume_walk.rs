//! P0-2 gate fixture: sibling btrfs subvolumes under one mount must
//! ALL be indexed — the walk must not stop at the first subvolume
//! (Dickles' superdog: 256k items under `@/`, ZERO under `@home/`,
//! where his NES library lives).
//!
//! Runs only when `UFM_TEST_BTRFS_ROOT` points at a prepared
//! fixture: a btrfs volume mounted at its top level with sibling
//! subvolumes `@`, `@home`, `@factory`, each holding uniquely-named
//! marker files (see the round's mk-btrfs-fixture.sh). Without the
//! env var the test is a no-op pass, so the regular gate stays
//! green on machines without the fixture.
//!
//! Phases mirror the superdog chronology: a fresh index, a
//! re-index (completed checkpoint -> fresh re-walk), a resume from
//! a checkpoint that had completed only `@` (the interrupted-run
//! shape), and a fresh re-walk after a file vanishes from `@home`
//! (the delete_unseen path). Every phase must keep every subvolume
//! fully visible.

use std::path::{Path, PathBuf};

/// Count item ROWS whose provider key sits under `<root>/<seg>/` —
/// the same evidence shape as the live superdog catalog (256,039
/// rows under `@/`, 0 under `@home/`). FTS hits are deliberately NOT
/// used: they can ghost across re-walks and would muddy the count.
fn count_items_under(catalog_path: &Path, root: &Path, seg: &str) -> i64 {
    let conn = rusqlite::Connection::open(catalog_path).unwrap();
    let prefix = format!("{}/{seg}/", root.canonicalize().unwrap().display());
    conn.query_row(
        "SELECT COUNT(*) FROM items WHERE provider_key LIKE ?1",
        rusqlite::params![format!("{prefix}%")],
        |r| r.get(0),
    )
    .unwrap()
}

fn location() -> ufm_vfs::Location {
    ufm_vfs::Location {
        id: ufm_core::LocationId::new(),
        provider: "local".into(),
        display_name: "SubvolFixture".into(),
        capabilities: ufm_vfs::Capabilities {
            read: true,
            ..Default::default()
        },
        protected: ufm_vfs::ProtectionTier::None,
    }
}

fn assert_all_visible(catalog_path: &Path, root: &Path, phase: &str) {
    // Item-row counts under each subvolume (dirs + files):
    // @ = etc, etc/rootfsuniq.txt, usr, usr/bin, usr/bin/tool = 5
    // @home = richard, Downloads, NESMegaPack201808, ROMS,
    //         notesuniq.txt, 2 ROMs = 7
    // @factory = pkg, pkg/cacheuniq.bin = 2
    assert_eq!(count_items_under(catalog_path, root, "@"), 5, "{phase}: @ rows");
    assert_eq!(
        count_items_under(catalog_path, root, "@home"),
        7,
        "{phase}: @home rows — the subvolume must not be invisible"
    );
    assert_eq!(
        count_items_under(catalog_path, root, "@factory"),
        2,
        "{phase}: @factory rows"
    );
}

fn index_once(
    catalog_path: &Path,
    checkpoint_path: &Path,
    root: &Path,
    location: &ufm_vfs::Location,
) -> ufm_search::IndexStats {
    let mut ix = ufm_search::Indexer::open(catalog_path, checkpoint_path).unwrap();
    ix.index_root(root, location).unwrap()
}

#[test]
fn sibling_subvolumes_all_indexed() {
    let Ok(root) = std::env::var("UFM_TEST_BTRFS_ROOT") else {
        eprintln!("skipped: UFM_TEST_BTRFS_ROOT not set");
        return;
    };
    let root = PathBuf::from(root);
    let work = std::env::temp_dir().join(format!("ufm-subvol-walk-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let catalog_path = work.join("catalog.db");
    let checkpoint_path = work.join("checkpoint.json");
    // One location identity across all phases — the app persists
    // location ids (locations.json), and the catalog dedupes item
    // rows by (location_id, provider_key).
    let loc = location();

    // Phase 1 — fresh index: every file in every subvolume.
    // Fixture files: @ (rootfsuniq.txt, usr/bin/tool), @home
    // (notesuniq.txt + 2 NES ROMs), @factory (cacheuniq.bin) = 6.
    let stats = index_once(&catalog_path, &checkpoint_path, &root, &loc);
    assert_eq!(stats.files_done, 6, "phase 1: all fixture files indexed");
    assert_all_visible(&catalog_path, &root, "phase 1 (fresh)");

    // Phase 2 — re-index over the completed checkpoint (a fresh
    // re-walk by design): totals identical, nothing pruned away.
    let stats = index_once(&catalog_path, &checkpoint_path, &root, &loc);
    assert_eq!(stats.files_done, 6, "phase 2: re-walk sees every file again");
    assert_all_visible(&catalog_path, &root, "phase 2 (re-index)");
    // And search must not ghost duplicates after the re-walk
    // (external-content FTS rowid churn, fixed beside this gate).
    let cat = ufm_catalog::Catalog::open(&catalog_path).unwrap();
    let hits = cat.search_fts("rootfsuniq*", 10).unwrap();
    assert_eq!(hits.len(), 1, "phase 2: exactly one FTS hit, no ghosts");
    assert_eq!(hits[0].0.name, "rootfsuniq.txt");

    // Phase 3 — resume from an interrupted run that had completed
    // ONLY the `@` subtree: the checkpoint names @'s dirs as done
    // (run_complete = false). The resume must prune exactly @ and
    // index the siblings in full.
    let canon = root.canonicalize().unwrap();
    let at = canon.join("@");
    let completed: Vec<String> = [
        canon.to_string_lossy().into_owned(),
        at.to_string_lossy().into_owned(),
        at.join("etc").to_string_lossy().into_owned(),
        at.join("usr").to_string_lossy().into_owned(),
        at.join("usr/bin").to_string_lossy().into_owned(),
    ]
    .into();
    let ck = serde_json::json!({
        "version": 2,
        "completed_dirs": completed,
        "files_done": 2,
        "bytes_done": 12,
        "run_complete": false,
    });
    std::fs::write(&checkpoint_path, serde_json::to_string_pretty(&ck).unwrap()).unwrap();
    let stats = index_once(&catalog_path, &checkpoint_path, &root, &loc);
    assert_eq!(
        stats.files_done, 4,
        "phase 3: resume indexes exactly the non-@ files"
    );
    assert_all_visible(&catalog_path, &root, "phase 3 (resume)");

    let _ = std::fs::remove_dir_all(&work);
}
