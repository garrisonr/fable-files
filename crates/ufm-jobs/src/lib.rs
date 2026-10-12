//! ufm-jobs: the operation engine.
//!
//! Spec §§9, 25: file operations are a **persistent DAG of resumable steps**
//! with progress and compensating actions — the TeraCopy-style queued,
//! pausable, verified, conflict-aware transfer center. Every job journals
//! atomically and can resume after a crash, a reboot, or a killed process.
//! (Standing build rule: checkpoint from the first version, never bolted on.)

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
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

pub struct JobJournal {
    entries: Vec<JournalEntry>,
}

impl JobJournal {
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    pub fn record(&mut self, job: JobId, step: &str, from: StepState, to: StepState) {
        self.entries.push(JournalEntry {
            job,
            step: step.to_string(),
            from,
            to,
            at: now_utc(),
            note: None,
        });
        // TODO(spec §25): persist to the catalog DB (jobs table) with fsync.
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
