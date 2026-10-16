//! The resumable X1-class background indexer.
//!
//! [`Indexer`] walks a location root with `walkdir` and records every entry
//! in the catalog via [`ufm_catalog::Catalog`]. Progress is checkpointed to a
//! JSON sidecar file after every 25 completed directories and at the end of
//! the run; a later [`Indexer`] opened on the same catalog + checkpoint
//! prunes already-completed subtrees and never re-indexes them.
//!
//! Resume contract (STANDING ORDER — resumable from the first version):
//! - A directory counts as *completed* only after its entire subtree has
//!   been visited (tracked by walk depth, not by pre-order insertion), so a
//!   crash can never mark a half-walked directory done.
//! - The checkpoint is written atomically: JSON goes to
//!   `<path>.tmp` (fsync), then `rename` commits it. A crash can only ever
//!   leave the *previous* good checkpoint behind.
//! - One checkpoint file per indexed root. Re-running `index_root` on a
//!   fully-indexed root is a no-op returning zeroed stats.
//! - Unreadable directories, vanished files, and symlinks never abort the
//!   run: they are skipped (symlinks are recorded as `Symlink`, never
//!   followed).
//!
//! Indexer v2 (Library Memory): a re-walk is INCREMENTAL. Before
//! walking, the indexer loads the catalog's own manifest for the
//! location (provider_key -> size + modified) and diffs every entry
//! against it: an entry whose size and mtime match is verified in
//! place — no catalog write, no FTS churn — and counted in
//! `files_unchanged`. The manifest derives from the catalog itself
//! (the source of truth), never from the checkpoint file, so a lost
//! or poisoned checkpoint can only cost time, never correctness:
//! worst case the diff finds nothing and the run degenerates to a
//! full re-walk. Deletions reconcile via `delete_unseen` after any
//! complete (unpruned) traversal, exactly as before.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ufm_core::{Result, UfmError};
use ufm_vfs::Location;

/// Per-run totals returned by [`Indexer::index_root`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IndexStats {
    /// Directories whose items were inserted during this run (root included).
    pub dirs_done: u64,
    /// Files whose items were inserted during this run.
    pub files_done: u64,
    /// Sum of `size_bytes` of files inserted during this run.
    pub bytes_done: u64,
    /// Indexer v2 (incremental): entries — files and directories —
    /// whose size + mtime already matched the catalog manifest and
    /// were verified WITHOUT a catalog write.
    pub files_unchanged: u64,
    /// Indexer v2: of the entries written this run, how many
    /// replaced an existing catalog row (the rest were brand new).
    pub files_updated: u64,
    /// Indexer v2: catalog rows removed by the post-walk reconcile
    /// (entries that no longer exist on disk).
    pub files_removed: u64,
}

/// On-disk checkpoint. `completed_dirs` holds canonical absolute paths of
/// fully-indexed directories, sorted for stable output; `files_done` /
/// `bytes_done` are cumulative across runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Checkpoint {
    version: u32,
    completed_dirs: Vec<String>,
    files_done: u64,
    bytes_done: u64,
    /// True when the run that wrote this checkpoint walked the whole root.
    /// A completed run must NOT prune the next run: files added since
    /// would never be indexed. Completion only licenses a fresh re-walk.
    #[serde(default)]
    run_complete: bool,
}

const CHECKPOINT_VERSION: u32 = 2;
/// Checkpoint cadence: persist after this many newly-completed directories.
const CHECKPOINT_EVERY_DIRS: u64 = 25;

/// Minimum wall-clock gap between non-final checkpoint writes
/// (freeze fix, 2026-10-16). The cadence above fires every 25
/// completed dirs, but each write re-serializes the ENTIRE
/// completed-dirs set — on Dickles' Home walk (59,309 dirs) that was
/// ~2,372 ever-larger pretty-JSON serializations (quadratic total
/// work, the perf-proven hot spot that saturated the app while
/// indexing: `Vec::from_iter` + free churn in this crate). Writes are
/// now time-throttled and compact; a crash loses at most this many
/// seconds of dir completions, and the resume re-walk is idempotent.
const CHECKPOINT_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Progress reports during [`Indexer::index_root`]: invoked with cumulative
/// stats on checkpoint cadence. Must be cheap — it runs on the indexing thread.
pub type ProgressCallback = Box<dyn Fn(IndexStats) + Send + 'static>;

pub struct Indexer {
    catalog: ufm_catalog::Catalog,
    checkpoint_path: PathBuf,
    completed_dirs: HashSet<String>,
    /// Roots THIS Indexer instance has fully walked. Only these may
    /// take the zeroed-stats fast path. A checkpoint loaded from
    /// disk must never fast-path a root: the writer can persist the
    /// root itself in a resumable (run_complete = false) checkpoint
    /// when a run dies during the final drain, and trusting it made
    /// a location permanently invisible — every later re-index
    /// returned instantly with zero stats while whole subtrees
    /// (btrfs sibling subvolumes on Dickles' superdog) had never
    /// been walked (P0-2, 2026-10-14).
    session_completed: HashSet<String>,
    /// Cumulative totals, restored from the checkpoint and re-persisted.
    files_done: u64,
    bytes_done: u64,
    /// Indexer v2 cumulative incremental counters (progress reports).
    files_unchanged: u64,
    files_updated: u64,
    files_removed: u64,
    progress: Option<ProgressCallback>,
    /// When the last non-final checkpoint actually hit disk (freeze
    /// fix: cadence writes inside the throttle window are skipped).
    last_checkpoint_write: Option<std::time::Instant>,
}

impl Indexer {
    /// Open (or create) the catalog at `catalog_path` and load the resume
    /// checkpoint at `checkpoint_path` when one exists.
    pub fn open(catalog_path: &Path, checkpoint_path: &Path) -> Result<Self> {
        let catalog = ufm_catalog::Catalog::open(catalog_path)?;
        let mut me = Self {
            catalog,
            checkpoint_path: checkpoint_path.to_path_buf(),
            completed_dirs: HashSet::new(),
            session_completed: HashSet::new(),
            files_done: 0,
            bytes_done: 0,
            files_unchanged: 0,
            files_updated: 0,
            files_removed: 0,
            progress: None,
            last_checkpoint_write: None,
        };
        me.load_checkpoint()?;
        Ok(me)
    }

    /// Install a progress callback invoked with cumulative stats on each
    /// checkpoint write during `index_root`.
    pub fn set_progress_callback(&mut self, cb: ProgressCallback) {
        self.progress = Some(cb);
    }

    fn report_progress(&self) {
        if let Some(cb) = &self.progress {
            cb(IndexStats {
                dirs_done: self.completed_dirs.len() as u64,
                files_done: self.files_done,
                bytes_done: self.bytes_done,
                files_unchanged: self.files_unchanged,
                files_updated: self.files_updated,
                files_removed: self.files_removed,
            });
        }
    }

    /// Index `root` into the catalog under `location`. The location row is
    /// inserted first; the root directory itself is indexed as an Item named
    /// after the location's display name with `provider_key` = the canonical
    /// root path. Returns this run's totals; a root this Indexer already
    /// walked in this session returns zeroed stats without touching the
    /// filesystem beyond the root stat (a checkpoint on disk never earns
    /// that fast path — see `session_completed`).
    pub fn index_root(&mut self, root: &Path, location: &Location) -> Result<IndexStats> {
        self.catalog.insert_location(location)?;
        let canon = root.canonicalize().map_err(|e| UfmError::Provider {
            provider: "local".into(),
            message: format!("index root {}: {e}", root.display()),
        })?;
        let canon_s = canon.to_string_lossy().into_owned();

        // Session fast path: THIS Indexer already walked the whole
        // root. Loaded checkpoint state deliberately does NOT count
        // (see session_completed): it is a prune filter, never proof
        // of completion.
        if self.session_completed.contains(&canon_s) {
            return Ok(IndexStats::default());
        }

        let mut stats = IndexStats::default();
        let mut completed_this_run: u64 = 0;

        // Indexer v2: the incremental manifest — what the catalog
        // already knows about this location. A failed load degrades
        // to an empty manifest (a full re-walk), never to an error:
        // the diff is an optimization with a correctness floor.
        let manifest = self
            .catalog
            .manifest_for_location(&location.id)
            .unwrap_or_default();
        let entry_unchanged = |item: &ufm_vfs::Item| -> bool {
            match item
                .provider_key
                .as_ref()
                .and_then(|pk| manifest.get(pk))
            {
                Some((msize, mmodified)) => {
                    item.size_bytes.map(|s| s as i64) == *msize
                        && item.modified.map(|m| m.to_rfc3339()).as_deref()
                            == mmodified.as_deref()
                }
                None => false,
            }
        };
        let entry_known = |item: &ufm_vfs::Item| -> bool {
            item.provider_key
                .as_ref()
                .is_some_and(|pk| manifest.contains_key(pk))
        };

        // Index the root dir itself (name = location display name).
        let mut root_item =
            ufm_vfs::item_from_path(location.id, &canon).ok_or_else(|| UfmError::Provider {
                provider: "local".into(),
                message: format!("cannot stat index root {}", canon.display()),
            })?;
        root_item.name = location.display_name.clone();
        // The manifest comparison is size + mtime only, so the
        // display-name substitution above never causes a spurious
        // rewrite: an unchanged root skips its insert and keeps the
        // stored display name.
        if entry_unchanged(&root_item) {
            stats.files_unchanged += 1;
            self.files_unchanged += 1;
        } else {
            // The root is a directory: a changed root is re-inserted
            // silently (files_updated counts files, not dirs).
            self.catalog.insert_item(&root_item)?;
        }
        stats.dirs_done += 1;

        // Snapshot for the prune closure: it cannot borrow `self` while the
        // loop below needs `&mut self`. Only prior-run completions matter
        // for pruning — this run's dirs are visited exactly once anyway.
        let done_snapshot: HashSet<String> = self.completed_dirs.clone();
        // A fresh walk (nothing pruned) sees every file: afterwards we can
        // drop catalog rows for files that no longer exist. A resumed walk
        // skips completed subtrees, so its seen-set would be a lie.
        let fresh_walk = done_snapshot.is_empty();
        let mut seen: HashSet<String> = HashSet::new();
        if fresh_walk {
            seen.insert(canon_s.clone());
        }

        // (canonical path, walk depth) of dirs whose subtree is in progress.
        // A dir is complete once walk depth drops back below its own depth.
        let mut dir_stack: Vec<(String, usize)> = vec![(canon_s.clone(), 0)];

        let walker = walkdir::WalkDir::new(&canon)
            .follow_links(false)
            .into_iter()
            .filter_entry(move |e| {
                if e.depth() == 0 {
                    return true; // root: handled above, never pruned
                }
                if !e.file_type().is_dir() {
                    return true;
                }
                // Prune already-completed subtrees on resume.
                !done_snapshot.contains(e.path().to_string_lossy().as_ref())
            });

        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue, // unreadable dir or entry: skip, keep going
            };
            if entry.depth() == 0 {
                continue; // root already indexed above
            }
            let depth = entry.depth();
            // Every stacked dir at this depth or deeper has had its whole
            // subtree yielded: mark complete (checkpoint on cadence).
            while dir_stack.last().is_some_and(|(_, d)| *d >= depth) {
                let (done, _) = dir_stack.pop().unwrap();
                self.completed_dirs.insert(done);
                completed_this_run += 1;
                if completed_this_run % CHECKPOINT_EVERY_DIRS == 0 {
                    self.write_checkpoint(false)?;
                    self.report_progress();
                }
            }
            let path = entry.path();
            let is_dir = entry.file_type().is_dir();
            match ufm_vfs::item_from_path(location.id, path) {
                Some(item) => {
                    // The seen-set is populated on every walk now
                    // (the incremental diff needs it for accounting),
                    // but delete_unseen below still only runs on a
                    // complete traversal (fresh_walk).
                    if let Some(pk) = &item.provider_key {
                        seen.insert(pk.clone());
                    }
                    if entry_unchanged(&item) {
                        // Verified against the catalog manifest:
                        // no write, no FTS churn.
                        stats.files_unchanged += 1;
                        self.files_unchanged += 1;
                        if is_dir {
                            dir_stack.push((path.to_string_lossy().into_owned(), depth));
                            stats.dirs_done += 1;
                        }
                        continue;
                    }
                    if entry_known(&item) && !is_dir {
                        stats.files_updated += 1;
                        self.files_updated += 1;
                    }
                    self.catalog.insert_item(&item)?;
                    if is_dir {
                        dir_stack.push((path.to_string_lossy().into_owned(), depth));
                        stats.dirs_done += 1;
                    } else {
                        // Files and symlinks: symlinks are recorded, never followed.
                        stats.files_done += 1;
                        self.files_done += 1;
                        if let Some(b) = item.size_bytes {
                            stats.bytes_done += b;
                            self.bytes_done += b;
                        }
                    }
                }
                None => continue, // vanished mid-walk or unreadable: skip
            }
        }

        // Drain: everything still stacked is fully visited now.
        // Deliberately NO cadence checkpoints here (P0-2): the drain
        // pops the root itself last, and a resumable checkpoint
        // written mid-drain could persist the root as "completed"
        // if the process dies in this window — the exact poison
        // that hid superdog's sibling subvolumes. A crash here just
        // leaves the previous mid-walk checkpoint behind, and the
        // resume re-walks the tail.
        while let Some((done, _)) = dir_stack.pop() {
            self.completed_dirs.insert(done);
            completed_this_run += 1;
        }
        if fresh_walk {
            // The walk saw everything: rows for vanished files go away.
            let removed = self.catalog.delete_unseen(&location.id, &seen)?;
            stats.files_removed = removed;
            self.files_removed += removed;
        }
        self.write_checkpoint(true)?;
        self.session_completed.insert(canon_s);
        self.report_progress();
        Ok(stats)
    }

    /// Delete the checkpoint file. The catalog rows are untouched; the next
    /// `index_root` re-walks from scratch (inserts are idempotent).
    ///
    /// Note: this only removes the file — this `Indexer`'s in-memory
    /// progress is unchanged. Drop it (or open a new one) to actually
    /// re-index from scratch.
    pub fn reset(&self) -> Result<()> {
        match std::fs::remove_file(&self.checkpoint_path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(UfmError::Database(format!("reset checkpoint: {e}"))),
        }
    }

    fn load_checkpoint(&mut self) -> Result<()> {
        let raw = match std::fs::read(&self.checkpoint_path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                return Err(UfmError::Database(format!(
                    "read checkpoint {}: {e}",
                    self.checkpoint_path.display()
                )))
            }
        };
        // A corrupt checkpoint can only come from disk damage (writes are
        // atomic via rename): start fresh rather than refuse to index.
        let ck: Checkpoint = match serde_json::from_slice(&raw) {
            Ok(ck) => ck,
            Err(_) => return Ok(()),
        };
        if ck.version != CHECKPOINT_VERSION {
            // Older checkpoint format: ignore it and walk fresh rather
            // than refuse to index.
            return Ok(());
        }
        if ck.run_complete {
            // The last run finished the whole root. Pruning from it would
            // hide every file added since — start a fresh walk instead
            // (completed_dirs stays empty, totals reset).
            return Ok(());
        }
        // Interrupted run: resume, pruning the subtrees it completed.
        self.completed_dirs = ck.completed_dirs.into_iter().collect();
        self.files_done = ck.files_done;
        self.bytes_done = ck.bytes_done;
        Ok(())
    }

    /// Atomic checkpoint write: serialize → `<path>.tmp` + fsync → rename.
    /// Readers only ever see the previous complete checkpoint or the new one.
    ///
    /// Freeze fix (2026-10-16): non-final writes are throttled to one
    /// per [`CHECKPOINT_MIN_INTERVAL`] and the JSON is compact. The
    /// old shape — full collect + sort + PRETTY serialization of the
    /// whole completed-dirs set every 25 dirs — was quadratic in the
    /// size of the walk and burned a full core for the entire pass
    /// (59k dirs → a 6.3 MB document re-serialized ~2,400 times),
    /// starving the UI that shares the process. The final write
    /// (run_complete) always lands.
    fn write_checkpoint(&mut self, run_complete: bool) -> Result<()> {
        if !run_complete {
            if let Some(last) = self.last_checkpoint_write {
                if last.elapsed() < CHECKPOINT_MIN_INTERVAL {
                    return Ok(());
                }
            }
        }
        let mut dirs: Vec<String> = self.completed_dirs.iter().cloned().collect();
        dirs.sort();
        let ck = Checkpoint {
            version: CHECKPOINT_VERSION,
            completed_dirs: dirs,
            files_done: self.files_done,
            bytes_done: self.bytes_done,
            run_complete,
        };
        let json = serde_json::to_string(&ck)
            .map_err(|e| UfmError::Database(format!("serialize checkpoint: {e}")))?;
        self.last_checkpoint_write = Some(std::time::Instant::now());
        let tmp = self.checkpoint_path.with_extension("tmp");
        if let Some(parent) = tmp.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| UfmError::Database(format!("checkpoint dir: {e}")))?;
            }
        }
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp)
                .map_err(|e| UfmError::Database(format!("write checkpoint: {e}")))?;
            f.write_all(json.as_bytes())
                .map_err(|e| UfmError::Database(format!("write checkpoint: {e}")))?;
            f.sync_all()
                .map_err(|e| UfmError::Database(format!("fsync checkpoint: {e}")))?;
        }
        std::fs::rename(&tmp, &self.checkpoint_path)
            .map_err(|e| UfmError::Database(format!("commit checkpoint: {e}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ufm-indexer-test-{}-{}-{}",
            tag,
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Builds: root/{a/{f1.txt (3B), f2.txt (5B)}, b/{f3.bin (7B)}, top.md (4B)}
    /// → 3 dirs (root, a, b) + 4 files, 19 bytes.
    fn fixture_tree() -> PathBuf {
        let root = scratch_dir("tree");
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(root.join("b")).unwrap();
        std::fs::write(root.join("a").join("f1.txt"), b"aaa").unwrap();
        std::fs::write(root.join("a").join("f2.txt"), b"aaaaa").unwrap();
        std::fs::write(root.join("b").join("f3.bin"), b"bbbbbbb").unwrap();
        std::fs::write(root.join("top.md"), b"cccc").unwrap();
        root
    }

    fn test_location() -> Location {
        Location {
            id: ufm_core::LocationId::new(),
            provider: "local".into(),
            display_name: "Fixture".into(),
            capabilities: ufm_vfs::Capabilities {
                read: true,
                ..Default::default()
            },
            protected: ufm_vfs::ProtectionTier::None,
        }
    }

    #[test]
    fn progress_callback_reports_cumulative_stats() {
        let work = scratch_dir("progress");
        let root = fixture_tree();
        let catalog_path = work.join("catalog.db");
        let checkpoint_path = work.join("checkpoint.json");
        let location = test_location();

        let seen: std::sync::Arc<std::sync::Mutex<Vec<IndexStats>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_cb = seen.clone();
        let stats = {
            let mut ix = Indexer::open(&catalog_path, &checkpoint_path).unwrap();
            ix.set_progress_callback(Box::new(move |s| {
                seen_cb.lock().unwrap().push(s);
            }));
            ix.index_root(&root, &location).unwrap()
        };
        let seen = seen.lock().unwrap();
        assert!(!seen.is_empty(), "progress callback never fired");
        let last = seen.last().unwrap();
        // Fresh index: cumulative totals equal this run's totals.
        assert_eq!(last.files_done, stats.files_done);
        assert_eq!(last.dirs_done, stats.dirs_done);
        for w in seen.windows(2) {
            assert!(
                w[1].files_done >= w[0].files_done,
                "progress must be monotonic"
            );
        }
    }

    #[test]
    fn index_then_reopen_rewalks_without_duplicates() {
        let work = scratch_dir("resume");
        let root = fixture_tree();
        let catalog_path = work.join("catalog.db");
        let checkpoint_path = work.join("checkpoint.json");
        let location = test_location();

        // First run: full index.
        let s1 = {
            let mut ix = Indexer::open(&catalog_path, &checkpoint_path).unwrap();
            ix.index_root(&root, &location).unwrap()
        };
        assert_eq!(s1.dirs_done, 3, "root + a + b");
        assert_eq!(s1.files_done, 4);
        assert_eq!(s1.bytes_done, 19);

        // Checkpoint exists, is valid JSON, and names every completed dir.
        let raw = std::fs::read_to_string(&checkpoint_path).unwrap();
        let ck: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(ck["version"], 2);
        assert_eq!(ck["run_complete"], true);
        assert_eq!(ck["files_done"], 4);
        assert_eq!(ck["bytes_done"], 19);
        let dirs = ck["completed_dirs"].as_array().unwrap();
        assert_eq!(dirs.len(), 3);

        // Second run on a NEW Indexer, same paths: the first run
        // COMPLETED, so this is a fresh re-walk — but Indexer v2
        // makes it INCREMENTAL: every entry diffs clean against the
        // catalog manifest, so nothing is rewritten (that is what
        // makes the re-walk cheap), the traversal still visits
        // everything (dirs_done matches), and the catalog stays
        // deduped. Files added since the first run must still be
        // picked up — see incremental_reindex_diffs_changes.
        let mut ix2 = Indexer::open(&catalog_path, &checkpoint_path).unwrap();
        let s2 = ix2.index_root(&root, &location).unwrap();
        assert_eq!(s2.dirs_done, s1.dirs_done, "the re-walk still traverses");
        assert_eq!(s2.files_done, 0, "nothing needed writing");
        assert_eq!(s2.bytes_done, 0);
        assert_eq!(s2.files_unchanged, 7, "root + a + b + 4 files verified in place");
        assert_eq!(s2.files_updated, 0);
        assert_eq!(s2.files_removed, 0);
        {
            let cat = ufm_catalog::Catalog::open(&catalog_path).unwrap();
            assert_eq!(cat.location_count().unwrap(), 1);
            assert_eq!(
                cat.manifest_for_location(&location.id).unwrap().len(),
                7,
                "no duplicates after the incremental re-walk"
            );
        }

        // Third run on the SAME Indexer: within one session a root is
        // walked once — the fast path still returns zeroed stats.
        let s3 = ix2.index_root(&root, &location).unwrap();
        assert_eq!(s3, IndexStats::default());

        std::fs::remove_dir_all(&work).ok();
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn incremental_reindex_diffs_changes() {
        let work = scratch_dir("incremental");
        let root = fixture_tree();
        let catalog_path = work.join("catalog.db");
        let checkpoint_path = work.join("checkpoint.json");
        let location = test_location();

        {
            let mut ix = Indexer::open(&catalog_path, &checkpoint_path).unwrap();
            let s1 = ix.index_root(&root, &location).unwrap();
            assert_eq!(s1.files_done, 4);
            assert_eq!(s1.files_unchanged, 0, "first index of a volume is full");
        }

        // Mutate the tree: one file changed (different size), one
        // added, one deleted.
        std::fs::write(root.join("a").join("f1.txt"), b"aaa-CHANGED-longer").unwrap();
        std::fs::write(root.join("a").join("f4.txt"), b"new!").unwrap();
        std::fs::remove_file(root.join("top.md")).unwrap();

        let s2 = {
            let mut ix = Indexer::open(&catalog_path, &checkpoint_path).unwrap();
            ix.index_root(&root, &location).unwrap()
        };
        assert_eq!(s2.files_updated, 1, "f1.txt changed size");
        assert_eq!(s2.files_done, 2, "the changed file + the new file were written");
        assert_eq!(s2.files_removed, 1, "top.md vanished from disk");
        // Unchanged: b + f2 + f3. The root and a/ are re-inserted
        // silently — adding/removing entries moved their mtimes,
        // which is correct (their contents did change).
        assert_eq!(s2.files_unchanged, 3, "b + f2 + f3 verified in place");

        // The catalog agrees with the disk afterwards.
        let cat = ufm_catalog::Catalog::open(&catalog_path).unwrap();
        assert!(cat.search_fts("top", 10).unwrap().is_empty());
        assert_eq!(cat.search_fts("f4", 10).unwrap().len(), 1);
        let manifest = cat.manifest_for_location(&location.id).unwrap();
        assert_eq!(manifest.len(), 7, "7 entries: 3 dirs + f1, f2, f3, f4");
        let f1_key = root
            .canonicalize()
            .unwrap()
            .join("a")
            .join("f1.txt")
            .to_string_lossy()
            .into_owned();
        assert_eq!(manifest[&f1_key].0, Some(18), "f1's new size is catalogued");

        std::fs::remove_dir_all(&work).ok();
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn reset_clears_checkpoint_and_reindex_is_idempotent() {
        let work = scratch_dir("reset");
        let root = fixture_tree();
        let catalog_path = work.join("catalog.db");
        let checkpoint_path = work.join("checkpoint.json");
        let location = test_location();

        let mut ix = Indexer::open(&catalog_path, &checkpoint_path).unwrap();
        let s1 = ix.index_root(&root, &location).unwrap();
        assert!(checkpoint_path.exists());
        ix.reset().unwrap();
        assert!(!checkpoint_path.exists());
        // Reset twice is fine.
        ix.reset().unwrap();
        drop(ix);

        // A fresh Indexer after reset re-walks from scratch. The
        // CATALOG survives a checkpoint reset, so Indexer v2's
        // manifest diff verifies every entry in place (identical
        // traversal totals, zero writes) and the catalog stays
        // deduplicated — the idempotency this test exists to prove.
        let mut ix2 = Indexer::open(&catalog_path, &checkpoint_path).unwrap();
        let s2 = ix2.index_root(&root, &location).unwrap();
        assert_eq!(s2.dirs_done, s1.dirs_done);
        assert_eq!(s2.files_done, 0);
        assert_eq!(s2.files_unchanged, 7);
        let hits = ix2.catalog.search_fts("f1*", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0.name, "f1.txt");

        std::fs::remove_dir_all(&work).ok();
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn handles_symlinks_and_missing_root() {
        let work = scratch_dir("edge");
        let root = scratch_dir("edge-tree");
        std::fs::write(root.join("real.txt"), b"hi").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("real.txt"), root.join("link.txt")).unwrap();
        let catalog_path = work.join("catalog.db");
        let checkpoint_path = work.join("checkpoint.json");
        let location = test_location();

        let mut ix = Indexer::open(&catalog_path, &checkpoint_path).unwrap();
        let stats = ix.index_root(&root, &location).unwrap();
        assert_eq!(stats.dirs_done, 1);
        #[cfg(unix)]
        assert_eq!(stats.files_done, 2, "real file + symlink recorded, not followed");
        #[cfg(not(unix))]
        assert_eq!(stats.files_done, 1);

        // Missing root → clean provider error, not a panic.
        let err = ix
            .index_root(&root.join("does-not-exist"), &location)
            .unwrap_err();
        assert!(err.to_string().contains("index root"));

        std::fs::remove_dir_all(&work).ok();
        std::fs::remove_dir_all(&root).ok();
    }

    /// Freeze fix (2026-10-16): cadence checkpoint writes inside the
    /// throttle window must NOT touch disk — the pre-fix shape
    /// re-serialized the entire completed-dirs set every 25 dirs
    /// (quadratic; the indexing-freeze hot spot). The final write
    /// always lands, and the on-disk format is compact JSON.
    #[test]
    fn checkpoint_writes_are_time_throttled_and_compact() {
        let work = scratch_dir("throttle");
        let catalog_path = work.join("catalog.db");
        let checkpoint_path = work.join("checkpoint.json");
        let mut ix = Indexer::open(&catalog_path, &checkpoint_path).unwrap();
        ix.completed_dirs.insert("/a".to_string());
        ix.completed_dirs.insert("/b".to_string());
        ix.write_checkpoint(false).unwrap();
        let first = std::fs::read_to_string(&checkpoint_path).unwrap();
        assert!(first.contains("\"/a\"") && first.contains("\"/b\""));
        assert!(
            !first.contains('\n'),
            "checkpoint must be compact JSON, got pretty output"
        );
        // Inside the throttle window: a cadence write is skipped whole.
        ix.completed_dirs.insert("/c".to_string());
        ix.write_checkpoint(false).unwrap();
        let second = std::fs::read_to_string(&checkpoint_path).unwrap();
        assert_eq!(first, second, "throttled write must not touch disk");
        // The final write always lands, throttle or not.
        ix.write_checkpoint(true).unwrap();
        let third = std::fs::read_to_string(&checkpoint_path).unwrap();
        assert!(third.contains("\"/c\""));
        assert!(third.contains("\"run_complete\":true"));
        std::fs::remove_dir_all(&work).ok();
    }
}
