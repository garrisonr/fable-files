//! Dry-run census: the metadata-only walk behind the huge-drive
//! safety gate (Hyperbase Solution #3, 2026-10-16).
//!
//! Adding a location used to be a single click straight into a full
//! index walk. For a folder that is fine; for a 12 TB preloaded drive
//! (≥2.1M files, ~2M of them individual PC-game asset files) it would
//! roughly triple the catalog before the user saw a single number.
//! The census is the indexer's own first pass — same traversal
//! discipline (walkdir, symlinks never followed, unreadable entries
//! skipped and counted) — run BEFORE anything is registered or
//! indexed: counts only, `symlink_metadata` only, no file body is
//! ever read. Cancelling after a census therefore leaves the catalog
//! byte-identical: nothing was written anywhere.
//!
//! The breakdown is per top-level entry (each directory directly
//! under the root, plus one synthetic entry for files sitting at the
//! root itself) so the add-location sheet can offer the folder pick
//! with real counts on every row.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use ufm_core::{Result, UfmError};

/// The gate threshold: a location whose walk would catalog more
/// than this many entries must be previewed in the census sheet
/// before anything is indexed.
///
/// Reasoning (2026-10-16, from the measured library): Dickles'
/// entire catalog is 1,262,899 items accumulated over his whole
/// Home tree — the primary library root. 50,000 entries in a single
/// add is ≥4% catalog growth from one click: the line where an add
/// stops being routine. Ordinary folders never come near it (his
/// NES+SNES ROM folders total ~1.3k files), while the trees the
/// gate exists for sail past it: the RetroBat roms tree alone is
/// 95,745 files, and a single PC game can carry ~26k files, so any
/// two-game add trips it too. Below the threshold the add flow is
/// unchanged — no sheet, no extra click.
pub const GATE_THRESHOLD_FILES: u64 = 50_000;

/// Sequential bytes/second assumed for the identify (hash) estimate.
/// The identification pass reads candidate files end to end; on the
/// hardware this gate exists for (a spinning USB hard drive) the
/// disk's sequential read rate — not the hash — is the bottleneck,
/// and ~120 MB/s is what a 12 TB USB HDD sustains. Labeled an
/// estimate wherever it is shown.
pub const HASH_BYTES_PER_SEC: u64 = 120_000_000;

/// One top-level entry of the censused root: a directory directly
/// under the root (with its whole subtree folded in), or the
/// synthetic root-files bucket (`name == ""`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TopEntry {
    /// Directory name, or "" for files sitting at the root itself.
    pub name: String,
    /// All non-directory entries in the subtree (files + symlinks),
    /// matching the indexer's counting discipline.
    pub files: u64,
    /// Directories in the subtree, the entry itself included.
    pub dirs: u64,
    /// Sum of file sizes (metadata length, symlinks excluded —
    /// a symlink's target is another walk's business).
    pub bytes: u64,
    /// Files whose extension is a ROM-identification candidate.
    pub candidate_files: u64,
    /// Sum of candidate file sizes — what the identify pass reads.
    pub candidate_bytes: u64,
}

impl TopEntry {
    /// Catalog items this entry contributes with no filtering:
    /// every file plus every folder row.
    pub fn items(&self) -> u64 {
        self.files + self.dirs
    }
    /// Catalog items under ROM focus: candidate files only, but
    /// folders are still cataloged (the tree stays navigable).
    pub fn items_rom_focus(&self) -> u64 {
        self.candidate_files + self.dirs
    }
}

/// The full census of one root.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Census {
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    pub candidate_files: u64,
    pub candidate_bytes: u64,
    /// Per-top-level-entry breakdown, sorted by file count
    /// descending (ties by name) so the sheet leads with the trees
    /// that dominate the walk.
    pub top: Vec<TopEntry>,
    /// Entries the walk could not read (permissions, vanished
    /// mid-walk). Counted, never fatal — same as the indexer.
    pub errors: u64,
    /// Wall-clock of the walk itself. The indexing estimate is
    /// derived from it (see [`Census::est_index_secs`]).
    pub elapsed_ms: u64,
}

impl Census {
    /// Total catalog entries an unfiltered index of this root
    /// would add (files + folder rows).
    pub fn total_items(&self) -> u64 {
        self.files + self.dirs
    }

    /// True when this root must pass through the census sheet
    /// before it may be added.
    pub fn over_threshold(&self) -> bool {
        over_threshold(self.total_items())
    }

    /// Estimated seconds for the identify (hash) pass over the
    /// given candidate bytes: a plain sequential read of those
    /// bytes off the disk. See [`HASH_BYTES_PER_SEC`].
    pub fn est_identify_secs(candidate_bytes: u64) -> u64 {
        candidate_bytes.div_ceil(HASH_BYTES_PER_SEC)
    }

    /// Estimated seconds for the index walk over a selection of
    /// this census. The census walk is the indexer's own metadata
    /// pass; indexing repeats that walk and additionally writes a
    /// catalog row per item, so 2× the measured walk time (scaled
    /// by the selected fraction) is the honest planning number.
    /// The uncertainty is the disk's, and it is labeled an estimate
    /// wherever shown.
    pub fn est_index_secs(&self, selected_items: u64) -> u64 {
        let total = self.total_items();
        if total == 0 || self.elapsed_ms == 0 {
            return 0;
        }
        let walk_secs = self.elapsed_ms as f64 / 1000.0;
        (walk_secs * 2.0 * (selected_items as f64 / total as f64)).ceil() as u64
    }
}

/// The gate test, factored out so the boundary itself is provable
/// without walking fifty thousand files.
pub fn over_threshold(total_items: u64) -> bool {
    total_items > GATE_THRESHOLD_FILES
}

/// Extension of a file name under the identification pass's own
/// semantics: the catalog feeds identification with
/// `lower(name) LIKE '%.ext'`, i.e. a case-insensitive match on the
/// suffix after the last dot. `None` when the name has no usable
/// extension (no dot, or a leading-dot name like `.zip` alone).
pub fn extension_of(name: &str) -> Option<String> {
    let dot = name.rfind('.')?;
    if dot == 0 {
        return None;
    }
    let ext = &name[dot + 1..];
    if ext.is_empty() {
        return None;
    }
    Some(ext.to_lowercase())
}

/// Is this file name a ROM-identification candidate under
/// `candidates` (the lowercase candidate set built from
/// `ufm_media::romdat::candidate_extensions`)?
pub fn is_candidate(name: &str, candidates: &HashSet<String>) -> bool {
    match extension_of(name) {
        Some(ext) => candidates.contains(&ext),
        None => false,
    }
}

/// Walk `root` metadata-only and fold the census. Mirrors the
/// indexer: `walkdir`, symlinks never followed, errors skipped and
/// counted, hidden entries included (the indexer indexes them, so
/// the census must count them — the sheet's promise is that its
/// numbers are the walk's numbers).
pub fn census_root(root: &Path, candidates: &HashSet<String>) -> Result<Census> {
    let started = Instant::now();
    let canon = root.canonicalize().map_err(|e| UfmError::Provider {
        provider: "local".into(),
        message: format!("census root {}: {e}", root.display()),
    })?;
    let mut census = Census::default();
    // Depth-1 directory name -> index into census.top. Root files
    // fold into the synthetic "" entry.
    let mut top_index: HashMap<String, usize> = HashMap::new();
    let mut top_entry = |census: &mut Census, name: &str| -> usize {
        if let Some(&i) = top_index.get(name) {
            return i;
        }
        census.top.push(TopEntry {
            name: name.to_string(),
            ..Default::default()
        });
        let i = census.top.len() - 1;
        top_index.insert(name.to_string(), i);
        i
    };

    // Index scope law (2026-10-18, indexer::prunes_hidden_dir):
    // the census applies the SAME hidden-directory prune as the
    // indexer, so the counts it shows the safety gate are the
    // counts a walk would actually catalog — and the folder pick
    // it feeds can never offer a tree the walk would skip.
    let walker = walkdir::WalkDir::new(&canon)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            !(e.file_type().is_dir()
                && crate::indexer::prunes_hidden_dir(
                    e.depth(),
                    &e.file_name().to_string_lossy(),
                ))
        });
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                census.errors += 1;
                continue;
            }
        };
        if entry.depth() == 0 {
            continue; // the root itself: context, not an item of a top entry
        }
        // The depth-1 ancestor decides the bucket: depth 1 entries
        // are their own bucket; deeper entries fold into the depth-1
        // directory above them; depth-1 files are root files.
        let rel = entry.path().strip_prefix(&canon).unwrap_or(entry.path());
        let first = rel
            .components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_default();
        let ft = entry.file_type();
        let is_dir = ft.is_dir();
        let bucket = if entry.depth() == 1 && !is_dir {
            top_entry(&mut census, "")
        } else {
            top_entry(&mut census, &first)
        };
        if is_dir {
            census.dirs += 1;
            census.top[bucket].dirs += 1;
            continue;
        }
        // Files and symlinks: metadata only. A symlink contributes
        // its own lstat size to nothing — the indexer records the
        // symlink but its size_bytes comes from item_from_path; for
        // byte totals the honest number is the target-free one:
        // symlink lengths are path strings, not data, so they are
        // counted as files with zero bytes.
        let name = entry.file_name().to_string_lossy();
        let candidate = is_candidate(&name, candidates);
        let size = if ft.is_symlink() {
            0
        } else {
            entry.metadata().map(|m| m.len()).unwrap_or(0)
        };
        census.files += 1;
        census.bytes += size;
        census.top[bucket].files += 1;
        census.top[bucket].bytes += size;
        if candidate {
            census.candidate_files += 1;
            census.candidate_bytes += size;
            census.top[bucket].candidate_files += 1;
            census.top[bucket].candidate_bytes += size;
        }
    }
    census.top.sort_by(|a, b| {
        b.files
            .cmp(&a.files)
            .then_with(|| a.name.cmp(&b.name))
    });
    census.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(census)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ufm-census-test-{}-{}-{}",
            tag,
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn candidates() -> HashSet<String> {
        ["zip", "sfc", "chd"].iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn extension_semantics_match_the_identify_feed() {
        assert_eq!(extension_of("Game (USA).zip"), Some("zip".into()));
        assert_eq!(extension_of("Game (USA).ZIP"), Some("zip".into()));
        assert_eq!(extension_of("a.b.c.sfc"), Some("sfc".into()));
        assert_eq!(extension_of("noext"), None);
        assert_eq!(extension_of(".zip"), None, "leading-dot name has no extension");
        assert_eq!(extension_of("trailing."), None);
        assert!(is_candidate("Alundra (USA).chd", &candidates()));
        assert!(!is_candidate("boxart.png", &candidates()));
    }

    #[test]
    fn threshold_boundary_is_exact() {
        assert!(!over_threshold(GATE_THRESHOLD_FILES));
        assert!(over_threshold(GATE_THRESHOLD_FILES + 1));
        assert!(!over_threshold(0));
    }

    #[test]
    fn estimates_are_monotonic_and_zero_safe() {
        assert_eq!(Census::est_identify_secs(0), 0);
        assert!(Census::est_identify_secs(HASH_BYTES_PER_SEC * 10) >= 10);
        let c = Census {
            files: 100,
            dirs: 10,
            elapsed_ms: 2000,
            ..Default::default()
        };
        assert_eq!(c.est_index_secs(0), 0);
        assert!(c.est_index_secs(110) >= c.est_index_secs(55));
        assert_eq!(Census::default().est_index_secs(10), 0);
    }

    /// root/{roms/{a.zip (10B), b.png (5B)}, game/{c.bin (7B)}, loose.sfc (3B)}
    #[test]
    fn census_folds_subtrees_into_top_entries() {
        let root = scratch("fold");
        std::fs::create_dir_all(root.join("roms")).unwrap();
        std::fs::create_dir_all(root.join("game")).unwrap();
        std::fs::write(root.join("roms").join("a.zip"), vec![0u8; 10]).unwrap();
        std::fs::write(root.join("roms").join("b.png"), vec![0u8; 5]).unwrap();
        std::fs::write(root.join("game").join("c.bin"), vec![0u8; 7]).unwrap();
        std::fs::write(root.join("loose.sfc"), vec![0u8; 3]).unwrap();

        let c = census_root(&root, &candidates()).unwrap();
        assert_eq!(c.files, 4);
        assert_eq!(c.dirs, 2);
        assert_eq!(c.bytes, 25);
        assert_eq!(c.candidate_files, 2, "a.zip + loose.sfc");
        assert_eq!(c.candidate_bytes, 13);
        let roms = c.top.iter().find(|t| t.name == "roms").unwrap();
        assert_eq!((roms.files, roms.dirs, roms.candidate_files), (2, 1, 1));
        let root_files = c.top.iter().find(|t| t.name == "").unwrap();
        assert_eq!((root_files.files, root_files.dirs), (1, 0));
        assert_eq!(c.total_items(), 6);
        std::fs::remove_dir_all(&root).ok();
    }

    /// Index scope law agreement (2026-10-18): the census prunes
    /// hidden directories exactly like the indexer, so its counts
    /// (and the folder pick built from them) describe the walk
    /// that will actually happen. root/{visible/f.txt,
    /// .hidden/secret.txt, visible/.nested/deep.txt, top.txt} →
    /// only f.txt + top.txt count; no ".hidden" top entry exists
    /// to be picked.
    #[test]
    fn census_prunes_hidden_dirs_like_the_indexer() {
        let root = scratch("hidden");
        std::fs::create_dir_all(root.join("visible").join(".nested")).unwrap();
        std::fs::create_dir_all(root.join(".hidden")).unwrap();
        std::fs::write(root.join("visible").join("f.txt"), vec![0u8; 3]).unwrap();
        std::fs::write(
            root.join("visible").join(".nested").join("deep.txt"),
            vec![0u8; 4],
        )
        .unwrap();
        std::fs::write(root.join(".hidden").join("secret.txt"), vec![0u8; 3]).unwrap();
        std::fs::write(root.join("top.txt"), vec![0u8; 3]).unwrap();

        let c = census_root(&root, &candidates()).unwrap();
        assert_eq!(c.files, 2, "f.txt + top.txt only");
        assert_eq!(c.dirs, 1, "visible only — .nested is pruned too");
        assert!(
            c.top.iter().all(|t| t.name != ".hidden"),
            "a pruned tree must never appear as a pickable top entry"
        );
        std::fs::remove_dir_all(&root).ok();
    }
}
