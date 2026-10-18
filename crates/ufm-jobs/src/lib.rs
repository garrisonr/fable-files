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
    /// ROM media enrichment: fetch one media file for one identified
    /// ROM from the source cascade and file it in the canonical media
    /// tree. The step is deliberately self-contained data: no URLs
    /// (the cascade resolves them at execution time), no destination
    /// path (recomputed from the media settings at execution time),
    /// and NEVER any credential: secrets are read from the OS keyring
    /// by the executor, never written into a job snapshot.
    FetchMedia {
        /// Catalog item id of the ROM (string form of ItemId).
        item_id: String,
        /// Fable dataset system key (e.g. "nes", "lynx").
        system: String,
        /// DAT display name (e.g. "Contra (USA)").
        dat_name: String,
        /// ROM filename stem: the storage name in the media tree.
        rom_stem: String,
        /// boxart | screenshot | titleshot | trailer
        media_type: String,
        /// Identification hashes from the DAT pass (hex), when known.
        crc: Option<String>,
        md5: Option<String>,
        sha1: Option<String>,
        /// The ROM file size in bytes, when known (ScreenScraper asks
        /// for it alongside the hashes).
        size: Option<u64>,
    },
    /// Phase 6: write a small text file into an existing folder — the
    /// one mutation a WASM plugin may cause, and only as a confirmed
    /// proposal (see ufm-plugins). The step is self-contained data:
    /// the full contents ride in the job snapshot, capped by the
    /// planner (64 KiB). The executor refuses to overwrite an existing
    /// file. The job is journaled like every other job; the file it
    /// creates is an ordinary file (trashable in-app).
    WriteFile {
        parent: Item,
        name: String,
        contents: String,
    },
    /// Media Completion: fetch TMDB artwork sidecars for one
    /// resolved video (poster.jpg / fanart.jpg / season posters, per
    /// the naming all three servers read). Data-only like FetchMedia:
    /// no URLs (the TMDB details call resolves image paths at
    /// execution time), no credentials (the TMDB key, when one is
    /// needed for the details call, is read from the OS keyring by
    /// the executor). `video_path` is where the video lives WHEN THE
    /// STEP RUNS — inside an Organize job that is the post-move path,
    /// and the step depends on that move. The executor never
    /// overwrites existing local artwork.
    FetchArtwork {
        /// Absolute path of the video file at execution time.
        video_path: String,
        /// "movie" | "episode" (catalog media_kind vocabulary).
        media_type: String,
        /// TMDB id of the confirmed match.
        tmdb_id: i64,
        /// Season number for episodes (season poster); None for movies.
        season: Option<u32>,
        /// Also fetch the backdrop as fanart.jpg (movies / show root).
        fanart: bool,
    },
    /// Media Completion: fetch one subtitle from OpenSubtitles for
    /// one resolved video and file it as a server sidecar
    /// (`<stem>.<lang>.srt`, SDH/forced labelled). Matching is by
    /// MOVIE HASH first (ufm_media::oshash), TMDB id / name as
    /// fallback — all resolved at execution time; the user's
    /// OpenSubtitles credentials are read from the OS keyring by the
    /// executor and never enter a job snapshot. Existing subtitles
    /// are never overwritten.
    FetchSubtitle {
        /// Absolute path of the video file at execution time.
        video_path: String,
        /// Preferred subtitle language code ("en" default).
        language: String,
        /// "movie" | "episode".
        media_type: String,
        /// TMDB id of the confirmed match, when known (fallback
        /// search key).
        tmdb_id: Option<i64>,
        /// Season / episode numbers for TV fallback search.
        season: Option<u32>,
        episode: Option<u32>,
    },
    /// File Auditor: convert one media file beside itself with
    /// ffmpeg — the execution arm of the Auditor's space-saving
    /// proposals, always a separately confirmed job (the Auditor
    /// proposes; the user disposes). `kind`: "flac" (WAV → FLAC,
    /// lossless), "hevc" (H.264 → H.265 in an .mkv, a generational
    /// re-encode), "png" (BMP/TIFF → PNG, lossless). The output name
    /// rides in the step so the plan the user confirmed is the plan
    /// that runs; the executor never overwrites an existing output,
    /// and its Done note records `before=<bytes> after=<bytes>` so
    /// the job reports the true saving. Replacing the original is
    /// the PLANNER's choice, expressed as a separate dependent
    /// Delete{use_trash:true} step — this step only converts.
    ConvertMedia {
        src: Item,
        dst_name: String,
        /// "flac" | "hevc" | "png"
        kind: String,
    },
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
    /// Indexer v2 (catalog hygiene): step tallies captured when a
    /// TERMINAL job's snapshot is slimmed — the steps map (the replay
    /// payload, only ever needed to resume an unfinished job) is
    /// dropped and these counts keep the job card truthful.
    /// `serde(default)` keeps older snapshots loadable.
    #[serde(default)]
    pub step_counts: Option<StepCounts>,
    /// True once this snapshot has been slimmed (see `step_counts`).
    /// A slimmed job is terminal by construction — recovery only
    /// loads unfinished jobs, and unfinished jobs are never slimmed.
    #[serde(default)]
    pub steps_slimmed: bool,
    /// Grand Pass steal 6: this job's name-collision policy, chosen
    /// at start. `serde(default)` = AutoRename for older snapshots.
    #[serde(default)]
    pub collision_policy: CollisionPolicy,
    /// Grand Pass steal 6: per-step collision marks by step id —
    /// Awaiting (the executor parks before this step until the
    /// user decides) / Replace (the user consented to this step
    /// taking the existing name). Lives on the job, not the step,
    /// so every existing step literal and journal row is untouched.
    #[serde(default)]
    pub collision_marks: std::collections::HashMap<String, CollisionMark>,
}

/// Tallies of a job's steps by state, recorded at slimming time so a
/// slimmed snapshot still reports truthful totals.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepCounts {
    pub total: u64,
    pub done: u64,
    pub skipped: u64,
    pub failed: u64,
    pub pending: u64,
}

impl StepCounts {
    /// Tally a steps map (used by the slimming pass and by summaries
    /// that must agree with it).
    pub fn tally(steps: &HashMap<String, Step>) -> Self {
        let mut c = Self {
            total: steps.len() as u64,
            ..Default::default()
        };
        for s in steps.values() {
            match s.state {
                StepState::Done => c.done += 1,
                StepState::Skipped => c.skipped += 1,
                StepState::Failed => c.failed += 1,
                StepState::Pending | StepState::Running => c.pending += 1,
            }
        }
        c
    }
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

/// Grand Pass steal 6: what a job does when a destination name is
/// already taken. `AutoRename` is the founding doctrine — a job
/// NEVER silently overwrites; the incoming file lands as
/// "name 2" — and it stays the default. `AskMe` instead parks the
/// job on the colliding item and lets the user choose per item:
/// Replace (the app's only overwrite, always explicit), KeepBoth
/// (the auto-rename), or Skip.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollisionPolicy {
    #[default]
    AutoRename,
    AskMe,
}

/// One step's collision state (steal 6), keyed by step id in
/// `Job::collision_marks`: the step is parked awaiting the user's
/// call, or the user said Replace and the step may take the name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollisionMark {
    Awaiting,
    Replace,
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
            step_counts: None,
            steps_slimmed: false,
            collision_policy: CollisionPolicy::AutoRename,
            collision_marks: std::collections::HashMap::new(),
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
    #[test]
    fn fetch_media_step_round_trips() {
        let step = Step {
            id: "s1".into(),
            label: "NES - Contra (USA) - box art".into(),
            op: StepOp::FetchMedia {
                item_id: "item-1".into(),
                system: "nes".into(),
                dat_name: "Contra (USA)".into(),
                rom_stem: "Contra (USA)".into(),
                media_type: "boxart".into(),
                crc: Some("1234abcd".into()),
                md5: None,
                sha1: Some("00112233445566778899aabbccddeeff00112233".into()),
                size: Some(24592),
            },
            depends_on: vec![],
            state: StepState::Pending,
            compensation: None,
            outcome: false,
            outcome_path: None,
        };
        let json = serde_json::to_string(&step).unwrap();
        let back: Step = serde_json::from_str(&json).unwrap();
        match back.op {
            StepOp::FetchMedia { ref system, ref media_type, .. } => {
                assert_eq!(system, "nes");
                assert_eq!(media_type, "boxart");
            }
            _ => panic!("wrong variant after round trip"),
        }
        // Older snapshots (no FetchMedia anywhere) still load: the
        // enum is serde-tagged, so a Copy step JSON is unaffected.
        let old = r#"{"id":"s0","label":"c","op":{"op":"mkdir","parent":{"id":"00000000-0000-0000-0000-000000000000","location":"00000000-0000-0000-0000-000000000000","name":"x","kind":"folder","size_bytes":null,"modified":null,"provider_key":null,"labels":[],"starred":false},"name":"y"},"depends_on":[],"state":"pending","compensation":null}"#;
        assert!(serde_json::from_str::<Step>(old).is_ok());
    }

    #[test]
    fn fetch_artwork_and_subtitle_steps_round_trip() {
        // Media Completion: the two fetch steps are data-only
        // snapshots (no URLs, no credentials) and must survive the
        // journal round trip exactly.
        let art = Step {
            id: "sa".into(),
            label: "artwork".into(),
            op: StepOp::FetchArtwork {
                video_path: "/lib/Dune (2021)/Dune (2021).mkv".into(),
                media_type: "movie".into(),
                tmdb_id: 438631,
                season: None,
                fanart: true,
            },
            depends_on: vec!["mv1".into()],
            state: StepState::Pending,
            compensation: None,
            outcome: false,
            outcome_path: None,
        };
        let json = serde_json::to_string(&art).unwrap();
        assert!(json.contains("\"op\":\"fetch_artwork\""), "tag: {json}");
        let back: Step = serde_json::from_str(&json).unwrap();
        match back.op {
            StepOp::FetchArtwork { ref media_type, tmdb_id, fanart, .. } => {
                assert_eq!(media_type, "movie");
                assert_eq!(tmdb_id, 438631);
                assert!(fanart);
            }
            _ => panic!("wrong variant after round trip"),
        }

        let sub = Step {
            id: "ss".into(),
            label: "subtitle".into(),
            op: StepOp::FetchSubtitle {
                video_path: "/lib/Show (2020)/Season 01/Show (2020) - S01E02.mkv".into(),
                language: "en".into(),
                media_type: "episode".into(),
                tmdb_id: Some(95396),
                season: Some(1),
                episode: Some(2),
            },
            depends_on: vec![],
            state: StepState::Pending,
            compensation: None,
            outcome: false,
            outcome_path: None,
        };
        let json = serde_json::to_string(&sub).unwrap();
        assert!(json.contains("\"op\":\"fetch_subtitle\""), "tag: {json}");
        let back: Step = serde_json::from_str(&json).unwrap();
        match back.op {
            StepOp::FetchSubtitle { ref language, season, episode, .. } => {
                assert_eq!(language, "en");
                assert_eq!(season, Some(1));
                assert_eq!(episode, Some(2));
            }
            _ => panic!("wrong variant after round trip"),
        }
    }

    #[test]
    fn convert_media_step_roundtrips() {
        let step = Step {
            id: "cv1".into(),
            label: "convert to FLAC".into(),
            op: StepOp::ConvertMedia {
                src: item("song", ItemKind::File),
                dst_name: "song.flac".into(),
                kind: "flac".into(),
            },
            depends_on: vec![],
            state: StepState::Pending,
            compensation: None,
            outcome: false,
            outcome_path: None,
        };
        let json = serde_json::to_string(&step).unwrap();
        assert!(json.contains("\"op\":\"convert_media\""), "tag: {json}");
        let back: Step = serde_json::from_str(&json).unwrap();
        match back.op {
            StepOp::ConvertMedia { ref dst_name, ref kind, .. } => {
                assert_eq!(dst_name, "song.flac");
                assert_eq!(kind, "flac");
            }
            _ => panic!("wrong variant after round trip"),
        }
    }

    #[test]
    fn write_file_step_roundtrips() {
        let step = Step {
            id: "s0001".into(),
            label: "write note".into(),
            op: StepOp::WriteFile {
                parent: item("dest", ItemKind::Folder),
                name: "game.fable-note.txt".into(),
                contents: "Title: game\n".into(),
            },
            depends_on: vec![],
            state: StepState::Pending,
            compensation: None,
            outcome: false,
            outcome_path: None,
        };
        let json = serde_json::to_string(&step).unwrap();
        assert!(json.contains("\"op\":\"write_file\""), "tag: {json}");
        let back: Step = serde_json::from_str(&json).unwrap();
        match back.op {
            StepOp::WriteFile { ref name, ref contents, .. } => {
                assert_eq!(name, "game.fable-note.txt");
                assert_eq!(contents, "Title: game\n");
            }
            _ => panic!("wrong variant after round trip"),
        }
    }

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
    fn step_counts_default_and_tally() {
        // Indexer v2 hygiene: snapshots written before slimming
        // existed load with no counts and steps_slimmed = false.
        let job = sample_job();
        assert_eq!(job.step_counts, None);
        assert!(!job.steps_slimmed);
        let mut jv = serde_json::to_value(&job).unwrap();
        jv.as_object_mut().unwrap().remove("step_counts");
        jv.as_object_mut().unwrap().remove("steps_slimmed");
        let jback: Job = serde_json::from_value(jv).unwrap();
        assert_eq!(jback.step_counts, None);
        assert!(!jback.steps_slimmed);

        // Tally: sample_job has s1 Pending, s2 Pending, s3 Pending.
        let counts = StepCounts::tally(&job.steps);
        assert_eq!(counts.total, 3);
        assert_eq!(counts.pending, 3);
        assert_eq!(counts.done, 0);
        let mut job2 = sample_job();
        job2.steps.get_mut("s1").unwrap().state = StepState::Done;
        job2.steps.get_mut("s2").unwrap().state = StepState::Skipped;
        job2.steps.get_mut("s3").unwrap().state = StepState::Failed;
        let counts = StepCounts::tally(&job2.steps);
        assert_eq!(
            counts,
            StepCounts { total: 3, done: 1, skipped: 1, failed: 1, pending: 0 }
        );
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

    #[test]
    fn collision_policy_defaults_and_roundtrips() {
        // Steal 6: a pre-pass snapshot (no collision fields at all)
        // loads as AutoRename with no marks — the doctrine default.
        let mut job = Job::new("legacy", JobPreview::default());
        job.add_step(step("s1", StepOp::Mkdir {
            parent: item("dst", ItemKind::Folder),
            name: "x".into(),
        }, &[]));
        let mut json = serde_json::to_value(&job).unwrap();
        let obj = json.as_object_mut().unwrap();
        obj.remove("collision_policy");
        obj.remove("collision_marks");
        let restored: Job = serde_json::from_value(json).unwrap();
        assert_eq!(restored.collision_policy, CollisionPolicy::AutoRename);
        assert!(restored.collision_marks.is_empty());

        // And an AskMe job with a parked step round-trips intact.
        let mut job2 = Job::new("ask", JobPreview::default());
        job2.collision_policy = CollisionPolicy::AskMe;
        job2.collision_marks
            .insert("s0007".into(), CollisionMark::Awaiting);
        let back: Job =
            serde_json::from_str(&serde_json::to_string(&job2).unwrap()).unwrap();
        assert_eq!(back.collision_policy, CollisionPolicy::AskMe);
        assert_eq!(
            back.collision_marks.get("s0007"),
            Some(&CollisionMark::Awaiting)
        );
    }
}
