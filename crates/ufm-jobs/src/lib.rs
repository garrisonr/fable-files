//! ufm-jobs: the operation engine.
//!
//! Spec §§9, 25: file operations are a **persistent DAG of resumable steps**
//! with progress and compensating actions — the TeraCopy-style queued,
//! pausable, verified, conflict-aware transfer center. Every job journals
//! atomically and can resume after a crash, a reboot, or a killed process.
//! (Standing build rule: checkpoint from the first version, never bolted on.)

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use ufm_core::{now_utc, JobId, Result, UfmError};
use ufm_vfs::Item;

// ---------------------------------------------------------------------------
// Steps — the unit of resumable work. A step is idempotent: re-running it
// after a crash must be safe (verify-then-skip, never blind re-copy).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub id: String,
    pub label: String,
    pub op: StepOp,
    /// Steps that must complete before this one may start (the DAG edges).
    pub depends_on: Vec<String>,
    pub state: StepState,
    /// Compensating action recorded when the step completes (for undo).
    pub compensation: Option<StepOp>,
    /// Inc-2: this step is the outcome step for its top-level source —
    /// its completion means the source fully arrived (move) or is fully
    /// gone / trashed (delete), so the executor records a `JobOutcome`.
    /// `serde(default)` keeps inc-1 job snapshots loadable.
    #[serde(default)]
    pub outcome: bool,
    /// Inc-2: the result path to record in the outcome when it cannot be
    /// derived from the op (decomposed cross-device moves).
    #[serde(default)]
    pub outcome_path: Option<String>,
}

/// Inc-2: the durable, per-source record of what a job actually did —
/// the undo feed. Lives in the job snapshot so it survives the app
/// being closed mid-job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobOutcome {
    pub step_id: String,
    pub src_path: String,
    /// Final absolute path for a fully moved source.
    pub result_path: Option<String>,
    /// Trash entry name (the `files/` name) for Delete-to-trash steps.
    pub trash_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum StepOp {
    Copy { src: Item, dst_parent: Item, new_name: String },
    Move { src: Item, dst_parent: Item, new_name: String },
    Delete { target: Item, use_trash: bool },
    Mkdir { parent: Item, name: String },
    /// Verify-then-skip: checks size/hash at the destination before copying.
    VerifyCopy { src: Item, dst: Item },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    Pending,
    Running,
    Done,
    Skipped, // verify-then-skip hit: destination already correct
    Failed,
}

// ---------------------------------------------------------------------------
// Job — the DAG plus its journal. The journal is the source of truth for
// resume; the in-memory queue is rebuilt from it on startup.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: JobId,
    pub title: String,
    pub steps: HashMap<String, Step>,
    pub created: chrono::DateTime<chrono::Utc>,
    pub state: JobState,
    /// Human-readable effect preview, computed before the first step runs
    /// (spec §6: power features preview their effect and reveal conflicts).
    pub preview: JobPreview,
    /// Inc-2: per-source outcomes, appended by the executor as outcome
    /// steps complete. `serde(default)` keeps inc-1 snapshots loadable.
    #[serde(default)]
    pub outcomes: Vec<JobOutcome>,
    /// UI round (packed transfer): the executor streamed this job's
    /// small-file mass through the tar pipe instead of per-file
    /// copies. Set by the executor when the packed pass engages;
    /// surfaced on the job card as a small "Packed transfer" label.
    /// `serde(default)` keeps older snapshots loadable.
    #[serde(default)]
    pub packed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Paused,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JobPreview {
    pub files_affected: u64,
    pub bytes_to_move: u64,
    pub conflicts: Vec<Conflict>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conflict {
    pub path: String,
    pub kind: ConflictKind,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictKind {
    NameCollision,
    ProtectedLocation,
    InsufficientSpace,
}

impl Job {
    pub fn new(title: impl Into<String>, preview: JobPreview) -> Self {
        Self {
            id: JobId::new(),
            title: title.into(),
            steps: HashMap::new(),
            created: now_utc(),
            state: JobState::Queued,
            preview,
            outcomes: Vec::new(),
            packed: false,
        }
    }

    pub fn add_step(&mut self, step: Step) {
        self.steps.insert(step.id.clone(), step);
    }

    /// Topological order of runnable steps (Kahn's algorithm). Steps whose
    /// dependencies are all Done/Skipped come out first.
    pub fn runnable_order(&self) -> Result<Vec<String>> {
        let mut indegree: HashMap<&str, usize> = HashMap::new();
        for step in self.steps.values() {
            indegree.entry(step.id.as_str()).or_insert(0);
            for dep in &step.depends_on {
                if !self.steps.contains_key(dep) {
                    return Err(UfmError::InvalidInput(format!(
                        "step {} depends on unknown step {dep}",
                        step.id
                    )));
                }
                *indegree.entry(step.id.as_str()).or_insert(0) += 1;
            }
        }
        let mut queue: VecDeque<&str> = indegree
            .iter()
            .filter(|(_, &d)| d == 0)
            .map(|(&id, _)| id)
            .collect();
        let mut order = Vec::new();
        while let Some(id) = queue.pop_front() {
            order.push(id.to_string());
            for step in self.steps.values() {
                if step.depends_on.iter().any(|d| d == id) {
                    let e = indegree.get_mut(step.id.as_str()).unwrap();
                    *e -= 1;
                    if *e == 0 {
                        queue.push_back(step.id.as_str());
                    }
                }
            }
        }
        if order.len() != self.steps.len() {
            return Err(UfmError::InvalidInput(
                "job graph has a dependency cycle".into(),
            ));
        }
        Ok(order)
    }
}

// ---------------------------------------------------------------------------
// Journal — append-only, fsync'd per step transition. Resume = replay the
// journal, mark Done steps, re-queue the rest. Temp file + rename, never
// truncate-then-write (standing data-safety rule).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub job: JobId,
    pub step: String,
    pub from: StepState,
    pub to: StepState,
    pub at: chrono::DateTime<chrono::Utc>,
    pub note: Option<String>,
}

/// Persistence target for journal entries. Implemented by ufm-catalog
/// (SQLite, the `job_journal` table) and by tests (in-memory). A trait so
/// ufm-jobs stays storage-agnostic: the engine defines WHAT is journaled,
/// the catalog owns WHERE it lands.
pub trait JournalSink: Send + Sync {
    fn record(&self, entry: &JournalEntry) -> Result<()>;
}

pub struct JobJournal {
    entries: Vec<JournalEntry>,
    sink: Option<Arc<dyn JournalSink>>,
}

impl JobJournal {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            sink: None,
        }
    }

    /// A journal that also persists every record through `sink`
    /// (spec §25: the journal is durable, fsync'd by the store).
    /// Persistence is best-effort — a sink failure never kills an
    /// operation mid-flight; the executor additionally rewrites the
    /// whole-job snapshot in the catalog on every transition, so a lost
    /// journal row can never strand a job.
    pub fn with_sink(sink: Arc<dyn JournalSink>) -> Self {
        Self {
            entries: Vec::new(),
            sink: Some(sink),
        }
    }

    /// A journal pre-loaded with persisted entries, for crash replay.
    pub fn from_entries(entries: Vec<JournalEntry>) -> Self {
        Self {
            entries,
            sink: None,
        }
    }

    pub fn entries(&self) -> &[JournalEntry] {
        &self.entries
    }

    pub fn record(&mut self, job: JobId, step: &str, from: StepState, to: StepState) {
        self.record_with_note(job, step, from, to, None);
    }

    pub fn record_with_note(
        &mut self,
        job: JobId,
        step: &str,
        from: StepState,
        to: StepState,
        note: Option<String>,
    ) {
        let entry = JournalEntry {
            job,
            step: step.to_string(),
            from,
            to,
            at: now_utc(),
            note,
        };
        if let Some(sink) = &self.sink {
            let _ = sink.record(&entry);
        }
        self.entries.push(entry);
    }

    /// Rebuild step states after a restart. Steps left `Running` at crash time
    /// go back to `Pending` — they are idempotent by contract.
    pub fn replay_into(&self, job: &mut Job) {
        for e in &self.entries {
            if let Some(step) = job.steps.get_mut(&e.step) {
                step.state = match e.to {
                    StepState::Running => StepState::Pending, // crash recovery
                    other => other,
                };
            }
        }
    }
}

impl Default for JobJournal {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use ufm_core::{ItemId, LocationId};
    use ufm_vfs::ItemKind;

    #[derive(Default)]
    struct VecSink {
        entries: Mutex<Vec<JournalEntry>>,
    }

    impl JournalSink for VecSink {
        fn record(&self, entry: &JournalEntry) -> Result<()> {
            self.entries.lock().unwrap().push(entry.clone());
            Ok(())
        }
    }

    fn item(name: &str, kind: ItemKind) -> Item {
        Item {
            id: ItemId::new(),
            location: LocationId::new(),
            name: name.into(),
            kind,
            size_bytes: Some(10),
            modified: None,
            provider_key: Some(format!("/tmp/{name}")),
            labels: vec![],
            starred: false,
        }
    }

    fn step(id: &str, op: StepOp, deps: &[&str]) -> Step {
        Step {
            id: id.into(),
            label: id.into(),
            op,
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            state: StepState::Pending,
            compensation: None,
            outcome: false,
            outcome_path: None,
        }
    }

    fn sample_job() -> Job {
        let mut job = Job::new("test", JobPreview::default());
        let dir = item("dest", ItemKind::Folder);
        let src = item("a.txt", ItemKind::File);
        job.add_step(step(
            "s1",
            StepOp::Mkdir {
                parent: dir.clone(),
                name: "sub".into(),
            },
            &[],
        ));
        job.add_step(step(
            "s2",
            StepOp::VerifyCopy {
                src: src.clone(),
                dst: item("a.txt", ItemKind::File),
            },
            &[],
        ));
        job.add_step(step(
            "s3",
            StepOp::Copy {
                src,
                dst_parent: dir,
                new_name: "a.txt".into(),
            },
            &["s1", "s2"],
        ));
        job
    }

    #[test]
    fn runnable_order_respects_dependencies() {
        let job = sample_job();
        let order = job.runnable_order().unwrap();
        let pos = |id: &str| order.iter().position(|s| s == id).unwrap();
        assert!(pos("s1") < pos("s3"));
        assert!(pos("s2") < pos("s3"));
        assert_eq!(order.len(), 3);
    }

    #[test]
    fn journal_forwards_to_sink_and_keeps_entries() {
        let sink = Arc::new(VecSink::default());
        let mut journal = JobJournal::with_sink(sink.clone());
        let job = JobId::new();
        journal.record(job, "s1", StepState::Pending, StepState::Running);
        journal.record_with_note(
            job,
            "s1",
            StepState::Running,
            StepState::Failed,
            Some("disk full".into()),
        );
        assert_eq!(journal.entries().len(), 2);
        let sunk = sink.entries.lock().unwrap();
        assert_eq!(sunk.len(), 2);
        assert_eq!(sunk[1].note.as_deref(), Some("disk full"));
        assert_eq!(sunk[0].to, StepState::Running);
    }

    #[test]
    fn replay_restores_done_and_resets_running() {
        let mut journal = JobJournal::new();
        let mut job = sample_job();
        let id = job.id;
        journal.record(id, "s1", StepState::Pending, StepState::Running);
        journal.record(id, "s1", StepState::Running, StepState::Done);
        journal.record(id, "s2", StepState::Pending, StepState::Running);
        // "Crash" here: s2 was Running, s3 never started.
        let replayed = JobJournal::from_entries(journal.entries().to_vec());
        let mut restored = sample_job();
        restored.id = id;
        replayed.replay_into(&mut restored);
        assert_eq!(restored.steps["s1"].state, StepState::Done);
        assert_eq!(restored.steps["s2"].state, StepState::Pending); // reset
        assert_eq!(restored.steps["s3"].state, StepState::Pending);
    }

    #[test]
    fn job_outcomes_default_empty_and_step_outcome_defaults_false() {
        // Upgrade safety (inc-2 §4.1): a Job built in code has no
        // outcomes, and an inc-1-era step snapshot — serialized before
        // the outcome fields existed — still deserializes with the
        // defaults. Crash recovery loads these from users' catalog DBs.
        let job = sample_job();
        assert!(job.outcomes.is_empty());

        let s = step(
            "sx",
            StepOp::Delete {
                target: item("d.txt", ItemKind::File),
                use_trash: true,
            },
            &[],
        );
        let mut v = serde_json::to_value(&s).unwrap();
        v.as_object_mut().unwrap().remove("outcome");
        v.as_object_mut().unwrap().remove("outcome_path");
        let back: Step = serde_json::from_value(v).unwrap();
        assert!(!back.outcome);
        assert_eq!(back.outcome_path, None);

        let mut jv = serde_json::to_value(&job).unwrap();
        jv.as_object_mut().unwrap().remove("outcomes");
        let jback: Job = serde_json::from_value(jv).unwrap();
        assert!(jback.outcomes.is_empty());
    }

    #[test]
    fn job_packed_defaults_false_and_deserializes_without_field() {
        // UI round (packed transfer): same upgrade-safety contract as
        // outcomes — a snapshot written before the field existed
        // loads with packed = false.
        let job = sample_job();
        assert!(!job.packed);
        let mut jv = serde_json::to_value(&job).unwrap();
        jv.as_object_mut().unwrap().remove("packed");
        let jback: Job = serde_json::from_value(jv).unwrap();
        assert!(!jback.packed);
    }

    #[test]
    fn replay_resets_running_delete_step() {
        // Inc-2 §4.2: the replay contract (Running -> Pending) covers
        // the new Delete op exactly like Copy.
        let mk = || {
            step(
                "s1",
                StepOp::Delete {
                    target: item("gone.txt", ItemKind::File),
                    use_trash: true,
                },
                &[],
            )
        };
        let mut job = Job::new("del", JobPreview::default());
        job.add_step(mk());
        let id = job.id;
        let mut journal = JobJournal::new();
        journal.record(id, "s1", StepState::Pending, StepState::Running);
        // "Crash" here: the delete was Running, never journaled Done.
        let replayed = JobJournal::from_entries(journal.entries().to_vec());
        let mut restored = Job::new("del", JobPreview::default());
        restored.id = id;
        restored.add_step(mk());
        replayed.replay_into(&mut restored);
        assert_eq!(restored.steps["s1"].state, StepState::Pending);
    }
}
