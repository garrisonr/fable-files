//! ufm-sync: two-way folder synchronization for Fable Files.
//!
//! Design: ~/workspace/fable-sync/DESIGN.md. The short version:
//!
//! - A pair is two folder roots (A and B). A per-pair **manifest**
//!   remembers every file's fingerprint as of the last completed sync
//!   (the *base*). Planning is a three-way diff (A, B, base) — the only
//!   honest way to tell "deleted here" from "new there".
//! - THE CONFLICT LAW: never silently overwrite, never silently delete.
//!   One-sided changes travel. Deletions propagate only when the other
//!   side is unchanged. Delete-vs-edit keeps the file. Both-sides-changed
//!   → newest wins the path, the loser is preserved in
//!   `.fable-conflicts/<stamp>/` on its own side and named in the report.
//! - This crate is the engine only: scan, fingerprint, plan, manifest
//!   store, report. The app turns a plan into a journaled job
//!   (Mkdir/Copy/Delete steps) and commits manifest effects per
//!   completed step; the bundled `sync-harness` binary executes plans
//!   directly, for scripted proofs and recovery drills.

use serde::{Deserialize, Serialize};
pub mod exec;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use ufm_core::{Result, UfmError};

/// The folder at either root that holds preserved conflict losers.
/// Never itself synced (the scanner skips it on sight).
pub const CONFLICTS_DIR: &str = ".fable-conflicts";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    A,
    B,
}

impl Side {
    pub fn other(self) -> Side {
        match self {
            Side::A => Side::B,
            Side::B => Side::A,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Side::A => "A",
            Side::B => "B",
        }
    }
}

/// What a file looked like the last time both sides agreed on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    pub size: u64,
    /// Modification time as Unix NANOSECONDS (i64 range covers
    /// ±292 years); None when the filesystem/provider won't say
    /// (some MTP mounts) — the hash then carries the decision alone.
    /// Nanosecond precision is load-bearing: at second granularity,
    /// a same-size edit inside one second is invisible to the fast
    /// path and a delete could propagate over it.
    pub mtime: Option<i64>,
    pub sha256: String,
}

/// A file as seen by a scan, before any hashing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    pub size: u64,
    pub mtime: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkipNote {
    pub side: Side,
    pub rel: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default)]
pub struct Scan {
    /// Relative paths (forward slashes) → file facts.
    pub files: BTreeMap<String, FileInfo>,
    pub skipped: Vec<SkipNote>,
}

/// Per-pair memory: the base every plan diffs against.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub entries: BTreeMap<String, Fingerprint>,
}

impl Manifest {
    pub fn new() -> Manifest {
        Manifest {
            version: 1,
            entries: BTreeMap::new(),
        }
    }

    pub fn load(path: &Path) -> Result<Manifest> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| UfmError::Internal(format!("parse {}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Manifest::new()),
            Err(e) => Err(UfmError::Internal(format!(
                "read {}: {e}",
                path.display()
            ))),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        save_json_atomic(path, self)
    }

    pub fn set(&mut self, rel: &str, fp: Fingerprint) {
        self.entries.insert(rel.to_string(), fp);
    }

    pub fn remove(&mut self, rel: &str) {
        self.entries.remove(rel);
    }
}

// ---------------------------------------------------------------------------
// JSON persistence — atomic (temp file in the same dir + rename), the
// house rule for every state file that must survive a crash.
// ---------------------------------------------------------------------------

pub fn load_json_or_default<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> Result<T> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| UfmError::Internal(format!("parse {}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(UfmError::Internal(format!("read {}: {e}", path.display()))),
    }
}

pub fn save_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| UfmError::Internal(format!("create {}: {e}", parent.display())))?;
    }
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| UfmError::Internal(format!("serialize: {e}")))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)
        .map_err(|e| UfmError::Internal(format!("write {}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| UfmError::Internal(format!("rename to {}: {e}", path.display())))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Scanning + fingerprinting
// ---------------------------------------------------------------------------

fn rel_string(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn mtime_secs(md: &std::fs::Metadata) -> Option<i64> {
    md.modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_nanos() as i64)
}

/// Walk one side. Symlinks are never followed (recorded as skips);
/// `.fable-conflicts` is invisible. A walk error aborts the scan —
/// planning from a half-seen tree is how sync tools delete things.
pub fn scan(root: &Path, side: Side) -> Result<Scan> {
    if !root.is_dir() {
        return Err(UfmError::InvalidInput(format!(
            "sync side {} is not a folder: {}",
            side.label(),
            root.display()
        )));
    }
    let mut out = Scan::default();
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(|e| {
            UfmError::Internal(format!("scan {}: {e}", root.display()))
        })?;
        let path = entry.path();
        if path == root {
            continue;
        }
        let rel = rel_string(root, path);
        if rel == CONFLICTS_DIR || rel.starts_with(&format!("{CONFLICTS_DIR}/")) {
            continue;
        }
        let ft = entry.file_type();
        if ft.is_symlink() {
            out.skipped.push(SkipNote {
                side,
                rel,
                reason: "symlink — sync never follows links".into(),
            });
            continue;
        }
        if ft.is_dir() {
            continue; // directories are structure, created on demand
        }
        let md = entry
            .metadata()
            .map_err(|e| UfmError::Internal(format!("stat {}: {e}", path.display())))?;
        out.files.insert(
            rel,
            FileInfo {
                size: md.len(),
                mtime: mtime_secs(&md),
            },
        );
    }
    Ok(out)
}

/// Content fingerprint of one file (size + mtime + SHA-256 of bytes).
pub fn fingerprint_file(path: &Path) -> Result<Fingerprint> {
    let md = std::fs::metadata(path)
        .map_err(|e| UfmError::Internal(format!("stat {}: {e}", path.display())))?;
    let mut f = std::fs::File::open(path)
        .map_err(|e| UfmError::Internal(format!("open {}: {e}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| UfmError::Internal(format!("read {}: {e}", path.display())))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    let mut sha = String::with_capacity(64);
    for b in digest {
        sha.push_str(&format!("{b:02x}"));
    }
    Ok(Fingerprint {
        size: md.len(),
        mtime: mtime_secs(&md),
        sha256: sha,
    })
}

// ---------------------------------------------------------------------------
// Planning — the three-way diff and the Conflict Law.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanAction {
    /// Copy `rel` from one side over the other (the source side is the
    /// one whose content wins this path).
    Copy { from: Side, rel: String },
    /// Propagated deletion: `rel` disappears from `side` (the app sends
    /// it to the Trash; the harness deletes). Only ever planned when the
    /// other side is unchanged since the base.
    Delete { side: Side, rel: String },
    /// Conflict preservation: copy the losing side's file into its own
    /// `.fable-conflicts/<stamp>/<rel>` BEFORE the winning Copy runs.
    Preserve { loser: Side, rel: String },
    /// Delete-vs-edit: the survivor is copied back to the side that
    /// deleted it. Mechanically a copy; reported as a restore.
    Restore { survivor: Side, rel: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictNote {
    pub rel: String,
    pub winner: Side,
    /// Where the loser was kept, relative to the loser's root
    /// (e.g. ".fable-conflicts/2026-10-16-1619/notes.txt"); empty for
    /// delete-vs-edit, where the loser is the deletion itself.
    pub loser_kept: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Plan {
    pub actions: Vec<PlanAction>,
    pub conflicts: Vec<ConflictNote>,
    /// Agreed-state fingerprints the base absorbs with NO action
    /// (both-sides-same-edit). Safe to apply any time: each was
    /// verified against both sides' bytes during planning. Fingerprints
    /// for action outcomes are NOT here — they commit per completed
    /// step, fingerprinted fresh at the destination by the executor.
    pub base_updates: Vec<(String, Fingerprint)>,
    /// Base entries to drop (file gone from both sides).
    pub base_removals: Vec<String>,
    pub skipped: Vec<SkipNote>,
    pub unchanged: usize,
    /// The conflict-folder stamp this plan's Preserve actions use.
    pub stamp: String,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty() && self.base_updates.is_empty()
    }
    pub fn count_from(&self, side: Side) -> usize {
        self.actions
            .iter()
            .filter(|a| matches!(a, PlanAction::Copy { from, .. } if *from == side))
            .count()
    }
    pub fn count_deletes(&self) -> usize {
        self.actions
            .iter()
            .filter(|a| matches!(a, PlanAction::Delete { .. }))
            .count()
    }
}

enum Presence {
    Absent,
    Unchanged,
    Changed(Fingerprint),
}

/// Classify one side's current file against the base. Hashes whenever
/// (size, mtime) disagrees with the base — bytes decide, not clocks.
fn classify(
    root: &Path,
    rel: &str,
    info: Option<&FileInfo>,
    base: Option<&Fingerprint>,
) -> Result<Presence> {
    let Some(info) = info else {
        return Ok(Presence::Absent);
    };
    if let Some(b) = base {
        if info.size == b.size && info.mtime.is_some() && info.mtime == b.mtime {
            return Ok(Presence::Unchanged);
        }
    }
    let fp = fingerprint_file(&root.join(rel))?;
    match base {
        Some(b) if fp.size == b.size && fp.sha256 == b.sha256 => Ok(Presence::Unchanged),
        _ => Ok(Presence::Changed(fp)),
    }
}

/// The planner. Deterministic: same trees + same base ⇒ same plan
/// (the app relies on this — the preview IS the plan that runs).
pub fn plan(
    root_a: &Path,
    root_b: &Path,
    manifest: &Manifest,
    stamp: String,
) -> Result<Plan> {
    let scan_a = scan(root_a, Side::A)?;
    let scan_b = scan(root_b, Side::B)?;
    let mut out = Plan {
        stamp,
        ..Default::default()
    };
    out.skipped.extend(scan_a.skipped.iter().cloned());
    out.skipped.extend(scan_b.skipped.iter().cloned());

    // Every path anyone knows about: A, B, or the base.
    let mut rels: Vec<String> = Vec::new();
    rels.extend(scan_a.files.keys().cloned());
    rels.extend(scan_b.files.keys().cloned());
    rels.extend(manifest.entries.keys().cloned());
    rels.sort();
    rels.dedup();

    for rel in rels {
        let base = manifest.entries.get(&rel);
        let pa = classify(root_a, &rel, scan_a.files.get(&rel), base)?;
        let pb = classify(root_b, &rel, scan_b.files.get(&rel), base)?;
        use Presence::*;
        match (pa, pb) {
            (Absent, Absent) => {
                // Gone from both: the base entry (its only possible
                // source) is dropped at commit time. No action.
                out.base_removals.push(rel.clone());
            }
            (Unchanged, Unchanged) => out.unchanged += 1,
            (Changed(_fp), Absent) => {
                if base.is_some() {
                    // B deleted it; A edited it → delete-vs-edit:
                    // the file survives, restored to B.
                    out.actions.push(PlanAction::Restore {
                        survivor: Side::A,
                        rel: rel.clone(),
                    });
                    out.conflicts.push(ConflictNote {
                        rel: rel.clone(),
                        winner: Side::A,
                        loser_kept: String::new(),
                        reason: "deleted on B, edited on A — the edit survives and was restored to B".into(),
                    });
                } else {
                    out.actions.push(PlanAction::Copy {
                        from: Side::A,
                        rel: rel.clone(),
                    });
                }
            }
            (Absent, Changed(_fp)) => {
                if base.is_some() {
                    out.actions.push(PlanAction::Restore {
                        survivor: Side::B,
                        rel: rel.clone(),
                    });
                    out.conflicts.push(ConflictNote {
                        rel: rel.clone(),
                        winner: Side::B,
                        loser_kept: String::new(),
                        reason: "deleted on A, edited on B — the edit survives and was restored to A".into(),
                    });
                } else {
                    out.actions.push(PlanAction::Copy {
                        from: Side::B,
                        rel: rel.clone(),
                    });
                }
            }
            (Unchanged, Absent) => {
                // In the base, untouched on A, gone on B → B's deletion
                // propagates to A. (Requires a base entry: without one,
                // "absent on B" just means "never there".)
                if base.is_some() {
                    out.actions.push(PlanAction::Delete {
                        side: Side::A,
                        rel: rel.clone(),
                    });
                } else {
                    out.actions.push(PlanAction::Copy {
                        from: Side::A,
                        rel,
                    });
                }
            }
            (Absent, Unchanged) => {
                if base.is_some() {
                    out.actions.push(PlanAction::Delete {
                        side: Side::B,
                        rel: rel.clone(),
                    });
                } else {
                    out.actions.push(PlanAction::Copy {
                        from: Side::B,
                        rel,
                    });
                }
            }
            (Changed(_fp), Unchanged) => {
                out.actions.push(PlanAction::Copy {
                    from: Side::A,
                    rel: rel.clone(),
                });
            }
            (Unchanged, Changed(_fp)) => {
                out.actions.push(PlanAction::Copy {
                    from: Side::B,
                    rel: rel.clone(),
                });
            }
            (Changed(fa), Changed(fb)) => {
                if fa.sha256 == fb.sha256 {
                    // Both sides made the same edit (or the file was
                    // copied outside sync): agree, absorb, no action.
                    out.base_updates.push((rel.clone(), fa));
                    out.unchanged += 1;
                } else {
                    // True conflict. Newest mtime wins the path; a tie
                    // goes to A (arbitrary, deterministic, reported).
                    let ma = scan_a.files.get(&rel).and_then(|f| f.mtime);
                    let mb = scan_b.files.get(&rel).and_then(|f| f.mtime);
                    let winner = if mb > ma { Side::B } else { Side::A };
                    let loser = winner.other();
                    out.actions.push(PlanAction::Preserve {
                        loser,
                        rel: rel.clone(),
                    });
                    out.actions.push(PlanAction::Copy {
                        from: winner,
                        rel: rel.clone(),
                    });
                    out.conflicts.push(ConflictNote {
                        rel: rel.clone(),
                        winner,
                        loser_kept: format!("{CONFLICTS_DIR}/{}/{rel}", out.stamp),
                        reason: format!(
                            "changed on both sides — newest wins; the {} copy was preserved",
                            loser.label()
                        ),
                    });
                }
            }
        }
    }
    Ok(out)
}

/// The path a Preserve action writes to, inside the loser's root.
pub fn preserved_path(root_loser: &Path, stamp: &str, rel: &str) -> PathBuf {
    root_loser.join(CONFLICTS_DIR).join(stamp).join(rel)
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncReport {
    pub pair_id: String,
    pub pair_name: String,
    pub started: String,
    pub finished: Option<String>,
    pub copied_a_to_b: Vec<String>,
    pub copied_b_to_a: Vec<String>,
    pub deleted_from_a: Vec<String>,
    pub deleted_from_b: Vec<String>,
    pub restored: Vec<String>,
    pub conflicts: Vec<ConflictNote>,
    pub skipped: Vec<SkipNote>,
    pub failures: Vec<String>,
    pub unchanged: usize,
}

impl SyncReport {
    pub fn from_plan(pair_id: &str, pair_name: &str, plan: &Plan, started: String) -> SyncReport {
        let mut r = SyncReport {
            pair_id: pair_id.to_string(),
            pair_name: pair_name.to_string(),
            started,
            conflicts: plan.conflicts.clone(),
            skipped: plan.skipped.clone(),
            unchanged: plan.unchanged,
            ..Default::default()
        };
        for a in &plan.actions {
            match a {
                PlanAction::Copy {
                    from: Side::A,
                    rel,
                } => r.copied_a_to_b.push(rel.clone()),
                PlanAction::Copy {
                    from: Side::B,
                    rel,
                } => r.copied_b_to_a.push(rel.clone()),
                PlanAction::Delete {
                    side: Side::A,
                    rel,
                } => r.deleted_from_a.push(rel.clone()),
                PlanAction::Delete {
                    side: Side::B,
                    rel,
                } => r.deleted_from_b.push(rel.clone()),
                PlanAction::Restore { rel, .. } => r.restored.push(rel.clone()),
                PlanAction::Preserve { .. } => {}
            }
        }
        r
    }

    /// One calm paragraph for humans.
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        let ab = self.copied_a_to_b.len();
        let ba = self.copied_b_to_a.len();
        if ab > 0 {
            parts.push(format!("{ab} file{} A → B", if ab == 1 { "" } else { "s" }));
        }
        if ba > 0 {
            parts.push(format!("{ba} file{} B → A", if ba == 1 { "" } else { "s" }));
        }
        let del = self.deleted_from_a.len() + self.deleted_from_b.len();
        if del > 0 {
            parts.push(format!("{del} deletion{} propagated", if del == 1 { "" } else { "s" }));
        }
        if !self.restored.is_empty() {
            parts.push(format!(
                "{} file{} restored after delete-vs-edit",
                self.restored.len(),
                if self.restored.len() == 1 { "" } else { "s" }
            ));
        }
        if !self.conflicts.is_empty() {
            parts.push(format!(
                "{} conflict{} (losers preserved)",
                self.conflicts.len(),
                if self.conflicts.len() == 1 { "" } else { "s" }
            ));
        }
        if !self.failures.is_empty() {
            parts.push(format!(
                "{} FAILED",
                self.failures.len()
            ));
        }
        if parts.is_empty() {
            "Already in sync — nothing moved.".to_string()
        } else {
            parts.join(" · ")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, SystemTime};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    struct Fx {
        dir: PathBuf,
        a: PathBuf,
        b: PathBuf,
        manifest: PathBuf,
    }

    fn fixture(tag: &str) -> Fx {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "ufm-sync-test-{tag}-{}-{n}",
            std::process::id()
        ));
        let a = dir.join("a");
        let b = dir.join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let manifest = dir.join("manifest.json");
        Fx { dir, a, b, manifest }
    }

    fn write(root: &Path, rel: &str, bytes: &str, mtime: u64) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
        let f = std::fs::File::options().write(true).open(&p).unwrap();
        f.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(mtime))
            .unwrap();
    }

    fn read(root: &Path, rel: &str) -> String {
        std::fs::read_to_string(root.join(rel)).unwrap()
    }

    fn stamp() -> String {
        "2026-10-16-120000".to_string()
    }

    /// Plan + execute fully; returns the plan that ran.
    fn sync(fx: &Fx) -> Plan {
        let m = Manifest::load(&fx.manifest).unwrap();
        let p = plan(&fx.a, &fx.b, &m, stamp()).unwrap();
        exec::run_plan(
            &fx.a,
            &fx.b,
            &fx.manifest,
            "pair1",
            "Test Pair",
            &p,
            "2026-10-16T12:00:00Z".into(),
            None,
        )
        .unwrap();
        p
    }

    fn cleanup(fx: Fx) {
        let _ = std::fs::remove_dir_all(&fx.dir);
    }

    #[test]
    fn new_files_travel_both_ways_and_second_sync_is_quiet() {
        let fx = fixture("new");
        write(&fx.a, "from-a.txt", "aaa", 100);
        write(&fx.b, "sub/from-b.txt", "bbb", 100);
        let p = sync(&fx);
        assert_eq!(p.count_from(Side::A), 1);
        assert_eq!(p.count_from(Side::B), 1);
        assert_eq!(read(&fx.b, "from-a.txt"), "aaa");
        assert_eq!(read(&fx.a, "sub/from-b.txt"), "bbb");
        let p2 = sync(&fx);
        assert!(p2.actions.is_empty(), "second sync must be empty: {p2:?}");
        cleanup(fx);
    }

    #[test]
    fn edit_propagates_and_delete_propagates_when_other_side_unchanged() {
        let fx = fixture("edit-del");
        write(&fx.a, "doc.txt", "v1", 100);
        write(&fx.b, "doc.txt", "v1", 100);
        write(&fx.a, "gone.txt", "x", 100);
        write(&fx.b, "gone.txt", "x", 100);
        sync(&fx); // establish base
        write(&fx.a, "doc.txt", "v2", 200); // edit on A
        std::fs::remove_file(fx.b.join("gone.txt")).unwrap(); // delete on B
        let p = sync(&fx);
        assert!(p.actions.contains(&PlanAction::Copy {
            from: Side::A,
            rel: "doc.txt".into()
        }));
        assert!(p.actions.contains(&PlanAction::Delete {
            side: Side::A,
            rel: "gone.txt".into()
        }));
        assert_eq!(read(&fx.b, "doc.txt"), "v2");
        assert!(!fx.a.join("gone.txt").exists());
        cleanup(fx);
    }

    #[test]
    fn delete_vs_edit_restores_the_file() {
        let fx = fixture("del-vs-edit");
        write(&fx.a, "keep.txt", "v1", 100);
        write(&fx.b, "keep.txt", "v1", 100);
        sync(&fx);
        std::fs::remove_file(fx.a.join("keep.txt")).unwrap(); // delete on A
        write(&fx.b, "keep.txt", "v2-edited", 200); // edit on B
        let p = sync(&fx);
        assert_eq!(p.conflicts.len(), 1);
        assert!(p.actions.contains(&PlanAction::Restore {
            survivor: Side::B,
            rel: "keep.txt".into()
        }));
        assert_eq!(read(&fx.a, "keep.txt"), "v2-edited");
        assert_eq!(read(&fx.b, "keep.txt"), "v2-edited");
        cleanup(fx);
    }

    #[test]
    fn both_sides_edit_conflict_newest_wins_loser_preserved() {
        let fx = fixture("conflict");
        write(&fx.a, "notes.txt", "base", 100);
        write(&fx.b, "notes.txt", "base", 100);
        sync(&fx);
        write(&fx.a, "notes.txt", "a-newer", 300);
        write(&fx.b, "notes.txt", "b-older", 200);
        let p = sync(&fx);
        assert_eq!(p.conflicts.len(), 1);
        assert_eq!(p.conflicts[0].winner, Side::A);
        assert_eq!(read(&fx.a, "notes.txt"), "a-newer");
        assert_eq!(read(&fx.b, "notes.txt"), "a-newer");
        let kept = fx
            .b
            .join(CONFLICTS_DIR)
            .join("2026-10-16-120000")
            .join("notes.txt");
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), "b-older");
        // The preserved loser must never re-enter sync.
        let p2 = sync(&fx);
        assert!(p2.actions.is_empty(), "{p2:?}");
        cleanup(fx);
    }

    #[test]
    fn same_edit_on_both_sides_is_absorbed_not_conflicted() {
        let fx = fixture("same-edit");
        write(&fx.a, "f.txt", "v1", 100);
        write(&fx.b, "f.txt", "v1", 100);
        sync(&fx);
        write(&fx.a, "f.txt", "v2", 200);
        write(&fx.b, "f.txt", "v2", 200);
        let p = sync(&fx);
        assert!(p.actions.is_empty());
        assert!(p.conflicts.is_empty());
        cleanup(fx);
    }

    #[test]
    fn touched_but_identical_is_not_a_change() {
        let fx = fixture("touch");
        write(&fx.a, "f.txt", "same", 100);
        write(&fx.b, "f.txt", "same", 100);
        sync(&fx);
        write(&fx.a, "f.txt", "same", 999); // new mtime, same bytes
        let p = sync(&fx);
        assert!(p.actions.is_empty(), "{p:?}");
        cleanup(fx);
    }

    #[test]
    fn partial_run_replans_to_exactly_the_remainder() {
        let fx = fixture("partial");
        write(&fx.a, "one.txt", "1", 100);
        write(&fx.a, "two.txt", "2", 100);
        write(&fx.a, "three.txt", "3", 100);
        // Execute only the first action of the first plan, committing
        // as the contract demands — a stand-in for a killed run.
        let m = Manifest::load(&fx.manifest).unwrap();
        let p = plan(&fx.a, &fx.b, &m, stamp()).unwrap();
        assert_eq!(p.actions.len(), 3);
        let mut man = Manifest::load(&fx.manifest).unwrap();
        let first = p.actions[0].clone();
        exec::apply_action(&fx.a, &fx.b, &p.stamp, &first).unwrap();
        exec::commit_effect(&mut man, &fx.a, &fx.b, &first).unwrap();
        man.save(&fx.manifest).unwrap();
        // Replan: exactly the two remaining copies, nothing redone.
        let p2 = sync(&fx);
        assert_eq!(p2.actions.len(), 2, "{p2:?}");
        for rel in ["one.txt", "two.txt", "three.txt"] {
            assert_eq!(read(&fx.b, rel), read(&fx.a, rel));
        }
        let p3 = sync(&fx);
        assert!(p3.actions.is_empty());
        cleanup(fx);
    }

    #[test]
    fn first_sync_of_identical_trees_only_absorbs() {
        let fx = fixture("identical");
        write(&fx.a, "x.txt", "same", 100);
        write(&fx.b, "x.txt", "same", 100);
        let p = sync(&fx);
        assert!(p.actions.is_empty());
        let p2 = sync(&fx);
        assert!(p2.is_empty());
        cleanup(fx);
    }

    #[test]
    fn same_size_edit_within_one_second_is_a_change() {
        // Regression: fingerprints once kept second-granularity
        // mtimes; a same-size edit landing inside the same second as
        // the base write classified Unchanged — and a delete on the
        // other side would then have propagated over the edit.
        let fx = fixture("subsecond");
        write(&fx.a, "f.txt", "edited", 100);
        write(&fx.b, "f.txt", "edited", 100);
        sync(&fx);
        // Same bytes length, different content, +1ms on A.
        let p = fx.a.join("f.txt");
        std::fs::write(&p, "b-edit").unwrap();
        let f = std::fs::File::options().write(true).open(&p).unwrap();
        f.set_modified(
            SystemTime::UNIX_EPOCH + Duration::from_millis(100_001),
        )
        .unwrap();
        let m = Manifest::load(&fx.manifest).unwrap();
        let p = plan(&fx.a, &fx.b, &m, stamp()).unwrap();
        assert!(
            p.actions.contains(&PlanAction::Copy {
                from: Side::A,
                rel: "f.txt".into()
            }),
            "sub-second same-size edit must plan a copy: {p:?}"
        );
        cleanup(fx);
    }

    #[test]
    fn manifest_round_trips_and_missing_side_is_a_hard_stop() {
        let fx = fixture("manifest");
        let mut m = Manifest::new();
        m.set(
            "a.txt",
            Fingerprint {
                size: 3,
                mtime: Some(7),
                sha256: "ab".into(),
            },
        );
        m.save(&fx.manifest).unwrap();
        let back = Manifest::load(&fx.manifest).unwrap();
        assert_eq!(back.entries.len(), 1);
        let missing = fx.dir.join("nope");
        assert!(plan(&missing, &fx.b, &back, stamp()).is_err());
        cleanup(fx);
    }
}
