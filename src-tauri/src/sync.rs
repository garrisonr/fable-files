//! Folder Sync pairs — the app side of the ufm-sync engine.
//!
//! Design: ~/workspace/fable-sync/DESIGN.md. This module owns the pair
//! registry and state files (all under `<fable_data_dir>/sync/`, beside
//! the incumbent sync-schedules.json), the plan views the dialog
//! renders, the run sidecar that maps job steps to manifest effects,
//! and the executor hook that commits those effects per completed
//! step. Job *building* lives in commands.rs (with the other
//! PathPlanner users); the planner itself is ufm_sync::plan.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use ufm_sync::{Manifest, Plan, PlanAction, Side, SyncReport};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncPair {
    pub id: String,
    pub name: String,
    pub root_a: String,
    pub root_b: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PairsFile {
    pairs: Vec<SyncPair>,
}

pub fn sync_dir(base: &Path) -> PathBuf {
    base.join("sync")
}

fn pairs_path(base: &Path) -> PathBuf {
    sync_dir(base).join("pairs.json")
}

pub fn manifest_path(base: &Path, pair_id: &str) -> PathBuf {
    sync_dir(base).join(format!("{pair_id}.manifest.json"))
}

pub fn report_path(base: &Path, pair_id: &str) -> PathBuf {
    sync_dir(base).join(format!("{pair_id}.report.json"))
}

fn runs_dir(base: &Path) -> PathBuf {
    sync_dir(base).join("runs")
}

pub fn sidecar_path(base: &Path, job_id: &str) -> PathBuf {
    runs_dir(base).join(format!("{job_id}.json"))
}

pub fn load_pairs(base: &Path) -> Vec<SyncPair> {
    ufm_sync::load_json_or_default::<PairsFile>(&pairs_path(base))
        .map(|f| f.pairs)
        .unwrap_or_default()
}

pub fn save_pairs(base: &Path, pairs: &[SyncPair]) -> Result<(), String> {
    ufm_sync::save_json_atomic(
        &pairs_path(base),
        &PairsFile {
            pairs: pairs.to_vec(),
        },
    )
    .map_err(|e| e.to_string())
}

pub fn pair_by_id(base: &Path, id: &str) -> Option<SyncPair> {
    load_pairs(base).into_iter().find(|p| p.id == id)
}

pub fn load_manifest(base: &Path, pair_id: &str) -> Result<Manifest, String> {
    Manifest::load(&manifest_path(base, pair_id)).map_err(|e| e.to_string())
}

pub fn save_manifest(base: &Path, pair_id: &str, manifest: &Manifest) -> Result<(), String> {
    manifest
        .save(&manifest_path(base, pair_id))
        .map_err(|e| e.to_string())
}

pub fn load_report(base: &Path, pair_id: &str) -> Option<SyncReport> {
    ufm_sync::load_json_or_default::<Option<SyncReport>>(&report_path(base, pair_id))
        .ok()
        .flatten()
}

pub fn stamp_now() -> String {
    chrono::Utc::now().format("%Y-%m-%d-%H%M%S").to_string()
}

/// Governor check-in seam (DESIGN §4): sync runs are heavy work and
/// will register with governord before their first step once the
/// Phase 2 wiring lands (with scheduled/on-connect triggers). In
/// Phase 1 runs are user-pressed and already serialize through the
/// jobs panel's pause/cancel, so the seam is a deliberate no-op that
/// marks the exact integration point.
pub fn governor_checkin(_pair_name: &str) -> Result<(), String> {
    Ok(())
}

// ---------------------------------------------------------------------------
// Views for the dialog
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairView {
    pub pair: SyncPair,
    pub a_available: bool,
    pub b_available: bool,
    pub last_summary: Option<String>,
}

pub fn pair_view(base: &Path, pair: SyncPair) -> PairView {
    let last_summary = load_report(base, &pair.id).map(|r| r.summary());
    PairView {
        a_available: Path::new(&pair.root_a).is_dir(),
        b_available: Path::new(&pair.root_b).is_dir(),
        pair,
        last_summary,
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanActionView {
    /// copy | delete | preserve | restore
    pub kind: String,
    pub rel: String,
    /// Human line for the preview list.
    pub line: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncPlanView {
    pub pair_id: String,
    pub actions: Vec<PlanActionView>,
    pub conflicts: Vec<ufm_sync::ConflictNote>,
    pub skipped: Vec<ufm_sync::SkipNote>,
    pub unchanged: usize,
    pub copies_a_to_b: usize,
    pub copies_b_to_a: usize,
    pub deletions: usize,
    pub summary: String,
}

pub fn plan_view(pair: &SyncPair, plan: &Plan) -> SyncPlanView {
    let mut actions = Vec::new();
    let (mut ab, mut ba, mut del) = (0usize, 0usize, 0usize);
    for a in &plan.actions {
        let (kind, line) = match a {
            PlanAction::Copy { from, rel } => {
                if *from == Side::A {
                    ab += 1;
                } else {
                    ba += 1;
                }
                (
                    "copy",
                    format!(
                        "{} → {}: {rel}",
                        from.label(),
                        from.other().label()
                    ),
                )
            }
            PlanAction::Delete { side, rel } => {
                del += 1;
                (
                    "delete",
                    format!("delete from {} (to Trash): {rel}", side.label()),
                )
            }
            PlanAction::Preserve { loser, rel } => (
                "preserve",
                format!(
                    "preserve {} copy of {rel} in {}/{}/",
                    loser.label(),
                    ufm_sync::CONFLICTS_DIR,
                    plan.stamp
                ),
            ),
            PlanAction::Restore { survivor, rel } => (
                "restore",
                format!(
                    "restore {rel} from {} (deleted on the other side, edited here)",
                    survivor.label()
                ),
            ),
        };
        let rel = match a {
            PlanAction::Copy { rel, .. }
            | PlanAction::Delete { rel, .. }
            | PlanAction::Preserve { rel, .. }
            | PlanAction::Restore { rel, .. } => rel.clone(),
        };
        actions.push(PlanActionView {
            kind: kind.to_string(),
            rel,
            line,
        });
    }
    let summary = if plan.actions.is_empty() {
        "Already in sync — nothing to do.".to_string()
    } else {
        format!(
            "{} change{}: {ab} A → B · {ba} B → A · {del} deletion{} · {} conflict{}",
            plan.actions.len(),
            if plan.actions.len() == 1 { "" } else { "s" },
            if del == 1 { "" } else { "s" },
            plan.conflicts.len(),
            if plan.conflicts.len() == 1 { "" } else { "s" },
        )
    };
    SyncPlanView {
        pair_id: pair.id.clone(),
        actions,
        conflicts: plan.conflicts.clone(),
        skipped: plan.skipped.clone(),
        unchanged: plan.unchanged,
        copies_a_to_b: ab,
        copies_b_to_a: ba,
        deletions: del,
        summary,
    }
}

// ---------------------------------------------------------------------------
// Run sidecar + the per-step manifest commit hook
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectKind {
    /// Copy/Restore landed: fingerprint the destination, set the entry.
    SetEntry,
    /// Propagated deletion landed: drop the entry.
    RemoveEntry,
    /// Conflict loser preserved: no manifest effect.
    PreserveOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepEffect {
    pub rel: String,
    pub kind: EffectKind,
    /// Destination root for SetEntry (the side the file landed on).
    pub dest_root: Option<String>,
    /// Report bucket: a2b | b2a | del_a | del_b | restored | preserve
    pub bucket: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RunSidecar {
    pub pair_id: String,
    pub pair_name: String,
    pub root_a: String,
    pub root_b: String,
    pub plan: Plan,
    pub effects: HashMap<String, StepEffect>,
    /// step_id → done | skipped | failed (hook bookkeeping)
    pub states: HashMap<String, String>,
    pub started: String,
}

pub fn write_sidecar(base: &Path, job_id: &str, sidecar: &RunSidecar) -> Result<(), String> {
    ufm_sync::save_json_atomic(&sidecar_path(base, job_id), sidecar).map_err(|e| e.to_string())
}

/// The executor hook: called from run_job's step-outcome arms beside
/// catalog_follow_completed_step. Applies the step's manifest effect
/// (only on done/skipped — a failed step never advances the base) and,
/// once every effect step is terminal, writes the pair's last report
/// and retires the sidecar. Silent no-op for non-sync jobs (no
/// sidecar file = not ours).
pub fn follow_step(base: &Path, job_id: &str, step_id: &str, outcome: &str) {
    let path = sidecar_path(base, job_id);
    if !path.exists() {
        return; // not a sync job — the overwhelmingly common case
    }
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(mut sidecar) = serde_json::from_str::<RunSidecar>(&text) else {
        return;
    };
    let Some(effect) = sidecar.effects.get(step_id).cloned() else {
        return; // a step of the job, but not a manifest-effect step
    };
    sidecar
        .states
        .insert(step_id.to_string(), outcome.to_string());

    // Executor semantics — re-anchored at the Folder Sync ship onto
    // the post-multitrash executor, which CONTINUES past a failed
    // step: independent steps still run and report their own
    // outcomes, and the failed step's dependents arrive afterwards
    // as "skipped". So a failure is recorded as-is — deliberately no
    // finalize-early here (the fail-fast draft of this hook marked
    // every un-run effect step failed on the first failure, which
    // would retire the sidecar mid-run and strand the commits of
    // steps that had not run yet). Finalization below simply waits
    // until every effect step is terminal. A step failure never
    // advances the manifest for its file (see the done/skipped arm);
    // the next run retries it, and the report names it.

    if outcome == "done" || outcome == "skipped" {
        if let Ok(mut manifest) = load_manifest(base, &sidecar.pair_id) {
            let applied = match &effect.kind {
                EffectKind::SetEntry => {
                    let dest = effect
                        .dest_root
                        .as_deref()
                        .map(|r| Path::new(r).join(&effect.rel));
                    match dest.and_then(|d| ufm_sync::fingerprint_file(&d).ok()) {
                        Some(fp) => {
                            manifest.set(&effect.rel, fp);
                            true
                        }
                        None => false,
                    }
                }
                EffectKind::RemoveEntry => {
                    manifest.remove(&effect.rel);
                    true
                }
                EffectKind::PreserveOnly => true,
            };
            if applied {
                let _ = save_manifest(base, &sidecar.pair_id, &manifest);
            }
        }
    }

    let all_terminal = sidecar
        .effects
        .keys()
        .all(|id| sidecar.states.contains_key(id));
    if all_terminal {
        let report = build_report(&sidecar);
        let _ = ufm_sync::save_json_atomic(&report_path(base, &sidecar.pair_id), &report);
        let _ = std::fs::remove_file(&path);
    } else {
        let _ = write_sidecar(base, job_id, &sidecar);
    }
}

/// Assemble the run's report from per-step outcomes (the plan says
/// what was intended; the states say what actually happened).
fn build_report(sidecar: &RunSidecar) -> SyncReport {
    let mut report = SyncReport {
        pair_id: sidecar.pair_id.clone(),
        pair_name: sidecar.pair_name.clone(),
        started: sidecar.started.clone(),
        finished: Some(chrono::Utc::now().to_rfc3339()),
        conflicts: sidecar.plan.conflicts.clone(),
        skipped: sidecar.plan.skipped.clone(),
        unchanged: sidecar.plan.unchanged,
        ..Default::default()
    };
    for (step_id, effect) in &sidecar.effects {
        let state = sidecar.states.get(step_id).map(|s| s.as_str());
        match state {
            Some("done") | Some("skipped") => match effect.bucket.as_str() {
                "a2b" => report.copied_a_to_b.push(effect.rel.clone()),
                "b2a" => report.copied_b_to_a.push(effect.rel.clone()),
                "del_a" => report.deleted_from_a.push(effect.rel.clone()),
                "del_b" => report.deleted_from_b.push(effect.rel.clone()),
                "restored" => report.restored.push(effect.rel.clone()),
                _ => {}
            },
            _ => {
                if effect.bucket != "preserve" {
                    report
                        .failures
                        .push(format!("{} ({})", effect.rel, effect.bucket));
                }
            }
        }
    }
    for v in [
        &mut report.copied_a_to_b,
        &mut report.copied_b_to_a,
        &mut report.deleted_from_a,
        &mut report.deleted_from_b,
        &mut report.restored,
    ] {
        v.sort();
    }
    report
}
