//! Listen — the read-aloud engine room (Ebook P3, Phases 3–4).
//!
//! Dickles' ruling: Kokoro fp32, buffer-ahead. The design follows
//! from the measurements (Phase 0): warm synthesis is ~2 s a
//! sentence at 1.19× real-time, so the engine synthesizes AHEAD of
//! the playhead — while sentence N plays, N+1…N+3 are already
//! voice. Chapter boundaries absorb the warm-up.
//!
//! Everything time-critical lives HERE, in Rust (Forge: webview
//! timers lie): the playback clock is rodio's queue on this
//! engine's own thread, the buffer is filled from a synthesis
//! thread, the sleep deadline is an `Instant` checked on the tick,
//! and position persistence happens on every sentence start. The
//! webview only paints events and snapshots.
//!
//! The voice itself lives in the `fable-tts` helper process
//! (spawned by ufm-tts's client on the synthesis thread): lazy by
//! construction — the engine's ~1.3 GB exists exactly while the
//! helper does. Stop, book-end, or ten paused minutes put it away.
//!
//! Playback survives the reader closing, the window hiding, and
//! the room changing: none of those are audio facts.

use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ufm_tts::client::HelperEngine;
use ufm_tts::{segment_chapter, unit_text, voice_by_id, SentenceRange, TtsEngine as _, Voice};

const LOOKAHEAD: usize = 3;
const PAUSED_UNLOAD: Duration = Duration::from_secs(600);
const TICK: Duration = Duration::from_millis(80);

// ---------------------------------------------------------------------------
// Snapshots (the UI adopts these wholesale — §AW's law).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TtsSnapshot {
    pub version: u64,
    /// idle | loading | playing | paused | stopped | error
    pub state: &'static str,
    pub path: Option<String>,
    pub title: Option<String>,
    pub chapter: Option<usize>,
    pub chapter_count: usize,
    pub sentence: Option<usize>,
    pub sentence_count: usize,
    /// Sentences synthesized and waiting ahead of the playhead.
    pub buffered: usize,
    /// Times the playhead waited on synthesis (the rig reads this;
    /// buffer-ahead should keep it at zero inside a chapter).
    pub stalls: u32,
    pub voice: String,
    pub speed: f32,
    /// off | 15 | 30 | 45 | 60 | chapter
    pub sleep: String,
    /// The plain-language state of the moment, when there's
    /// something to say ("Sleep timer — goodnight").
    pub message: Option<String>,
}

impl Default for TtsSnapshot {
    fn default() -> Self {
        TtsSnapshot {
            version: 0,
            state: "idle",
            path: None,
            title: None,
            chapter: None,
            chapter_count: 0,
            sentence: None,
            sentence_count: 0,
            buffered: 0,
            stalls: 0,
            voice: ufm_tts::DEFAULT_VOICE_ID.to_string(),
            speed: 1.0,
            sleep: "off".to_string(),
            message: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSnapshot {
    /// ready | missing | downloading | verifying | extracting | error
    pub state: &'static str,
    pub downloaded: u64,
    pub total: u64,
    pub message: Option<String>,
}

impl Default for ModelSnapshot {
    fn default() -> Self {
        ModelSnapshot {
            state: "missing",
            downloaded: 0,
            total: ufm_tts::model::ARCHIVE_BYTES,
            message: None,
        }
    }
}

// ---------------------------------------------------------------------------
// The hub: the engine thread's mailbox + the shared snapshots.
// ---------------------------------------------------------------------------

enum Cmd {
    Play {
        path: String,
        chapter: Option<usize>,
        sentence: Option<usize>,
    },
    Pause,
    Resume,
    Stop,
    NextSentence,
    PrevSentence,
    NextChapter,
    PrevChapter,
    SetVoice(String),
    SetSpeed(f32),
    SetSleep(String),
    Quit,
}

enum EngineMsg {
    Cmd(Cmd),
    Synth(SynthDone),
}

struct SynthJob {
    generation: u64,
    chapter: usize,
    sentence: usize,
    text: String,
    voice_id: String,
    speed: f32,
}

struct SynthDone {
    generation: u64,
    chapter: usize,
    sentence: usize,
    result: Result<(Vec<f32>, u32), String>,
}

enum SynthMsg {
    Job(SynthJob),
    Shutdown,
}

pub struct TtsHub {
    tx: Sender<EngineMsg>,
    snap: Arc<Mutex<TtsSnapshot>>,
    model: Arc<Mutex<ModelSnapshot>>,
    model_dir: PathBuf,
    helper: PathBuf,
}

impl TtsHub {
    pub fn start(app: tauri::AppHandle) -> Self {
        let (tx, rx) = channel::<EngineMsg>();
        let snap = Arc::new(Mutex::new(TtsSnapshot::default()));
        let model = Arc::new(Mutex::new(model_snapshot_fresh()));
        let model_dir = tts_model_dir();
        let helper = helper_path(&app);
        let hub_snap = snap.clone();
        let engine_tx = tx.clone();
        let engine_model_dir = model_dir.clone();
        let engine_helper = helper.clone();
        std::thread::Builder::new()
            .name("tts-engine".into())
            .spawn(move || {
                let mut engine = Engine::new(
                    app,
                    hub_snap,
                    engine_tx,
                    engine_helper,
                    engine_model_dir,
                );
                engine.run(rx);
            })
            .expect("tts engine thread");
        TtsHub {
            tx,
            snap,
            model,
            model_dir,
            helper,
        }
    }

    pub fn snapshot(&self) -> TtsSnapshot {
        self.snap.lock().unwrap().clone()
    }

    pub fn model_snapshot(&self) -> ModelSnapshot {
        self.model.lock().unwrap().clone()
    }

    /// App exit: the engine saves the ear mark and puts the
    /// narrator away before the process goes.
    pub fn shutdown(&self) {
        let _ = self.tx.send(EngineMsg::Cmd(Cmd::Quit));
    }

    pub fn model_dir(&self) -> PathBuf {
        self.model_dir.clone()
    }

    pub fn helper_path(&self) -> PathBuf {
        self.helper.clone()
    }

    /// Send a command and return the snapshot the engine settles
    /// into (adopt-the-returned-snapshot: poll the version briefly
    /// so the caller never renders the pre-command state).
    fn send(&self, cmd: Cmd) -> TtsSnapshot {
        let before = self.snap.lock().unwrap().version;
        let _ = self.tx.send(EngineMsg::Cmd(cmd));
        let deadline = Instant::now() + Duration::from_millis(600);
        loop {
            let snap = self.snap.lock().unwrap();
            if snap.version != before || Instant::now() > deadline {
                return snap.clone();
            }
            drop(snap);
            std::thread::sleep(Duration::from_millis(15));
        }
    }
}

// ---------------------------------------------------------------------------
// Paths.
// ---------------------------------------------------------------------------

/// The TTS store: `<data dir>/tts` — the model archive, its
/// extraction, and nothing else. Deleting the folder deletes the
/// voice (SPEC §A.7's law, applied to models).
pub fn tts_base_dir() -> PathBuf {
    if let Ok(p) = std::env::var("FABLE_TTS_DIR") {
        return PathBuf::from(p);
    }
    crate::reader::data_dir().join("tts")
}

pub fn tts_model_dir() -> PathBuf {
    ufm_tts::model::model_dir(&tts_base_dir())
}

fn model_snapshot_fresh() -> ModelSnapshot {
    let mut s = ModelSnapshot::default();
    if ufm_tts::model::model_ready(&tts_model_dir()) {
        s.state = "ready";
    }
    s
}

/// Where the narrator program lives, in the order the rig and the
/// bundle actually place it (the rom-dats candidate pattern).
pub fn helper_path(app: &tauri::AppHandle) -> PathBuf {
    use tauri::Manager;
    if let Ok(p) = std::env::var("FABLE_TTS_HELPER") {
        return PathBuf::from(p);
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(res) = app.path().resource_dir() {
        candidates.push(res.join("tts-runtime").join("fable-tts"));
        candidates.push(res.join("resources").join("tts-runtime").join("fable-tts"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("fable-tts"));
        }
    }
    candidates.push(tts_base_dir().join("tts-runtime").join("fable-tts"));
    for c in &candidates {
        if c.is_file() {
            return c.clone();
        }
    }
    candidates
        .into_iter()
        .next()
        .unwrap_or_else(|| PathBuf::from("fable-tts"))
}

// ---------------------------------------------------------------------------
// The synthesis thread: owns the helper process, speaks PCM.
// ---------------------------------------------------------------------------

fn synth_thread(
    helper: PathBuf,
    model_dir: PathBuf,
    rx: Receiver<SynthMsg>,
    out: Sender<EngineMsg>,
) {
    let mut engine: Option<HelperEngine> = None;
    while let Ok(msg) = rx.recv() {
        match msg {
            SynthMsg::Shutdown => {
                if let Some(mut e) = engine.take() {
                    e.shutdown();
                }
            }
            SynthMsg::Job(job) => {
                if engine.is_none() {
                    match HelperEngine::spawn(&helper, &model_dir, 4) {
                        Ok(e) => engine = Some(e),
                        Err(err) => {
                            let _ = out.send(EngineMsg::Synth(SynthDone {
                                generation: job.generation,
                                chapter: job.chapter,
                                sentence: job.sentence,
                                result: Err(err.to_string()),
                            }));
                            continue;
                        }
                    }
                }
                let voice: &Voice = voice_by_id(&job.voice_id)
                    .unwrap_or_else(|| voice_by_id(ufm_tts::DEFAULT_VOICE_ID).unwrap());
                let eng = engine.as_mut().expect("engine just ensured");
                let result = match eng.synthesize(&job.text, voice, job.speed) {
                    Ok(audio) => Ok((audio.samples, audio.sample_rate)),
                    Err(err) => {
                        // A helper that stumbled may be dead; drop
                        // it so the next job respawns clean.
                        engine = None;
                        Err(err.to_string())
                    }
                };
                let done = SynthDone {
                    generation: job.generation,
                    chapter: job.chapter,
                    sentence: job.sentence,
                    result,
                };
                if out.send(EngineMsg::Synth(done)).is_err() {
                    break;
                }
            }
        }
    }
    if let Some(mut e) = engine.take() {
        e.shutdown();
    }
}

// ---------------------------------------------------------------------------
// The engine: playback clock, buffer, sleep deadline, position.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Idle,
    Loading,
    Playing,
    Paused,
    Stopped,
    Error,
}

impl Status {
    fn name(self) -> &'static str {
        match self {
            Status::Idle => "idle",
            Status::Loading => "loading",
            Status::Playing => "playing",
            Status::Paused => "paused",
            Status::Stopped => "stopped",
            Status::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sleep {
    Off,
    Minutes(u64),
    Chapter,
}

struct Engine {
    app: tauri::AppHandle,
    snap: Arc<Mutex<TtsSnapshot>>,
    synth_tx: Sender<SynthMsg>,
    out_tx: Sender<EngineMsg>,
    helper: PathBuf,
    model_dir: PathBuf,
    // Book state.
    status: Status,
    path: Option<String>,
    title: Option<String>,
    chapter: usize,
    chapter_count: usize,
    chapter_text: String,
    sentences: Vec<SentenceRange>,
    // Playhead + buffer.
    cur: usize,
    next_append: usize,
    cache: HashMap<usize, Arc<Vec<f32>>>,
    sample_rate: u32,
    requested: HashSet<usize>,
    /// The one synthesis job currently out: (generation, sentence).
    /// Generation-tagged so a stale arrival can never free a newer
    /// job's slot.
    in_flight: Option<(u64, usize)>,
    generation: u64,
    // Audio.
    stream: Option<rodio::OutputStream>,
    sink: Option<rodio::Sink>,
    sink_from: usize,
    appended: usize,
    last_consumed: usize,
    stalled: bool,
    stalls: u32,
    // Settings + timers.
    voice_id: String,
    speed: f32,
    sleep: Sleep,
    sleep_deadline: Option<Instant>,
    paused_since: Option<Instant>,
    helper_alive: bool,
    message: Option<String>,
}

impl Engine {
    fn new(
        app: tauri::AppHandle,
        snap: Arc<Mutex<TtsSnapshot>>,
        out_tx: Sender<EngineMsg>,
        helper: PathBuf,
        model_dir: PathBuf,
    ) -> Self {
        let (synth_tx, synth_rx) = channel::<SynthMsg>();
        let synth_out = out_tx.clone();
        let synth_helper = helper.clone();
        let synth_model = model_dir.clone();
        std::thread::Builder::new()
            .name("tts-synth".into())
            .spawn(move || synth_thread(synth_helper, synth_model, synth_rx, synth_out))
            .expect("tts synth thread");
        Engine {
            app,
            snap,
            synth_tx,
            out_tx,
            helper,
            model_dir,
            status: Status::Idle,
            path: None,
            title: None,
            chapter: 0,
            chapter_count: 0,
            chapter_text: String::new(),
            sentences: Vec::new(),
            cur: 0,
            next_append: 0,
            cache: HashMap::new(),
            sample_rate: 24000,
            requested: HashSet::new(),
            in_flight: None,
            generation: 0,
            stream: None,
            sink: None,
            sink_from: 0,
            appended: 0,
            last_consumed: 0,
            stalled: false,
            stalls: 0,
            voice_id: ufm_tts::DEFAULT_VOICE_ID.to_string(),
            speed: 1.0,
            sleep: Sleep::Off,
            sleep_deadline: None,
            paused_since: None,
            helper_alive: false,
            message: None,
        }
    }

    fn run(&mut self, rx: Receiver<EngineMsg>) {
        loop {
            match rx.recv_timeout(TICK) {
                Ok(EngineMsg::Cmd(cmd)) => {
                    if !self.handle_cmd(cmd) {
                        break; // Quit
                    }
                }
                Ok(EngineMsg::Synth(done)) => self.handle_synth(done),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
            self.tick();
        }
        // App going down: position first, then the narrator away.
        self.save_position();
        let _ = self.synth_tx.send(SynthMsg::Shutdown);
    }

    // -- snapshot ---------------------------------------------------

    fn publish(&mut self) {
        let buffered = self
            .cache
            .keys()
            .filter(|k| **k > self.cur)
            .count();
        {
            let mut snap = self.snap.lock().unwrap();
            snap.version += 1;
            snap.state = self.status.name();
            snap.path = self.path.clone();
            snap.title = self.title.clone();
            snap.chapter = if self.path.is_some() {
                Some(self.chapter)
            } else {
                None
            };
            snap.chapter_count = self.chapter_count;
            snap.sentence = if self.path.is_some() {
                Some(self.cur)
            } else {
                None
            };
            snap.sentence_count = self.sentences.len();
            snap.buffered = buffered;
            snap.stalls = self.stalls;
            snap.voice = self.voice_id.clone();
            snap.speed = self.speed;
            snap.sleep = match self.sleep {
                Sleep::Off => "off".to_string(),
                Sleep::Minutes(m) => m.to_string(),
                Sleep::Chapter => "chapter".to_string(),
            };
            snap.message = self.message.clone();
        }
        use tauri::Emitter;
        let _ = self.app.emit("tts-state", self.snap.lock().unwrap().clone());
    }

    // -- book loading ----------------------------------------------

    fn load_chapter(&mut self, chapter: usize) -> Result<(), String> {
        let path = self.path.clone().ok_or("no book is open")?;
        let (text, count, title) = crate::reader::chapter_text(&path, chapter)?;
        self.chapter = chapter;
        self.chapter_count = count;
        if self.title.is_none() {
            self.title = title;
        }
        self.chapter_text = text;
        self.sentences = segment_chapter(&self.chapter_text);
        Ok(())
    }

    /// Reset the playhead machinery for a (re)start at `sentence`.
    fn rewind_to(&mut self, sentence: usize) {
        self.generation += 1;
        self.cache.clear();
        self.requested.clear();
        self.in_flight = None;
        self.cur = sentence.min(self.sentences.len().saturating_sub(1));
        self.next_append = self.cur;
        self.appended = 0;
        self.last_consumed = 0;
        self.stalled = false;
        self.drop_sink();
    }

    fn drop_sink(&mut self) {
        if let Some(sink) = self.sink.take() {
            sink.stop();
        }
    }

    fn ensure_sink(&mut self) -> Result<&rodio::Sink, String> {
        if self.stream.is_none() {
            // rodio 0.21's builder API: the stream owns the
            // mixer; sinks connect to it.
            let stream = rodio::OutputStreamBuilder::open_default_stream()
                .map_err(|e| format!("no sound device could be opened: {e}"))?;
            self.stream = Some(stream);
        }
        if self.sink.is_none() {
            let mixer = self.stream.as_ref().expect("stream ensured").mixer();
            let sink = rodio::Sink::connect_new(mixer);
            self.sink = Some(sink);
            self.sink_from = self.next_append;
            self.appended = 0;
            self.last_consumed = 0;
        }
        Ok(self.sink.as_ref().expect("sink ensured"))
    }

    // -- buffer -----------------------------------------------------

    /// Keep LOOKAHEAD sentences synthesized or synthesizing ahead
    /// of the playhead. One job in flight: the helper speaks one
    /// sentence at a time, in order.
    fn fill_buffer(&mut self) {
        if self.in_flight.is_some() || self.path.is_none() {
            return;
        }
        if !matches!(self.status, Status::Playing | Status::Loading) {
            return;
        }
        let horizon = (self.cur + LOOKAHEAD).min(self.sentences.len().saturating_sub(1));
        let mut target = self.cur;
        while target <= horizon {
            if !self.cache.contains_key(&target) && !self.requested.contains(&target) {
                break;
            }
            target += 1;
        }
        if target > horizon || self.cache.contains_key(&target) {
            return;
        }
        let text = unit_text(&self.chapter_text, &self.sentences[target]);
        if text.trim().is_empty() {
            return;
        }
        self.requested.insert(target);
        self.in_flight = Some((self.generation, target));
        let _ = self.synth_tx.send(SynthMsg::Job(SynthJob {
            generation: self.generation,
            chapter: self.chapter,
            sentence: target,
            text,
            voice_id: self.voice_id.clone(),
            speed: self.speed,
        }));
    }

    /// Append every contiguous cached sentence from next_append.
    fn drain_to_sink(&mut self) {
        // Read before the loop: ensure_sink borrows self mutably,
        // and the status can't change mid-drain anyway.
        let paused = self.status == Status::Paused;
        loop {
            let pcm = match self.cache.get(&self.next_append) {
                Some(p) => p.clone(),
                None => break,
            };
            let rate = self.sample_rate;
            match self.ensure_sink() {
                Ok(sink) => {
                    let source = rodio::buffer::SamplesBuffer::new(1, rate, (*pcm).clone());
                    sink.append(source);
                    if paused {
                        // Priming while paused (a voice/speed
                        // change): the buffer fills in silence —
                        // a fresh sink plays by default, so hold
                        // it until Resume says go.
                        sink.pause();
                    }
                    self.appended += 1;
                    self.next_append += 1;
                }
                Err(e) => {
                    self.status = Status::Error;
                    self.message = Some(e);
                    return;
                }
            }
        }
        if self.status == Status::Loading && self.appended > 0 {
            self.status = Status::Playing;
            self.stalled = false;
        }
    }

    fn handle_synth(&mut self, done: SynthDone) {
        if done.generation != self.generation || done.chapter != self.chapter {
            // Stale (a seek or chapter turn superseded it). Free
            // the slot ONLY if this arrival is the job recorded
            // there — a newer job may already be out under the
            // current generation, and its slot is not this
            // arrival's to free.
            if self.in_flight == Some((done.generation, done.sentence)) {
                self.in_flight = None;
            }
            return;
        }
        self.in_flight = None;
        match done.result {
            Ok((samples, rate)) => {
                self.helper_alive = true;
                self.sample_rate = rate;
                self.cache.insert(done.sentence, Arc::new(samples));
                self.requested.remove(&done.sentence);
                self.drain_to_sink();
            }
            Err(reason) => {
                self.helper_alive = false;
                if matches!(self.status, Status::Playing | Status::Loading) {
                    self.status = Status::Error;
                    self.message = Some(reason);
                    self.drop_sink();
                }
            }
        }
        self.publish();
    }

    // -- clock --------------------------------------------------------

    fn tick(&mut self) {
        // Sleep deadline: a Rust Instant, checked on the tick —
        // never a webview timer.
        if self.status == Status::Playing {
            if let Some(deadline) = self.sleep_deadline {
                if Instant::now() >= deadline {
                    self.stop_inner(Some("Sleep timer — goodnight.".into()));
                    self.publish();
                    return;
                }
            }
        }
        // Paused too long: put the narrator away (position kept).
        if self.status == Status::Paused {
            if let Some(since) = self.paused_since {
                if since.elapsed() > PAUSED_UNLOAD && self.helper_alive {
                    let _ = self.synth_tx.send(SynthMsg::Shutdown);
                    self.helper_alive = false;
                    self.message =
                        Some("The narrator was put away to save memory — press Play to bring it back.".into());
                    self.publish();
                }
            }
        }
        if !matches!(self.status, Status::Playing | Status::Paused) {
            return;
        }
        // Playhead tracking by queue consumption.
        if let Some(sink) = &self.sink {
            let remaining = sink.len();
            let consumed = self.appended.saturating_sub(remaining);
            if consumed != self.last_consumed {
                self.last_consumed = consumed;
                let now_playing = self.sink_from + consumed;
                if now_playing < self.sentences.len() && now_playing != self.cur {
                    self.cur = now_playing;
                    self.stalled = false;
                    self.save_position();
                    self.emit_sentence();
                }
                // Prune what's behind the playhead.
                let floor = self.cur.saturating_sub(1);
                self.cache.retain(|k, _| *k >= floor);
                self.requested.retain(|k| *k >= floor);
            }
            if self.status == Status::Playing && sink.empty() && self.in_flight.is_none() {
                if self.next_append < self.sentences.len() {
                    // Waiting on synthesis with a silent queue.
                    if !self.stalled {
                        self.stalled = true;
                        self.stalls += 1;
                    }
                    self.fill_buffer();
                } else if self.cache.keys().all(|k| *k < self.next_append) {
                    // The chapter is fully spoken.
                    self.chapter_finished();
                }
            }
        }
        if self.status == Status::Playing {
            self.fill_buffer();
        }
    }

    fn emit_sentence(&self) {
        use tauri::Emitter;
        #[derive(Serialize, Clone)]
        #[serde(rename_all = "camelCase")]
        struct SentenceEvent {
            path: Option<String>,
            chapter: usize,
            sentence: usize,
        }
        let _ = self.app.emit(
            "tts-sentence",
            SentenceEvent {
                path: self.path.clone(),
                chapter: self.chapter,
                sentence: self.cur,
            },
        );
        // Keep the shared snapshot's playhead honest between full
        // publishes: the sentence moved, so the snapshot says so.
        {
            let mut snap = self.snap.lock().unwrap();
            snap.chapter = if self.path.is_some() {
                Some(self.chapter)
            } else {
                None
            };
            snap.sentence = if self.path.is_some() {
                Some(self.cur)
            } else {
                None
            };
        }
        self.publish_lite();
    }

    fn publish_lite(&self) {
        use tauri::Emitter;
        let _ = self.app.emit("tts-state", self.snap.lock().unwrap().clone());
    }

    fn chapter_finished(&mut self) {
        if self.sleep == Sleep::Chapter {
            self.stop_inner(Some("Sleep timer — end of chapter.".into()));
            self.publish();
            return;
        }
        if self.chapter + 1 < self.chapter_count {
            let next = self.chapter + 1;
            match self.load_chapter(next) {
                Ok(()) => {
                    self.rewind_to(0);
                    self.status = Status::Loading;
                    self.save_position();
                    self.fill_buffer();
                }
                Err(e) => {
                    self.status = Status::Error;
                    self.message = Some(e);
                }
            }
        } else {
            self.stop_inner(Some("Finished the book.".into()));
        }
        self.publish();
    }

    // -- commands -----------------------------------------------------

    fn handle_cmd(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::Quit => return false,
            Cmd::Play {
                path,
                chapter,
                sentence,
            } => self.play(path, chapter, sentence),
            Cmd::Pause => {
                if self.status == Status::Playing {
                    if let Some(sink) = &self.sink {
                        sink.pause();
                    }
                    self.status = Status::Paused;
                    self.paused_since = Some(Instant::now());
                    self.save_position();
                }
            }
            Cmd::Resume => {
                if self.status == Status::Paused {
                    self.resume_inner();
                }
            }
            Cmd::Stop => self.stop_inner(None),
            Cmd::NextSentence => self.seek_sentence(self.cur + 1),
            Cmd::PrevSentence => self.seek_sentence(self.cur.saturating_sub(1)),
            Cmd::NextChapter => self.seek_chapter(self.chapter + 1),
            Cmd::PrevChapter => self.seek_chapter(self.chapter.saturating_sub(1)),
            Cmd::SetVoice(id) => {
                if voice_by_id(&id).is_some() && id != self.voice_id {
                    self.voice_id = id;
                    self.restart_at_cur();
                }
            }
            Cmd::SetSpeed(speed) => {
                let speed = speed.clamp(0.75, 2.0);
                if (speed - self.speed).abs() > f32::EPSILON {
                    self.speed = speed;
                    self.restart_at_cur();
                }
            }
            Cmd::SetSleep(mode) => {
                self.sleep = match mode.as_str() {
                    "15" => Sleep::Minutes(15),
                    "30" => Sleep::Minutes(30),
                    "45" => Sleep::Minutes(45),
                    "60" => Sleep::Minutes(60),
                    "chapter" => Sleep::Chapter,
                    _ => Sleep::Off,
                };
                self.sleep_deadline = match self.sleep {
                    Sleep::Minutes(m) if self.status == Status::Playing => {
                        Some(Instant::now() + Duration::from_secs(m * 60))
                    }
                    _ => None,
                };
            }
        }
        self.publish();
        true
    }

    fn play(&mut self, path: String, chapter: Option<usize>, sentence: Option<usize>) {
        if !ufm_tts::model::model_ready(&self.model_dir) {
            self.status = Status::Error;
            self.message = Some(
                "The narrator's voice isn't here yet — it downloads once, then it's yours offline."
                    .into(),
            );
            self.publish();
            return;
        }
        // Same book, paused, no explicit place: that's a resume.
        if self.path.as_deref() == Some(path.as_str())
            && self.status == Status::Paused
            && chapter.is_none()
            && sentence.is_none()
        {
            self.resume_inner();
            self.publish();
            return;
        }
        let saved = crate::reader::listening_position(&path);
        let start_chapter = chapter
            .or_else(|| saved.map(|(c, _)| c))
            .unwrap_or(0);
        let start_sentence = sentence.or_else(|| {
            saved.and_then(|(c, s)| if c == start_chapter { Some(s) } else { None })
        });
        self.path = Some(path);
        self.title = None;
        match self.load_chapter(start_chapter) {
            Ok(()) => {}
            Err(e) => {
                self.status = Status::Error;
                self.message = Some(e);
                self.publish();
                return;
            }
        }
        if self.sentences.is_empty() {
            self.status = Status::Error;
            self.message = Some("That chapter has no readable text to say aloud.".into());
            self.publish();
            return;
        }
        self.rewind_to(start_sentence.unwrap_or(0));
        self.status = Status::Loading;
        self.message = None;
        self.arm_sleep_deadline();
        self.save_position();
        self.fill_buffer();
        self.publish();
    }

    fn resume_inner(&mut self) {
        self.paused_since = None;
        if self.helper_alive {
            if let Some(sink) = &self.sink {
                sink.play();
            }
            self.status = Status::Playing;
        } else {
            // Narrator was put away: rebuild at the ear mark.
            self.rewind_to(self.cur);
            self.status = Status::Loading;
            self.fill_buffer();
        }
        self.arm_sleep_deadline();
    }

    fn stop_inner(&mut self, message: Option<String>) {
        self.save_position();
        self.drop_sink();
        self.generation += 1;
        self.cache.clear();
        self.requested.clear();
        self.in_flight = None;
        if self.helper_alive {
            let _ = self.synth_tx.send(SynthMsg::Shutdown);
            self.helper_alive = false;
        }
        self.status = Status::Stopped;
        self.message = message;
        self.paused_since = None;
    }

    fn seek_sentence(&mut self, target: usize) {
        if self.path.is_none() || self.sentences.is_empty() {
            return;
        }
        if target >= self.sentences.len() {
            // Stepping past the chapter's last sentence turns it.
            self.seek_chapter(self.chapter + 1);
            return;
        }
        self.rewind_to(target);
        self.status = Status::Loading;
        self.message = None;
        self.save_position();
        self.fill_buffer();
    }

    fn seek_chapter(&mut self, target: usize) {
        if self.path.is_none() || target >= self.chapter_count {
            return;
        }
        match self.load_chapter(target) {
            Ok(()) => {
                self.rewind_to(0);
                self.status = Status::Loading;
                self.message = None;
                self.save_position();
                self.fill_buffer();
            }
            Err(e) => {
                self.status = Status::Error;
                self.message = Some(e);
            }
        }
    }

    /// Voice/speed changed: what's already queued speaks the old
    /// settings, so rebuild from the ear mark in the new ones.
    fn restart_at_cur(&mut self) {
        if self.path.is_none() || !matches!(self.status, Status::Playing | Status::Paused | Status::Loading) {
            return;
        }
        let was_paused = self.status == Status::Paused;
        self.rewind_to(self.cur);
        self.status = Status::Loading;
        self.fill_buffer();
        if was_paused {
            // The rebuild plays from the sink; a paused restart
            // primes the buffer and waits.
            self.status = Status::Paused;
            self.paused_since = Some(Instant::now());
        }
    }

    fn arm_sleep_deadline(&mut self) {
        self.sleep_deadline = match self.sleep {
            Sleep::Minutes(m) => Some(Instant::now() + Duration::from_secs(m * 60)),
            _ => None,
        };
    }

    fn save_position(&self) {
        if let Some(path) = &self.path {
            crate::reader::save_listening_position(path, self.chapter, self.cur);
        }
    }
}

// ---------------------------------------------------------------------------
// Tauri commands — every verb returns the fresh snapshot (§AW).
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn tts_snapshot(hub: tauri::State<'_, TtsHub>) -> TtsSnapshot {
    hub.snapshot()
}

#[tauri::command]
pub fn tts_play(
    hub: tauri::State<'_, TtsHub>,
    path: String,
    chapter: Option<usize>,
    sentence: Option<usize>,
) -> TtsSnapshot {
    hub.send(Cmd::Play {
        path,
        chapter,
        sentence,
    })
}

#[tauri::command]
pub fn tts_pause(hub: tauri::State<'_, TtsHub>) -> TtsSnapshot {
    hub.send(Cmd::Pause)
}

#[tauri::command]
pub fn tts_resume(hub: tauri::State<'_, TtsHub>) -> TtsSnapshot {
    hub.send(Cmd::Resume)
}

#[tauri::command]
pub fn tts_stop(hub: tauri::State<'_, TtsHub>) -> TtsSnapshot {
    hub.send(Cmd::Stop)
}

#[tauri::command]
pub fn tts_next_sentence(hub: tauri::State<'_, TtsHub>) -> TtsSnapshot {
    hub.send(Cmd::NextSentence)
}

#[tauri::command]
pub fn tts_prev_sentence(hub: tauri::State<'_, TtsHub>) -> TtsSnapshot {
    hub.send(Cmd::PrevSentence)
}

#[tauri::command]
pub fn tts_next_chapter(hub: tauri::State<'_, TtsHub>) -> TtsSnapshot {
    hub.send(Cmd::NextChapter)
}

#[tauri::command]
pub fn tts_prev_chapter(hub: tauri::State<'_, TtsHub>) -> TtsSnapshot {
    hub.send(Cmd::PrevChapter)
}

#[tauri::command]
pub fn tts_set_voice(hub: tauri::State<'_, TtsHub>, voice: String) -> TtsSnapshot {
    hub.send(Cmd::SetVoice(voice))
}

#[tauri::command]
pub fn tts_set_speed(hub: tauri::State<'_, TtsHub>, speed: f32) -> TtsSnapshot {
    hub.send(Cmd::SetSpeed(speed))
}

#[tauri::command]
pub fn tts_set_sleep(hub: tauri::State<'_, TtsHub>, mode: String) -> TtsSnapshot {
    hub.send(Cmd::SetSleep(mode))
}

// ---------------------------------------------------------------------------
// The model download: one archive, hash-pinned, resume-capable,
// deletable by deleting files. Single-flight; progress is state,
// not a promise.
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn tts_model_status(hub: tauri::State<'_, TtsHub>) -> ModelSnapshot {
    // Refresh from disk truth when idle: a model deleted behind
    // the app's back must read as missing, not ready.
    let cur = hub.model.lock().unwrap().clone();
    if matches!(cur.state, "ready" | "missing") {
        let ready = ufm_tts::model::model_ready(&hub.model_dir());
        let mut s = hub.model.lock().unwrap();
        s.state = if ready { "ready" } else { "missing" };
        return s.clone();
    }
    cur
}

#[tauri::command]
pub fn tts_model_download(
    app: tauri::AppHandle,
    hub: tauri::State<'_, TtsHub>,
) -> ModelSnapshot {
    {
        let cur = hub.model.lock().unwrap();
        if matches!(cur.state, "downloading" | "verifying" | "extracting" | "ready") {
            return cur.clone();
        }
    }
    let model_state = hub.model.clone();
    let base = tts_base_dir();
    std::thread::Builder::new()
        .name("tts-model-download".into())
        .spawn(move || {
            use tauri::Emitter;
            let set = |state: &'static str,
                       downloaded: u64,
                       message: Option<String>,
                       st: &Arc<Mutex<ModelSnapshot>>| {
                {
                    let mut s = st.lock().unwrap();
                    s.state = state;
                    s.downloaded = downloaded;
                    s.total = ufm_tts::model::ARCHIVE_BYTES;
                    s.message = message;
                }
                let _ = app.emit("tts-model", st.lock().unwrap().clone());
            };
            let result = download_model(&base, &model_state, &app);
            match result {
                Ok(()) => set("ready", ufm_tts::model::ARCHIVE_BYTES, None, &model_state),
                Err(reason) => set("error", 0, Some(reason), &model_state),
            }
        })
        .expect("model download thread");
    hub.model.lock().unwrap().clone()
}

fn download_model(
    base: &PathBuf,
    model_state: &Arc<Mutex<ModelSnapshot>>,
    app: &tauri::AppHandle,
) -> Result<(), String> {
    use tauri::Emitter;
    std::fs::create_dir_all(base).map_err(|e| format!("couldn't create the voice folder: {e}"))?;
    let part_path = base.join(format!("{}.part", ufm_tts::model::ARCHIVE_FILE));
    let have = std::fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0);
    let mut req = ureq::get(ufm_tts::model::ARCHIVE_URL)
        .set("User-Agent", "Fable Files/0.1.0 (narrator download)")
        .timeout(Duration::from_secs(600));
    if have > 0 {
        req = req.set("Range", &format!("bytes={have}-"));
    }
    let resp = req.call().map_err(|e| match e {
        ureq::Error::Status(code, _) => format!("the download server answered {code}"),
        other => format!("couldn't reach the download server: {other}"),
    })?;
    let status = resp.status();
    // 206 = the range was honored; anything else restarts clean.
    let resume = status == 206 && have > 0;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(resume)
        .write(!resume)
        .truncate(!resume)
        .open(&part_path)
        .map_err(|e| format!("couldn't open the download file: {e}"))?;
    let mut downloaded = if resume { have } else { 0 };
    {
        let mut s = model_state.lock().unwrap();
        s.state = "downloading";
        s.downloaded = downloaded;
    }
    let mut reader = resp.into_reader();
    let mut buf = [0u8; 256 * 1024];
    let mut last_emit = Instant::now();
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("the download broke partway: {e}"))?;
        if n == 0 {
            break;
        }
        use std::io::Write as _;
        file.write_all(&buf[..n])
            .map_err(|e| format!("couldn't write the download: {e}"))?;
        downloaded += n as u64;
        if last_emit.elapsed() > Duration::from_millis(400) {
            last_emit = Instant::now();
            {
                let mut s = model_state.lock().unwrap();
                s.downloaded = downloaded;
            }
            let _ = app.emit("tts-model", model_state.lock().unwrap().clone());
        }
    }
    drop(file);
    // The pin is the contract: hash what landed, compare exactly.
    {
        let mut s = model_state.lock().unwrap();
        s.state = "verifying";
        s.downloaded = downloaded;
    }
    let _ = app.emit("tts-model", model_state.lock().unwrap().clone());
    let bytes = std::fs::read(&part_path).map_err(|e| format!("couldn't read the download: {e}"))?;
    let sha = {
        use sha2::Digest;
        let mut h = sha2::Sha256::new();
        h.update(&bytes);
        h.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>()
    };
    if sha != ufm_tts::model::ARCHIVE_SHA256 {
        let _ = std::fs::remove_file(&part_path);
        return Err(
            "the downloaded voice files don't match their fingerprint, so they were thrown away — nothing was installed"
                .into(),
        );
    }
    {
        let mut s = model_state.lock().unwrap();
        s.state = "extracting";
    }
    let _ = app.emit("tts-model", model_state.lock().unwrap().clone());
    let file = std::fs::File::open(&part_path).map_err(|e| e.to_string())?;
    let decoder = bzip2::read::BzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    let staging = base.join(".extracting");
    if staging.exists() {
        std::fs::remove_dir_all(&staging).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    archive
        .unpack(&staging)
        .map_err(|e| format!("the voice archive wouldn't open: {e}"))?;
    let extracted = staging.join(ufm_tts::model::MODEL_DIR_NAME);
    if !ufm_tts::model::model_ready(&extracted) {
        return Err("the voice archive unpacked without its model inside".into());
    }
    let final_dir = ufm_tts::model::model_dir(base);
    if final_dir.exists() {
        std::fs::remove_dir_all(&final_dir).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&extracted, &final_dir).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(&staging);
    let _ = std::fs::remove_file(&part_path);
    Ok(())
}
