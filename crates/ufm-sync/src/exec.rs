//! Reference plan executor: applies plan actions directly to the
//! filesystem, committing the manifest after EACH action (the same
//! contract the app's job hook implements per completed step). The
//! app itself executes plans through the journaled job system; this
//! executor exists for the `sync-harness` proofs and crate tests —
//! and because a plan that cannot be executed by a hundred lines of
//! plain fs calls is a plan that doesn't mean what it says.

use crate::{fingerprint_file, preserved_path, Manifest, Plan, PlanAction, Side, SyncReport};
use std::path::{Path, PathBuf};
use ufm_core::{Result, UfmError};

fn side_root<'a>(root_a: &'a Path, root_b: &'a Path, side: Side) -> &'a Path {
    match side {
        Side::A => root_a,
        Side::B => root_b,
    }
}

fn copy_file(src: &Path, dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| UfmError::Internal(format!("mkdir {}: {e}", parent.display())))?;
    }
    std::fs::copy(src, dst).map_err(|e| {
        UfmError::Internal(format!("copy {} → {}: {e}", src.display(), dst.display()))
    })?;
    // Keep the destination's mtime equal to the source's, so the
    // fingerprint the commit records is the one the source side sees.
    if let Ok(md) = std::fs::metadata(src) {
        if let Ok(mt) = md.modified() {
            if let Ok(f) = std::fs::File::options().write(true).open(dst) {
                let _ = f.set_modified(mt);
            }
        }
    }
    Ok(())
}

/// Perform one plan action. Idempotent by construction (copies
/// overwrite with the winning bytes; deletes of absent files pass).
pub fn apply_action(root_a: &Path, root_b: &Path, stamp: &str, action: &PlanAction) -> Result<()> {
    match action {
        PlanAction::Copy { from, rel } | PlanAction::Restore { survivor: from, rel } => {
            let (from, rel) = (*from, rel);
            let src = side_root(root_a, root_b, from).join(&rel);
            let dst = side_root(root_a, root_b, from.other()).join(&rel);
            copy_file(&src, &dst)
        }
        PlanAction::Delete { side, rel } => {
            let target = side_root(root_a, root_b, *side).join(rel);
            match std::fs::remove_file(&target) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(UfmError::Internal(format!(
                    "delete {}: {e}",
                    target.display()
                ))),
            }
        }
        PlanAction::Preserve { loser, rel } => {
            let src = side_root(root_a, root_b, *loser).join(rel);
            let dst: PathBuf = preserved_path(side_root(root_a, root_b, *loser), stamp, rel);
            copy_file(&src, &dst)
        }
    }
}

/// The manifest effect of a completed action, to be committed (and
/// the manifest saved) before the next action runs. Copies fingerprint
/// the DESTINATION as it now stands — the base records the agreed
/// truth, not the plan's hope.
pub fn commit_effect(
    manifest: &mut Manifest,
    root_a: &Path,
    root_b: &Path,
    action: &PlanAction,
) -> Result<()> {
    match action {
        PlanAction::Copy { from, rel } | PlanAction::Restore { survivor: from, rel } => {
            let dst = side_root(root_a, root_b, from.other()).join(rel);
            let fp = fingerprint_file(&dst)?;
            manifest.set(rel, fp);
        }
        PlanAction::Delete { rel, .. } => manifest.remove(rel),
        PlanAction::Preserve { .. } => {}
    }
    Ok(())
}

/// Execute a whole plan against the manifest at `manifest_path`,
/// saving the manifest after the absorb pass and after EVERY action.
/// `die_after = Some(n)` aborts the process after n committed actions
/// — the harness's crash simulator; production callers pass None.
pub fn run_plan(
    root_a: &Path,
    root_b: &Path,
    manifest_path: &Path,
    pair_id: &str,
    pair_name: &str,
    plan: &Plan,
    started: String,
    die_after: Option<usize>,
) -> Result<SyncReport> {
    let mut manifest = Manifest::load(manifest_path)?;
    // Absorb pass: agreed-state updates and dead-entry removals
    // describe the CURRENT disk truth (hash-verified at plan time),
    // so they commit before any byte moves.
    for rel in &plan.base_removals {
        manifest.remove(rel);
    }
    for (rel, fp) in &plan.base_updates {
        manifest.set(rel, fp.clone());
    }
    manifest.save(manifest_path)?;

    let mut report = SyncReport::from_plan(pair_id, pair_name, plan, started);
    let mut done = 0usize;
    for action in &plan.actions {
        if let Err(e) = apply_action(root_a, root_b, &plan.stamp, action) {
            report.failures.push(format!("{action:?}: {e}"));
            continue;
        }
        commit_effect(&mut manifest, root_a, root_b, action)?;
        manifest.save(manifest_path)?;
        done += 1;
        if die_after == Some(done) {
            std::process::abort();
        }
    }
    Ok(report)
}
