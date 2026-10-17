//! Safety-gate proofs (Hyperbase Solution #3) against a scaled
//! replica of the 12 TB drive, per ~/workspace/hyperbase-review/
//! CENSUS.md:
//!
//! - two "PC games" of exactly 26,000 files each (the census's
//!   measured per-title count) — asset bulk, a handful of candidates
//!   at most by accident of extension;
//! - a RetroBat-shaped tree: roms/<system>/ mixing candidate ROMs
//!   with the scraped artwork that dominates the real tree's file
//!   count (48,766 PNGs there), a bios/ folder, ES config files;
//! - a small plain folder (below the gate threshold);
//! - one loose file at the drive root.
//!
//! The candidate set is the PRODUCTION list
//! (`ufm_media::romdat::candidate_extensions`), so "the filter
//! admits exactly the candidate set" is a statement about the
//! shipped wiring, not about a test-local copy.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use ufm_search::{census_root, IndexScope, Indexer};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ufm-safety-gate-{}-{}-{}",
        tag,
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn production_candidates() -> HashSet<String> {
    ufm_media::romdat::candidate_extensions()
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Fixture dimensions, named so the expected sets below are
/// arithmetic on construction constants, never on the code under
/// test.
mod dim {
    pub const GAME_FILES: usize = 26_000; // per PC game, CENSUS.md's measured title
    pub const NES_ZIP: usize = 120;
    pub const NES_PNG: usize = 240; // scraped boxart — on the real tree PNGs (48,766) outnumber ZIPs (37,756)
    pub const NES_MP4: usize = 10; // snap videos
    pub const SNES_SFC: usize = 80;
    pub const SNES_PNG: usize = 80;
    pub const PSX_CHD: usize = 30;
    pub const PSX_JPG: usize = 30;
    pub const GBA_ZIP: usize = 60;
    pub const GBA_TXT: usize = 5;
    pub const BIOS_BIN: usize = 12; // ".bin" IS a candidate extension
    pub const ES_FILES: usize = 20; // .xml/.cfg — never candidates
    pub const PLAIN_FILES: usize = 40;
}

fn write_files(dir: &Path, prefix: &str, ext: &str, n: usize, body: &[u8]) {
    std::fs::create_dir_all(dir).unwrap();
    for i in 0..n {
        std::fs::write(dir.join(format!("{prefix}{i:05}.{ext}")), body).unwrap();
    }
}

/// One PC game: GAME_FILES files spread over subdirectories,
/// asset extensions only (`.pak`/`.dat`/`.tex` — none are ROM
/// candidates), plus an .exe at its root.
fn build_game(root: &Path, name: &str) {
    let game = root.join(name);
    let per_dir = 500;
    let dirs = dim::GAME_FILES / per_dir;
    for d in 0..dirs {
        write_files(
            &game.join(format!("data{d:03}")),
            "asset",
            "pak",
            per_dir,
            b"x",
        );
    }
    std::fs::write(game.join(format!("{name}.exe")), b"MZ").unwrap();
}

/// The RetroBat-shaped tree. Returns nothing; expectations are
/// computed from `dim`.
fn build_retrobat(root: &Path) {
    let rb = root.join("retrobat");
    write_files(&rb.join("roms").join("nes"), "game", "zip", dim::NES_ZIP, b"rom");
    write_files(&rb.join("roms").join("nes"), "box", "png", dim::NES_PNG, b"art");
    write_files(&rb.join("roms").join("nes"), "snap", "mp4", dim::NES_MP4, b"vid");
    write_files(&rb.join("roms").join("snes"), "game", "sfc", dim::SNES_SFC, b"rom");
    write_files(&rb.join("roms").join("snes"), "box", "png", dim::SNES_PNG, b"art");
    write_files(&rb.join("roms").join("psx"), "game", "chd", dim::PSX_CHD, b"rom");
    write_files(&rb.join("roms").join("psx"), "box", "jpg", dim::PSX_JPG, b"art");
    write_files(&rb.join("roms").join("gba"), "game", "zip", dim::GBA_ZIP, b"rom");
    write_files(&rb.join("roms").join("gba"), "notes", "txt", dim::GBA_TXT, b"txt");
    write_files(&rb.join("bios"), "scph", "bin", dim::BIOS_BIN, b"bios");
    write_files(&rb.join("emulationstation"), "cfg", "xml", dim::ES_FILES, b"<x/>");
}

/// The replica drive, built once per test-binary run (indexing is
/// read-only on the tree, so tests may share it).
fn drive() -> &'static Path {
    static DRIVE: OnceLock<PathBuf> = OnceLock::new();
    DRIVE.get_or_init(|| {
        let root = scratch("drive");
        build_game(&root, "GameOne");
        build_game(&root, "GameTwo");
        build_retrobat(&root);
        write_files(&root.join("plain"), "doc", "md", dim::PLAIN_FILES, b"doc");
        std::fs::write(root.join("loose.txt"), b"loose").unwrap();
        root
    })
}

fn test_location() -> ufm_vfs::Location {
    ufm_vfs::Location {
        id: ufm_core::LocationId::new(),
        provider: "local".into(),
        display_name: "Replica".into(),
        capabilities: ufm_vfs::Capabilities {
            read: true,
            ..Default::default()
        },
        protected: ufm_vfs::ProtectionTier::None,
    }
}

/// Independent ground truth: a naive recursive recount written
/// against `std::fs` only — deliberately NOT the walkdir machinery
/// the census uses, so the comparison means something.
fn naive_recount(root: &Path) -> (u64, u64, u64) {
    let (mut files, mut dirs, mut bytes) = (0u64, 0u64, 0u64);
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let ft = entry.file_type().unwrap();
            if ft.is_dir() {
                dirs += 1;
                stack.push(entry.path());
            } else {
                files += 1;
                if !ft.is_symlink() {
                    bytes += entry.metadata().unwrap().len();
                }
            }
        }
    }
    (files, dirs, bytes)
}

/// The catalog's provider keys for a location, read through a
/// fresh Catalog handle (the Indexer's own is private).
fn manifest_keys(catalog_path: &Path, location: &ufm_vfs::Location) -> HashSet<String> {
    let cat = ufm_catalog::Catalog::open(catalog_path).unwrap();
    cat.manifest_for_location(&location.id)
        .unwrap()
        .keys()
        .cloned()
        .collect()
}

fn item_count(catalog_path: &Path) -> i64 {
    let conn = rusqlite::Connection::open(catalog_path).unwrap();
    conn.query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
        .unwrap()
}

/// Expected provider keys for a walk of `base` admitting exactly
/// the paths for which `admit(rel_components, is_dir)` holds, plus
/// the root item itself. Built from the fixture on disk with the
/// naive walker, so expectations track construction, not the
/// indexer's logic.
fn expected_keys(base: &Path, admit: &dyn Fn(&[String], bool) -> bool) -> HashSet<String> {
    let canon = base.canonicalize().unwrap();
    let mut out: HashSet<String> = HashSet::new();
    out.insert(canon.to_string_lossy().into_owned()); // the root item
    let mut stack: Vec<(PathBuf, Vec<String>)> = vec![(canon.clone(), Vec::new())];
    while let Some((dir, comps)) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let is_dir = entry.file_type().unwrap().is_dir();
            let mut child = comps.clone();
            child.push(entry.file_name().to_string_lossy().into_owned());
            if !admit(&child, is_dir) {
                continue;
            }
            out.insert(entry.path().to_string_lossy().into_owned());
            if is_dir {
                stack.push((entry.path(), child));
            }
        }
    }
    out
}

fn index_with(
    work: &Path,
    root: &Path,
    location: &ufm_vfs::Location,
    scope: &IndexScope,
) -> (ufm_search::IndexStats, PathBuf) {
    let catalog_path = work.join("catalog.db");
    let checkpoint_path = work.join("checkpoint.json");
    let candidates = production_candidates();
    let mut ix = Indexer::open(&catalog_path, &checkpoint_path).unwrap();
    let stats = ix
        .index_root_scoped(root, location, scope, &candidates)
        .unwrap();
    (stats, catalog_path)
}

// ---------------------------------------------------------------------------
// 1. Census counts match ground truth; the gate trips where it must.
// ---------------------------------------------------------------------------

#[test]
fn census_matches_ground_truth_and_trips_the_gate() {
    let drive = drive();
    let candidates = production_candidates();
    let census = census_root(drive, &candidates).unwrap();
    let (files, dirs, bytes) = naive_recount(drive);
    assert_eq!(census.files, files, "file count == naive recount");
    assert_eq!(census.dirs, dirs, "dir count == naive recount");
    assert_eq!(census.bytes, bytes, "bytes == naive recount");

    // Per-top-entry truth for the two games and the ROM tree.
    let game_one = census.top.iter().find(|t| t.name == "GameOne").unwrap();
    assert_eq!(game_one.files as usize, dim::GAME_FILES + 1, "26k assets + the .exe");
    let rb = census.top.iter().find(|t| t.name == "retrobat").unwrap();
    let rb_files = dim::NES_ZIP
        + dim::NES_PNG
        + dim::NES_MP4
        + dim::SNES_SFC
        + dim::SNES_PNG
        + dim::PSX_CHD
        + dim::PSX_JPG
        + dim::GBA_ZIP
        + dim::GBA_TXT
        + dim::BIOS_BIN
        + dim::ES_FILES;
    assert_eq!(rb.files as usize, rb_files);
    // Candidates inside retrobat: every ROM + the BIOS .bin files
    // (extension candidacy is the filter's whole contract — the
    // identify pass, not the filter, decides they are not games).
    let rb_candidates =
        dim::NES_ZIP + dim::SNES_SFC + dim::PSX_CHD + dim::GBA_ZIP + dim::BIOS_BIN;
    assert_eq!(rb.candidate_files as usize, rb_candidates);

    // The gate: the whole drive trips it; the plain folder and the
    // (scaled) ROM tree alone do not.
    assert!(census.over_threshold(), "a 52k-file drive must trip the gate");
    assert!(census.total_items() > ufm_search::GATE_THRESHOLD_FILES);
    let plain = census_root(&drive.join("plain"), &candidates).unwrap();
    assert_eq!(plain.files as usize, dim::PLAIN_FILES);
    assert!(!plain.over_threshold());
    let rb_only = census_root(&drive.join("retrobat"), &candidates).unwrap();
    assert!(!rb_only.over_threshold(), "scaled replica tree stays under the line");
}

// ---------------------------------------------------------------------------
// 2. ROM focus admits exactly the candidate set (+ folder rows).
// ---------------------------------------------------------------------------

#[test]
fn rom_focus_indexes_exactly_the_candidate_set() {
    let work = scratch("romfocus");
    let rb_root = drive().join("retrobat");
    let location = test_location();
    let scope = IndexScope {
        rom_focus: true,
        ..IndexScope::ALL
    };
    let (stats, catalog_path) = index_with(&work, &rb_root, &location, &scope);

    let candidates = production_candidates();
    let expected = expected_keys(&rb_root, &|comps, is_dir| {
        if is_dir {
            return true; // folders are always cataloged
        }
        let name = comps.last().unwrap();
        ufm_search::census::is_candidate(name, &candidates)
    });
    let actual = manifest_keys(&catalog_path, &location);
    assert_eq!(actual, expected, "catalog == candidates + folders + root");
    // The artwork is NOT there, and the ROMs are — by name.
    assert!(actual.iter().any(|k| k.ends_with("game00000.zip")));
    assert!(actual.iter().any(|k| k.ends_with("game00000.chd")));
    assert!(!actual.iter().any(|k| k.ends_with(".png")));
    assert!(!actual.iter().any(|k| k.ends_with(".mp4")));
    assert_eq!(stats.files_done as usize, expected.len() - count_dirs(&rb_root) - 1);
    std::fs::remove_dir_all(&work).ok();
}

fn count_dirs(root: &Path) -> usize {
    let (mut dirs, mut stack) = (0usize, vec![root.to_path_buf()]);
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(d).unwrap() {
            let e = e.unwrap();
            if e.file_type().unwrap().is_dir() {
                dirs += 1;
                stack.push(e.path());
            }
        }
    }
    dirs
}

// ---------------------------------------------------------------------------
// 3. Folder pick maps to exactly the indexed set; narrowing the
//    scope and re-indexing reconciles the catalog to the new scope.
// ---------------------------------------------------------------------------

#[test]
fn folder_pick_maps_to_exactly_the_indexed_set() {
    let work = scratch("pick");
    let drive = drive();
    let location = test_location();
    let pick: HashSet<String> = ["retrobat".to_string(), "plain".to_string()]
        .into_iter()
        .collect();
    let scope = IndexScope {
        rom_focus: false,
        include_dirs: Some(pick),
        include_root_files: false,
    };
    let (_, catalog_path) = index_with(&work, drive, &location, &scope);
    let expected = expected_keys(drive, &|comps, _is_dir| {
        comps[0] == "retrobat" || comps[0] == "plain"
    });
    let actual = manifest_keys(&catalog_path, &location);
    assert_eq!(actual, expected, "picked subtrees only, root item included");
    assert!(!actual.iter().any(|k| k.contains("GameOne")));
    assert!(!actual.iter().any(|k| k.contains("GameTwo")));
    assert!(
        !actual.iter().any(|k| k.ends_with("loose.txt")),
        "root files ride on their own flag — off here"
    );

    // Narrow further (retrobat + ROM focus) and re-index: the
    // catalog must reconcile to exactly the new scope.
    let scope2 = IndexScope {
        rom_focus: true,
        include_dirs: Some(["retrobat".to_string()].into_iter().collect()),
        include_root_files: false,
    };
    let catalog_path2 = work.join("catalog.db");
    let checkpoint_path = work.join("checkpoint.json");
    let candidates = production_candidates();
    {
        let mut ix = Indexer::open(&catalog_path2, &checkpoint_path).unwrap();
        let s2 = ix
            .index_root_scoped(drive, &location, &scope2, &candidates)
            .unwrap();
        assert!(s2.files_removed > 0, "the plain tree's rows were reconciled away");
    }
    let expected2 = expected_keys(drive, &|comps, is_dir| {
        if comps[0] != "retrobat" {
            return false;
        }
        if is_dir {
            return true;
        }
        ufm_search::census::is_candidate(comps.last().unwrap(), &candidates)
    });
    let actual2 = manifest_keys(&catalog_path2, &location);
    assert_eq!(actual2, expected2, "reconciled to the narrowed scope exactly");
    std::fs::remove_dir_all(&work).ok();
}

// ---------------------------------------------------------------------------
// 4. Cancel at the census leaves the catalog byte-identical.
// ---------------------------------------------------------------------------

#[test]
fn census_is_read_only_for_the_catalog() {
    let work = scratch("cancel");
    let drive = drive();
    let location = test_location();
    // A pre-existing catalog with real content (the plain tree).
    let (_, catalog_path) = index_with(
        &work,
        &drive.join("plain"),
        &location,
        &IndexScope::ALL,
    );
    let bytes_before = std::fs::read(&catalog_path).unwrap();
    let items_before = item_count(&catalog_path);
    assert!(items_before > 0);

    // The user opens the add sheet for the whole drive… census…
    let census = census_root(drive, &production_candidates()).unwrap();
    assert!(census.over_threshold());
    // …and cancels. Nothing was registered or indexed: the census
    // was the only thing that ran, and it writes nothing anywhere.
    let bytes_after = std::fs::read(&catalog_path).unwrap();
    assert_eq!(items_before, item_count(&catalog_path));
    assert_eq!(
        bytes_before, bytes_after,
        "catalog file must be byte-identical after a cancelled census"
    );
    std::fs::remove_dir_all(&work).ok();
}

// ---------------------------------------------------------------------------
// 5. The production candidate list is the filter's list.
// ---------------------------------------------------------------------------

#[test]
fn production_candidate_list_drives_the_filter() {
    let candidates = production_candidates();
    // The formats the census found on the real drive.
    for ext in ["zip", "7z", "sfc", "smc", "nes", "gba", "chd", "iso", "cue", "adf", "z80", "tzx", "gcz"] {
        assert!(candidates.contains(ext), "{ext} must be a candidate");
    }
    // Switch containers nominate too (Hyperbase #2): no DAT exists
    // for them in the bundled set, so they are checked and honestly
    // match nothing — visible, never invented identity.
    for ext in ["nsp", "xci"] {
        assert!(candidates.contains(ext), "{ext} must be a candidate");
    }
    for ext in ["png", "jpg", "mp4", "txt", "xml", "exe", "pak"] {
        assert!(!candidates.contains(ext), "{ext} must NOT be a candidate");
    }
    // Hyperbase #2 (2026-10-17) widened the list as this tripwire
    // anticipated: GameCube .gcz is a candidate now and identifies
    // natively (crate::gcz). Wii .rvz remains a known gap — none
    // were on the census drive and no reader exists yet — so it
    // stays as the tripwire for that future build.
    assert!(candidates.contains("gcz"));
    assert!(!candidates.contains("rvz"));
    // Sizing: the RetroBat census's ROM share, reproduced on the
    // replica — candidates are a minority of the tree (artwork
    // dominates), which is the entire point of ROM focus.
    let census = census_root(&drive().join("retrobat"), &candidates).unwrap();
    assert!(census.candidate_files < census.files / 2);
}

// ---------------------------------------------------------------------------
// 6. Checkpoint scope tags: a changed scope discards the prune set;
//    the same scope keeps resume semantics.
// ---------------------------------------------------------------------------

fn plant_checkpoint(path: &Path, completed: &[String], tag: &str) {
    let json = serde_json::json!({
        "version": 2,
        "completed_dirs": completed,
        "files_done": 0,
        "bytes_done": 0,
        "run_complete": false,
        "scope_tag": tag,
    });
    std::fs::write(path, json.to_string()).unwrap();
}

#[test]
fn scope_tag_guards_checkpoint_resume() {
    let drive = drive();
    let candidates = production_candidates();
    let game_one = drive
        .join("GameOne")
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let pick_game: IndexScope = IndexScope {
        rom_focus: false,
        include_dirs: Some(["GameOne".to_string()].into_iter().collect()),
        include_root_files: true,
    };

    // Same tag: the planted "already completed" GameOne is trusted
    // (in production its rows are already in the catalog from the
    // interrupted run; the empty catalog here makes the trust
    // visible as absence).
    let work = scratch("tagmatch");
    let catalog_path = work.join("catalog.db");
    let checkpoint_path = work.join("checkpoint.json");
    plant_checkpoint(
        &checkpoint_path,
        std::slice::from_ref(&game_one),
        &pick_game.tag(&candidates),
    );
    let location = test_location();
    {
        let mut ix = Indexer::open(&catalog_path, &checkpoint_path).unwrap();
        ix.index_root_scoped(drive, &location, &pick_game, &candidates)
            .unwrap();
    }
    let keys = manifest_keys(&catalog_path, &location);
    assert!(
        !keys.iter().any(|k| k.contains("GameOne")),
        "matching tag: the completed subtree is pruned as before"
    );

    // Different tag: the same planted checkpoint must NOT prune —
    // the new scope's walk sees the whole tree.
    let work2 = scratch("tagmismatch");
    let catalog_path2 = work2.join("catalog.db");
    let checkpoint_path2 = work2.join("checkpoint.json");
    plant_checkpoint(
        &checkpoint_path2,
        std::slice::from_ref(&game_one),
        &pick_game.tag(&candidates),
    );
    let location2 = test_location();
    {
        let mut ix = Indexer::open(&catalog_path2, &checkpoint_path2).unwrap();
        ix.index_root_scoped(drive, &location2, &IndexScope::ALL, &candidates)
            .unwrap();
    }
    let keys2 = manifest_keys(&catalog_path2, &location2);
    assert!(
        keys2.iter().any(|k| k.contains("GameOne")),
        "mismatched tag: prune set discarded, subtree is walked"
    );
    // Sanity: the unscoped walk really did index the whole drive.
    let mut counts: HashMap<String, usize> = HashMap::new();
    for k in &keys2 {
        if let Some(top) = k
            .strip_prefix(&format!("{}/", drive.canonicalize().unwrap().to_string_lossy()))
            .and_then(|r| r.split('/').next())
        {
            *counts.entry(top.to_string()).or_default() += 1;
        }
    }
    assert!(counts["GameOne"] > dim::GAME_FILES);
    assert!(counts["retrobat"] > 100);
    std::fs::remove_dir_all(&work).ok();
    std::fs::remove_dir_all(&work2).ok();
}
