//! The shared music transport core (Music Glow-Up Phase 2, crew Diva).
//!
//! One playback truth for every music surface — the Music Player
//! panel, the Stage window, the detached Mini window, and Quick
//! Look's audio card. Before this module the "player" was four
//! disconnected surfaces with four private audio elements, and
//! Dickles' 2026-10-16 bug — preview B talking over preview A —
//! was structural: nothing anywhere stopped the other element.
//!
//! The core is owned by Rust (the webview-timers-lie law: WebKitGTK
//! throttles hidden windows, so position/progress truth lives
//! here). Surfaces hold exactly one `<audio>` element each (the
//! "owner engine"); the core decides what plays, in what order,
//! and revokes the previous owner the instant a new surface claims
//! playback. The named acceptance test — start B while A plays and
//! A stops, on every surface — is structural here: only the
//! owner's element is ever unpaused, and a claim revokes the old
//! owner's right to sound.
//!
//! The pure state machine (`Core`) takes `Instant`s as parameters
//! so its math is unit-testable without a clock; the Tauri glue at
//! the bottom wires it to AppState, events, the catalog's
//! audio_play_state, and a 4 Hz tick thread that broadcasts
//! snapshots while playing.

use serde::{Deserialize, Serialize};
use std::time::Instant;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OwnerKind {
    Panel,
    Stage,
    Mini,
    QuickLook,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlayState {
    Stopped,
    Playing,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RepeatMode {
    Off,
    All,
    One,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueEntry {
    /// Absolute path (the catalog provider_key) — the identity
    /// every surface already speaks.
    pub provider_key: String,
    pub name: String,
    /// Known duration from the catalog, when probed. Surfaces may
    /// refine it via a `duration` fact once their element loads.
    pub duration_secs: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CastMirror {
    pub device_name: String,
    pub playing: bool,
    pub position_secs: f64,
    pub duration_secs: f64,
}

/// The full broadcast truth. Every surface renders from this and
/// nothing else; it rides the `player-core` event and is also the
/// return of the `player_state` command for late-booting windows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub owner: Option<OwnerKind>,
    pub state: PlayState,
    pub queue: Vec<QueueEntry>,
    /// Play order: queue indices in the order they will play
    /// (identity unless shuffled).
    pub order: Vec<usize>,
    /// Queue index of the current entry (None = nothing cued).
    pub index: Option<usize>,
    pub position_secs: f64,
    pub duration_secs: f64,
    pub repeat: RepeatMode,
    pub shuffle: bool,
    pub volume: f64,
    pub muted: bool,
    /// Increments on every entry load; engines reload when it moves.
    pub play_token: u64,
    pub cast: Option<CastMirror>,
    pub last_error: Option<String>,
}

/// Side effects the glue performs after a core mutation.
#[derive(Debug, Default)]
pub struct Effects {
    /// The current entry changed and the owner's engine must load
    /// it (at the snapshot's position / play state).
    pub load: bool,
    /// The load STARTS a new listen (history counts it). Claims
    /// (detach / return / takeover) reload the same listen and
    /// must never double-count it.
    pub count_play: bool,
    /// This owner kind lost ownership to a new claim.
    pub revoked: Option<(OwnerKind, Option<String>)>,
    /// Volume/mute changed — persist prefs.
    pub persist_prefs: bool,
}

// ---------------------------------------------------------------------------
// The pure core
// ---------------------------------------------------------------------------

pub struct Core {
    queue: Vec<QueueEntry>,
    order: Vec<usize>,
    /// Position within `order` of the current entry.
    pos_in_order: usize,
    state: PlayState,
    owner: Option<OwnerKind>,
    owner_label: Option<String>,
    repeat: RepeatMode,
    shuffle: bool,
    /// Position (seconds) at `anchor` — the position clock.
    pos_base: f64,
    anchor: Option<Instant>,
    /// Duration of the current entry as last known (entry metadata
    /// or an engine fact). 0 = unknown.
    duration: f64,
    volume: f64,
    muted: bool,
    play_token: u64,
    cast: Option<CastMirror>,
    last_error: Option<String>,
    /// Deterministic shuffle seed source (tests pin it).
    shuffle_seed: u64,
}

impl Core {
    pub fn new(volume: f64, muted: bool) -> Self {
        Core {
            queue: Vec::new(),
            order: Vec::new(),
            pos_in_order: 0,
            state: PlayState::Stopped,
            owner: None,
            owner_label: None,
            repeat: RepeatMode::Off,
            shuffle: false,
            pos_base: 0.0,
            anchor: None,
            duration: 0.0,
            volume: volume.clamp(0.0, 1.0),
            muted,
            play_token: 0,
            cast: None,
            last_error: None,
            shuffle_seed: 0x9E3779B97F4A7C15,
        }
    }

    pub fn current_index(&self) -> Option<usize> {
        self.order.get(self.pos_in_order).copied()
    }

    pub fn current_entry(&self) -> Option<&QueueEntry> {
        self.current_index().and_then(|i| self.queue.get(i))
    }

    /// The position clock: anchored while Playing, frozen otherwise.
    pub fn position(&self, now: Instant) -> f64 {
        let mut p = self.pos_base;
        if self.state == PlayState::Playing {
            if let Some(a) = self.anchor {
                p += now.saturating_duration_since(a).as_secs_f64();
            }
        }
        if self.duration > 0.0 {
            p = p.min(self.duration);
        }
        p.max(0.0)
    }

    /// Freeze the clock at the current position (state changes).
    fn rebase(&mut self, now: Instant) {
        self.pos_base = self.position(now);
        self.anchor = if self.state == PlayState::Playing {
            Some(now)
        } else {
            None
        };
    }

    fn set_state(&mut self, state: PlayState, now: Instant) {
        self.pos_base = self.position(now);
        self.state = state;
        self.anchor = if state == PlayState::Playing {
            Some(now)
        } else {
            None
        };
    }

    /// Load the entry at `pos_in_order`: reset the clock, adopt its
    /// known duration, bump the token (engines reload on the bump).
    fn load_at(&mut self, pos_in_order: usize, now: Instant, autoplay: bool) {
        self.pos_in_order = pos_in_order;
        self.pos_base = 0.0;
        self.duration = self
            .current_entry()
            .and_then(|e| e.duration_secs)
            .unwrap_or(0.0);
        self.play_token += 1;
        self.last_error = None;
        self.set_state(
            if autoplay {
                PlayState::Playing
            } else {
                PlayState::Paused
            },
            now,
        );
    }

    fn build_order(&mut self, keep_first: Option<usize>) {
        let mut idx: Vec<usize> = (0..self.queue.len()).collect();
        if self.shuffle {
            // Fisher–Yates with a splitmix64 stream — deterministic
            // per seed so tests can assert permutation invariants.
            let mut seed = self.shuffle_seed;
            self.shuffle_seed = self.shuffle_seed.wrapping_add(0x9E3779B97F4A7C15);
            let mut next = move || {
                seed = seed.wrapping_add(0x9E3779B97F4A7C15);
                let mut z = seed;
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
                z ^ (z >> 31)
            };
            for i in (1..idx.len()).rev() {
                let j = (next() % (i as u64 + 1)) as usize;
                idx.swap(i, j);
            }
            if let Some(first) = keep_first {
                if let Some(p) = idx.iter().position(|&x| x == first) {
                    idx.remove(p);
                    idx.insert(0, first);
                }
            }
        }
        self.order = idx;
    }

    // -- claims ---------------------------------------------------

    /// THE claim: a surface takes (or retakes) playback with a
    /// queue. Any previous owner is revoked — its engine obeys the
    /// `player-revoked` event / next snapshot and goes silent.
    pub fn open_queue(
        &mut self,
        owner: OwnerKind,
        owner_label: Option<String>,
        entries: Vec<QueueEntry>,
        start_index: usize,
        start_position: f64,
        autoplay: bool,
        now: Instant,
    ) -> Effects {
        let mut fx = Effects::default();
        if let Some(old) = self.owner {
            if old != owner {
                fx.revoked = Some((old, self.owner_label.clone()));
            }
        }
        self.owner = Some(owner);
        self.owner_label = owner_label;
        self.queue = entries;
        let start = if self.queue.is_empty() {
            0
        } else {
            start_index.min(self.queue.len() - 1)
        };
        self.build_order(Some(start));
        self.pos_in_order = self
            .order
            .iter()
            .position(|&x| x == start)
            .unwrap_or(0);
        if self.queue.is_empty() {
            self.state = PlayState::Stopped;
            self.pos_base = 0.0;
            self.anchor = None;
            self.duration = 0.0;
            return fx;
        }
        self.load_at(self.pos_in_order, now, autoplay);
        if start_position > 0.0 {
            self.pos_base = start_position;
            self.rebase(now);
        }
        fx.load = true;
        fx.count_play = true;
        fx
    }

    /// Retake ownership of the CURRENT queue (detach / return /
    /// surface takeover) without replacing it.
    pub fn claim(
        &mut self,
        owner: OwnerKind,
        owner_label: Option<String>,
        now: Instant,
    ) -> Effects {
        let mut fx = Effects::default();
        if let Some(old) = self.owner {
            if old != owner {
                fx.revoked = Some((old, self.owner_label.clone()));
            }
        }
        self.owner = Some(owner);
        self.owner_label = owner_label;
        // The new owner's engine loads the current entry at the
        // clock's position with the current play state.
        if self.current_entry().is_some() {
            self.rebase(now);
            self.play_token += 1;
            fx.load = true;
        }
        fx
    }

    /// The owner window died without a return (tombstone backstop):
    /// ownership lapses; the clock freezes at the end-clamp so a
    /// later claim lands somewhere sane.
    pub fn detach_gone(&mut self, now: Instant) {
        if self.state == PlayState::Playing {
            // Freeze where the clock says we are; an engine's ended
            // fact never arrives from a dead window, so do not
            // advance the queue on a guess.
            self.set_state(PlayState::Paused, now);
        } else {
            self.rebase(now);
        }
        self.owner = None;
        self.owner_label = None;
    }

    pub fn stop(&mut self, now: Instant) {
        self.set_state(PlayState::Stopped, now);
        self.pos_base = 0.0;
        self.anchor = None;
    }

    // -- transport --------------------------------------------------

    pub fn toggle(&mut self, now: Instant) -> Effects {
        let mut fx = Effects::default();
        match self.state {
            PlayState::Playing => self.set_state(PlayState::Paused, now),
            PlayState::Paused => self.set_state(PlayState::Playing, now),
            PlayState::Stopped => {
                if self.current_entry().is_some() {
                    // Cue the current entry from the top.
                    self.load_at(self.pos_in_order, now, true);
                    fx.load = true;
                    fx.count_play = true;
                }
            }
        }
        fx
    }

    /// Manual step. Manual next ignores RepeatMode::One (the
    /// listener asked to move); repeat-all wraps; the ends stop.
    pub fn step(&mut self, dir: i32, now: Instant) -> Effects {
        let mut fx = Effects::default();
        if self.order.is_empty() {
            return fx;
        }
        let playing = self.state == PlayState::Playing;
        let next = self.pos_in_order as i32 + dir;
        if next < 0 {
            // Previous at the head: restart the entry (standard).
            self.load_at(self.pos_in_order, now, playing);
            fx.load = true;
            return fx;
        }
        if next as usize >= self.order.len() {
            if self.repeat == RepeatMode::All && dir > 0 {
                self.load_at(0, now, playing);
                fx.load = true;
                fx.count_play = true;
            } else if dir > 0 {
                self.stop(now);
            }
            return fx;
        }
        self.load_at(next as usize, now, playing || self.state == PlayState::Paused);
        fx.load = true;
        fx.count_play = true;
        fx
    }

    /// The owner's engine reports the entry ended: the core alone
    /// decides what happens next (repeat modes live here, never in
    /// a webview timer).
    pub fn on_ended(&mut self, now: Instant) -> Effects {
        let mut fx = Effects::default();
        match self.repeat {
            RepeatMode::One => {
                self.load_at(self.pos_in_order, now, true);
                fx.load = true;
                fx.count_play = true;
            }
            RepeatMode::All | RepeatMode::Off => {
                let next = self.pos_in_order + 1;
                if next < self.order.len() {
                    self.load_at(next, now, true);
                    fx.load = true;
                    fx.count_play = true;
                } else if self.repeat == RepeatMode::All && !self.order.is_empty() {
                    self.load_at(0, now, true);
                    fx.load = true;
                    fx.count_play = true;
                } else {
                    self.stop(now);
                }
            }
        }
        fx
    }

    /// Play a specific QUEUE index (row click / Up Next click):
    /// wherever it sits in the play order, it loads now.
    pub fn play_index(&mut self, queue_index: usize, now: Instant) -> Effects {
        let mut fx = Effects::default();
        if queue_index >= self.queue.len() {
            return fx;
        }
        if let Some(pos) = self.order.iter().position(|&i| i == queue_index) {
            self.load_at(pos, now, true);
            fx.load = true;
            fx.count_play = true;
        }
        fx
    }

    pub fn seek(&mut self, secs: f64, now: Instant) {
        let mut s = secs.max(0.0);
        if self.duration > 0.0 {
            s = s.min(self.duration);
        }
        self.pos_base = s;
        self.rebase(now);
    }

    pub fn set_repeat(&mut self, mode: RepeatMode) {
        self.repeat = mode;
    }

    pub fn set_shuffle(&mut self, on: bool, now: Instant) {
        if on == self.shuffle {
            return;
        }
        self.shuffle = on;
        let current = self.current_index();
        self.build_order(current);
        self.pos_in_order = current
            .and_then(|c| self.order.iter().position(|&x| x == c))
            .unwrap_or(0);
        self.rebase(now);
    }

    pub fn set_volume(&mut self, v: f64) -> Effects {
        self.volume = v.clamp(0.0, 1.0);
        Effects {
            persist_prefs: true,
            ..Default::default()
        }
    }

    pub fn set_muted(&mut self, muted: bool) -> Effects {
        self.muted = muted;
        Effects {
            persist_prefs: true,
            ..Default::default()
        }
    }

    pub fn set_cast(&mut self, cast: Option<CastMirror>) {
        self.cast = cast;
    }

    // -- queue management ------------------------------------------

    /// Add entries to the queue (and the play order). `next` inserts
    /// them immediately after the current entry in the play order.
    pub fn enqueue(&mut self, entries: Vec<QueueEntry>, next: bool) {
        if entries.is_empty() {
            return;
        }
        let was_empty = self.queue.is_empty();
        let base = self.queue.len();
        let new_indices: Vec<usize> = (base..base + entries.len()).collect();
        self.queue.extend(entries);
        if was_empty {
            self.order = new_indices;
            self.pos_in_order = 0;
            self.duration = self
                .current_entry()
                .and_then(|e| e.duration_secs)
                .unwrap_or(0.0);
            return;
        }
        if next {
            let at = (self.pos_in_order + 1).min(self.order.len());
            for (k, idx) in new_indices.iter().enumerate() {
                self.order.insert(at + k, *idx);
            }
        } else {
            self.order.extend(new_indices);
        }
    }

    /// Remove a queue entry by QUEUE index. Removing the current
    /// entry advances to whatever followed it in the play order
    /// (same play state), or stops when nothing follows.
    pub fn remove(&mut self, queue_index: usize, now: Instant) -> Effects {
        let mut fx = Effects::default();
        if queue_index >= self.queue.len() {
            return fx;
        }
        // Capture the current QUEUE index before any remap: after
        // the order is rebuilt, order[pos_in_order] names a
        // different entry — reading current_index() then would
        // corrupt the position (the queue_ops test caught this).
        let cur_before = self.current_index();
        let was_current = cur_before == Some(queue_index);
        let playing = self.state == PlayState::Playing;
        self.queue.remove(queue_index);
        // Remap the order: drop the removed index, shift higher ones.
        self.order = self
            .order
            .iter()
            .filter_map(|&i| {
                if i == queue_index {
                    None
                } else if i > queue_index {
                    Some(i - 1)
                } else {
                    Some(i)
                }
            })
            .collect();
        if self.queue.is_empty() {
            self.pos_in_order = 0;
            self.stop(now);
            return fx;
        }
        if was_current {
            let target = self.pos_in_order.min(self.order.len() - 1);
            self.load_at(target, now, playing);
            fx.load = true;
            fx.count_play = true;
        } else if let Some(mut cur) = cur_before {
            if queue_index < cur {
                cur -= 1;
            }
            self.pos_in_order = self
                .order
                .iter()
                .position(|&x| x == cur)
                .unwrap_or(0);
        }
        fx
    }

    /// Clear the queue; optionally keep the current entry.
    pub fn clear_queue(&mut self, keep_current: bool, now: Instant) -> Effects {
        let mut fx = Effects::default();
        if keep_current {
            if let Some(cur) = self.current_entry().cloned() {
                self.queue = vec![cur];
                self.order = vec![0];
                self.pos_in_order = 0;
                return fx;
            }
        }
        self.queue.clear();
        self.order.clear();
        self.pos_in_order = 0;
        self.duration = 0.0;
        self.stop(now);
        fx
    }

    /// Move a queue entry (drag reorder in the Panel rail): the
    /// entry keeps its identity; the play order remaps around it.
    pub fn reorder(&mut self, from: usize, to: usize) {
        if from >= self.queue.len() || to >= self.queue.len() || from == to {
            return;
        }
        let entry = self.queue.remove(from);
        self.queue.insert(to, entry);
        let remap = |i: usize| -> usize {
            if i == from {
                to
            } else if from < to && i > from && i <= to {
                i - 1
            } else if to < from && i >= to && i < from {
                i + 1
            } else {
                i
            }
        };
        self.order = self.order.iter().map(|&i| remap(i)).collect();
        if let Some(cur) = self.current_index() {
            self.pos_in_order = self
                .order
                .iter()
                .position(|&x| x == cur)
                .unwrap_or(self.pos_in_order);
        }
    }

    // -- engine facts ------------------------------------------------

    /// Facts flow IN from the owner's engine only (the glue checks
    /// the caller's owner label before calling this).
    pub fn fact(&mut self, kind: &str, value: f64, now: Instant) -> Effects {
        let mut fx = Effects::default();
        match kind {
            "duration" => {
                if value > 0.0 {
                    self.duration = value;
                    if let Some(idx) = self.current_index() {
                        if let Some(e) = self.queue.get_mut(idx) {
                            e.duration_secs = Some(value);
                        }
                    }
                }
            }
            "position" => {
                // Drift correction: the element is the ground
                // truth for where the stream actually is (transcode
                // offsets, buffering); adopt big disagreements.
                let computed = self.position(now);
                if (value - computed).abs() > 1.5 {
                    self.pos_base = value.max(0.0);
                    self.rebase(now);
                }
            }
            "playing" => {
                if self.state == PlayState::Paused {
                    self.set_state(PlayState::Playing, now);
                }
            }
            "paused" => {
                if self.state == PlayState::Playing {
                    self.set_state(PlayState::Paused, now);
                }
            }
            "ended" => {
                return self.on_ended(now);
            }
            _ => {}
        }
        fx
    }

    pub fn fact_error(&mut self, message: String, now: Instant) {
        self.last_error = Some(message);
        if self.state == PlayState::Playing {
            self.set_state(PlayState::Paused, now);
        }
    }

    pub fn snapshot(&self, now: Instant) -> Snapshot {
        Snapshot {
            owner: self.owner,
            state: self.state,
            queue: self.queue.clone(),
            order: self.order.clone(),
            index: self.current_index(),
            position_secs: self.position(now),
            duration_secs: self.duration,
            repeat: self.repeat,
            shuffle: self.shuffle,
            volume: self.volume,
            muted: self.muted,
            play_token: self.play_token,
            cast: self.cast.clone(),
            last_error: self.last_error.clone(),
        }
    }
}

/// The pinned resume rule (audit §7): a saved position applies
/// only to long-form listening — duration ≥ 10 minutes, or an
/// audiobook file (.m4b) — and only when it is meaningfully in
/// (past the first seconds, not at the credits).
pub fn resume_position(
    name: &str,
    duration_secs: Option<f64>,
    saved_secs: f64,
) -> Option<f64> {
    if saved_secs <= 5.0 {
        return None;
    }
    let long_form = match duration_secs {
        Some(d) if d >= 600.0 => true,
        _ => name.to_lowercase().ends_with(".m4b"),
    };
    if !long_form {
        return None;
    }
    if let Some(d) = duration_secs {
        if saved_secs >= d - 10.0 {
            return None;
        }
    }
    Some(saved_secs)
}

// ---------------------------------------------------------------------------
// Tauri glue
// ---------------------------------------------------------------------------

use crate::AppState;
use tauri::{Emitter, Manager, State};

fn publish(app: &tauri::AppHandle, core: &Core, fx: &Effects, now: Instant) {
    let snap = core.snapshot(now);
    if let Some((_, label)) = &fx.revoked {
        if let Some(l) = label {
            if let Some(w) = app.get_webview_window(l) {
                let _ = w.emit("player-revoked", snap.clone());
            }
        }
    }
    let _ = app.emit("player-core", snap);
}

fn with_core<R>(
    app: &tauri::AppHandle,
    state: &AppState,
    f: impl FnOnce(&mut Core, Instant) -> (Effects, R),
) -> R {
    let now = Instant::now();
    let (fx, out, snap_for_history) = {
        let mut core = state.player_core.lock().unwrap();
        let token_before = core.snapshot(now).play_token;
        let (fx, out) = f(&mut core, now);
        let snap = if fx.count_play && core.snapshot(now).play_token != token_before {
            Some(core.snapshot(now))
        } else {
            None
        };
        (fx, out, snap)
    };
    // Play history: a fresh entry load notes the play (count +
    // last-played) in the catalog, exactly once per token.
    if let Some(snap) = &snap_for_history {
        if let Some(idx) = snap.index {
            if let Some(entry) = snap.queue.get(idx) {
                let key = entry.provider_key.clone();
                if let Ok(cat) = state.catalog.lock() {
                    let _ = cat.audio_note_play(&key);
                }
            }
        }
    }
    let core = state.player_core.lock().unwrap();
    publish(app, &core, &fx, Instant::now());
    if fx.persist_prefs {
        save_prefs(state, core.volume, core.snapshot(Instant::now()).muted);
    }
    out
}

fn prefs_path(state: &AppState) -> std::path::PathBuf {
    state.data_base.join("player-prefs.json")
}

fn save_prefs(state: &AppState, volume: f64, muted: bool) {
    let path = prefs_path(state);
    let body = serde_json::json!({ "volume": volume, "muted": muted }).to_string();
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

pub fn load_prefs_at(base: &std::path::Path) -> (f64, bool) {
    if let Ok(data) = std::fs::read(base.join("player-prefs.json")) {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&data) {
            let vol = v
                .get("volume")
                .and_then(|x| x.as_f64())
                .unwrap_or(0.8)
                .clamp(0.0, 1.0);
            let muted = v.get("muted").and_then(|x| x.as_bool()).unwrap_or(false);
            return (vol, muted);
        }
    }
    (0.8, false)
}

/// Start the 4 Hz snapshot tick + position-persistence loop (once
/// per process). Time-critical truth lives here, not in webview
/// timers: snapshots keep flowing to covered/parked windows, and
/// the saved position lands in the catalog every ~15 s of play.
pub fn ensure_tick(app: &tauri::AppHandle) {
    let state = app.state::<AppState>();
    if state
        .player_tick_started
        .swap(true, std::sync::atomic::Ordering::SeqCst)
    {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let mut last_persist = Instant::now();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(250));
            let state = app.state::<AppState>();
            let (snap, persist) = {
                let core = state.player_core.lock().unwrap();
                let snap = core.snapshot(Instant::now());
                let persist = snap.state == PlayState::Playing
                    && last_persist.elapsed() >= std::time::Duration::from_secs(15);
                (snap, persist)
            };
            if snap.state == PlayState::Playing {
                let _ = app.emit("player-core", snap.clone());
            }
            if persist {
                last_persist = Instant::now();
                if let Some(idx) = snap.index {
                    if let Some(entry) = snap.queue.get(idx) {
                        let key = entry.provider_key.clone();
                        let pos = snap.position_secs;
                        if let Ok(cat) = state.catalog.lock() {
                            let _ = cat.audio_set_position(&key, pos, false);
                        }
                    }
                }
            }
        }
    });
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenQueueArgs {
    pub owner: OwnerKind,
    pub entries: Vec<QueueEntry>,
    pub start_index: Option<usize>,
    pub start_position: Option<f64>,
    pub autoplay: Option<bool>,
}

#[tauri::command]
pub fn player_open_queue(
    app: tauri::AppHandle,
    window: tauri::Window,
    state: State<'_, AppState>,
    args: OpenQueueArgs,
) -> Result<Snapshot, String> {
    ensure_tick(&app);
    let label = owner_label_for(args.owner, &window);
    // Resume: an unspecified start position consults the saved
    // position for the starting entry (pinned rule in the core).
    let start_position = args.start_position.unwrap_or_else(|| {
        let idx = args.start_index.unwrap_or(0);
        if let Some(entry) = args.entries.get(idx) {
            if let Ok(cat) = state.catalog.lock() {
                if let Ok(st) = cat.audio_play_state_for(&entry.provider_key) {
                    return resume_position(
                        &entry.name,
                        entry.duration_secs,
                        st.last_position_secs,
                    )
                    .unwrap_or(0.0);
                }
            }
        }
        0.0
    });
    Ok(with_core(&app, &state, |core, now| {
        let fx = core.open_queue(
            args.owner,
            Some(label),
            args.entries,
            args.start_index.unwrap_or(0),
            start_position,
            args.autoplay.unwrap_or(true),
            now,
        );
        (fx, core.snapshot(now))
    }))
}

/// The window label that receives a surface's targeted events.
/// Panel and QuickLook both live in the main window; Mini and
/// Stage ARE their windows. (A Mini clicking Return claims
/// "panel" — the label must still land on the main window.)
fn owner_label_for(owner: OwnerKind, window: &tauri::Window) -> String {
    match owner {
        OwnerKind::Panel | OwnerKind::QuickLook => "main".to_string(),
        OwnerKind::Mini | OwnerKind::Stage => window.label().to_string(),
    }
}

#[tauri::command]
pub fn player_claim(
    app: tauri::AppHandle,
    window: tauri::Window,
    state: State<'_, AppState>,
    owner: OwnerKind,
) -> Result<Snapshot, String> {
    ensure_tick(&app);
    let label = owner_label_for(owner, &window);
    Ok(with_core(&app, &state, |core, now| {
        let fx = core.claim(owner, Some(label), now);
        (fx, core.snapshot(now))
    }))
}

#[tauri::command]
pub fn player_state(state: State<'_, AppState>) -> Result<Snapshot, String> {
    let core = state.player_core.lock().unwrap();
    Ok(core.snapshot(Instant::now()))
}

#[tauri::command]
pub fn player_toggle(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Snapshot, String> {
    Ok(with_core(&app, &state, |core, now| {
        let fx = core.toggle(now);
        (fx, core.snapshot(now))
    }))
}

#[tauri::command]
pub fn player_step(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    dir: i32,
) -> Result<Snapshot, String> {
    Ok(with_core(&app, &state, |core, now| {
        let fx = core.step(dir, now);
        (fx, core.snapshot(now))
    }))
}

#[tauri::command]
pub fn player_play_index(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    index: usize,
) -> Result<Snapshot, String> {
    Ok(with_core(&app, &state, |core, now| {
        let fx = core.play_index(index, now);
        (fx, core.snapshot(now))
    }))
}

#[tauri::command]
pub fn player_seek(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    secs: f64,
) -> Result<Snapshot, String> {
    Ok(with_core(&app, &state, |core, now| {
        core.seek(secs, now);
        (Effects::default(), core.snapshot(now))
    }))
}

#[tauri::command]
pub fn player_set_repeat(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    mode: RepeatMode,
) -> Result<Snapshot, String> {
    Ok(with_core(&app, &state, |core, now| {
        core.set_repeat(mode);
        (Effects::default(), core.snapshot(now))
    }))
}

#[tauri::command]
pub fn player_set_shuffle(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    on: bool,
) -> Result<Snapshot, String> {
    Ok(with_core(&app, &state, |core, now| {
        core.set_shuffle(on, now);
        (Effects::default(), core.snapshot(now))
    }))
}

#[tauri::command]
pub fn player_set_volume(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    volume: f64,
) -> Result<Snapshot, String> {
    Ok(with_core(&app, &state, |core, now| {
        let fx = core.set_volume(volume);
        (fx, core.snapshot(now))
    }))
}

#[tauri::command]
pub fn player_set_muted(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    muted: bool,
) -> Result<Snapshot, String> {
    Ok(with_core(&app, &state, |core, now| {
        let fx = core.set_muted(muted);
        (fx, core.snapshot(now))
    }))
}

#[tauri::command]
pub fn player_enqueue(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    entries: Vec<QueueEntry>,
    next: Option<bool>,
) -> Result<Snapshot, String> {
    Ok(with_core(&app, &state, |core, now| {
        core.enqueue(entries, next.unwrap_or(false));
        (Effects::default(), core.snapshot(now))
    }))
}

#[tauri::command]
pub fn player_remove(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    index: usize,
) -> Result<Snapshot, String> {
    Ok(with_core(&app, &state, |core, now| {
        let fx = core.remove(index, now);
        (fx, core.snapshot(now))
    }))
}

#[tauri::command]
pub fn player_clear_queue(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    keep_current: Option<bool>,
) -> Result<Snapshot, String> {
    Ok(with_core(&app, &state, |core, now| {
        let fx = core.clear_queue(keep_current.unwrap_or(false), now);
        (fx, core.snapshot(now))
    }))
}

#[tauri::command]
pub fn player_reorder(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    from: usize,
    to: usize,
) -> Result<Snapshot, String> {
    Ok(with_core(&app, &state, |core, now| {
        core.reorder(from, to);
        (Effects::default(), core.snapshot(now))
    }))
}

#[tauri::command]
pub fn player_stop(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Snapshot, String> {
    Ok(with_core(&app, &state, |core, now| {
        core.stop(now);
        (Effects::default(), core.snapshot(now))
    }))
}

/// Engine facts. Only the owner window's reports are honored —
/// a revoked surface's element may still fire events while it
/// tears down, and those must not move the core.
#[tauri::command]
pub fn player_fact(
    app: tauri::AppHandle,
    window: tauri::Window,
    state: State<'_, AppState>,
    kind: String,
    value: Option<f64>,
    message: Option<String>,
) -> Result<Snapshot, String> {
    let label = window.label().to_string();
    // The entry that is current as the fact arrives: an `ended`
    // fact completes THAT listen, whatever the core advances to.
    let ended_key = if kind == "ended" {
        let core = state.player_core.lock().unwrap();
        core.current_entry().map(|e| e.provider_key.clone())
    } else {
        None
    };
    let snap = with_core(&app, &state, |core, now| {
        let is_owner = core
            .snapshot(now)
            .owner
            .is_some()
            && label_matches(core, &label);
        if !is_owner {
            return (Effects::default(), core.snapshot(now));
        }
        if kind == "error" {
            core.fact_error(message.unwrap_or_else(|| "playback error".into()), now);
            return (Effects::default(), core.snapshot(now));
        }
        let fx = core.fact(&kind, value.unwrap_or(0.0), now);
        (fx, core.snapshot(now))
    });
    if let Some(key) = ended_key {
        if let Ok(cat) = state.catalog.lock() {
            let _ = cat.audio_set_position(&key, 0.0, true);
        }
    }
    Ok(snap)
}

fn label_matches(core: &Core, label: &str) -> bool {
    core.owner_label.as_deref() == Some(label)
}

#[tauri::command]
pub fn player_detach_gone(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Snapshot, String> {
    Ok(with_core(&app, &state, |core, now| {
        core.detach_gone(now);
        (Effects::default(), core.snapshot(now))
    }))
}

/// Spectrum bands ride from the owner engine to Rust and back out
/// to every window (the "Rust-fed" requirement, literally).
#[tauri::command]
pub fn player_spectrum(
    app: tauri::AppHandle,
    window: tauri::Window,
    state: State<'_, AppState>,
    bands: Vec<f32>,
) -> Result<(), String> {
    let label = window.label().to_string();
    let is_owner = {
        let core = state.player_core.lock().unwrap();
        label_matches(&core, &label)
    };
    if is_owner {
        let _ = app.emit("player-spectrum", bands);
    }
    Ok(())
}

#[tauri::command]
pub fn player_set_cast(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    cast: Option<CastMirror>,
) -> Result<Snapshot, String> {
    Ok(with_core(&app, &state, |core, now| {
        core.set_cast(cast);
        (Effects::default(), core.snapshot(now))
    }))
}

// -- play history commands -------------------------------------------------

/// Cached catalog durations for a batch of paths, positionally
/// aligned (None = not probed). Surfaces merge these at open so
/// durations are known before anything plays.
#[tauri::command]
pub fn audio_durations(
    state: State<'_, AppState>,
    paths: Vec<String>,
) -> Result<Vec<Option<f64>>, String> {
    let cat = state.catalog.lock().map_err(|_| "catalog lock".to_string())?;
    cat.durations_for_paths(&paths).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn audio_play_state(
    state: State<'_, AppState>,
    provider_key: String,
) -> Result<ufm_catalog::AudioPlayState, String> {
    let cat = state.catalog.lock().map_err(|_| "catalog lock".to_string())?;
    cat.audio_play_state_for(&provider_key)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn audio_recently_played(
    state: State<'_, AppState>,
    limit: Option<i64>,
) -> Result<Vec<ufm_catalog::RecentAudio>, String> {
    let cat = state.catalog.lock().map_err(|_| "catalog lock".to_string())?;
    cat.audio_recently_played(limit.unwrap_or(10))
        .map_err(|e| e.to_string())
}

/// Completed-listen bookkeeping rides the `ended` fact: when an
/// entry ends naturally the glue marks it complete. (Kept as a
/// command too so surfaces can mark a >90% listen complete on
/// skip-away, matching the ROM shelf's honesty.)
#[tauri::command]
pub fn audio_mark_completed(
    state: State<'_, AppState>,
    provider_key: String,
) -> Result<(), String> {
    let cat = state.catalog.lock().map_err(|_| "catalog lock".to_string())?;
    cat.audio_set_position(&provider_key, 0.0, true)
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Tests — the acceptance behaviors, pinned.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn entry(name: &str, dur: f64) -> QueueEntry {
        QueueEntry {
            provider_key: format!("/music/{name}"),
            name: name.into(),
            duration_secs: if dur > 0.0 { Some(dur) } else { None },
        }
    }

    fn queue3() -> Vec<QueueEntry> {
        vec![entry("a.flac", 100.0), entry("b.flac", 200.0), entry("c.flac", 300.0)]
    }

    #[test]
    fn claim_revokes_previous_owner() {
        // THE acceptance test (Dickles, 2026-10-16): preview B
        // starts while preview A plays → A is revoked, always.
        let t0 = Instant::now();
        let mut core = Core::new(0.8, false);
        let fx = core.open_queue(OwnerKind::QuickLook, Some("main".into()), queue3(), 0, 0.0, true, t0);
        assert!(fx.revoked.is_none());
        assert_eq!(core.snapshot(t0).owner, Some(OwnerKind::QuickLook));
        let fx = core.open_queue(OwnerKind::Panel, Some("main".into()), queue3(), 1, 0.0, true, t0);
        let (old, _) = fx.revoked.expect("panel claim must revoke quicklook");
        assert_eq!(old, OwnerKind::QuickLook);
        assert!(fx.count_play, "a fresh queue load starts a listen");
        let snap = core.snapshot(t0);
        assert_eq!(snap.owner, Some(OwnerKind::Panel));
        assert_eq!(snap.index, Some(1));
        assert_eq!(snap.state, PlayState::Playing);
        // A detach/return claim reloads the SAME listen: it must
        // load (engine swap) but never count as a new play.
        let fx = core.claim(OwnerKind::Mini, Some("player-x".into()), t0);
        assert!(fx.load);
        assert!(!fx.count_play);
        let (old, _) = fx.revoked.expect("mini claim revokes panel");
        assert_eq!(old, OwnerKind::Panel);
    }

    #[test]
    fn position_clock_runs_only_while_playing() {
        let t0 = Instant::now();
        let mut core = Core::new(0.8, false);
        core.open_queue(OwnerKind::Panel, None, queue3(), 0, 10.0, true, t0);
        let t1 = t0 + Duration::from_secs(5);
        assert!((core.position(t1) - 15.0).abs() < 0.001);
        core.toggle(t1); // pause at 15
        let t2 = t1 + Duration::from_secs(60);
        assert!((core.position(t2) - 15.0).abs() < 0.001);
        core.toggle(t2); // resume
        let t3 = t2 + Duration::from_secs(5);
        assert!((core.position(t3) - 20.0).abs() < 0.001);
        // The clock clamps at the known duration.
        let t4 = t2 + Duration::from_secs(500);
        assert!((core.position(t4) - 100.0).abs() < 0.001);
    }

    #[test]
    fn seek_clamps_and_rebases() {
        let t0 = Instant::now();
        let mut core = Core::new(0.8, false);
        core.open_queue(OwnerKind::Panel, None, queue3(), 0, 0.0, true, t0);
        core.seek(42.0, t0);
        assert!((core.position(t0) - 42.0).abs() < 0.001);
        core.seek(9999.0, t0);
        assert!((core.position(t0) - 100.0).abs() < 0.001);
        core.seek(-5.0, t0);
        assert!((core.position(t0) - 0.0).abs() < 0.001);
    }

    #[test]
    fn ended_advances_and_repeat_modes_decide() {
        let t0 = Instant::now();
        let mut core = Core::new(0.8, false);
        core.open_queue(OwnerKind::Panel, None, queue3(), 0, 0.0, true, t0);
        let fx = core.on_ended(t0);
        assert!(fx.load);
        assert_eq!(core.snapshot(t0).index, Some(1));
        // Repeat one: ended reloads the same entry.
        core.set_repeat(RepeatMode::One);
        core.on_ended(t0);
        assert_eq!(core.snapshot(t0).index, Some(1));
        assert_eq!(core.snapshot(t0).state, PlayState::Playing);
        // Repeat off at the tail: stop.
        core.set_repeat(RepeatMode::Off);
        core.step(1, t0);
        assert_eq!(core.snapshot(t0).index, Some(2));
        core.on_ended(t0);
        assert_eq!(core.snapshot(t0).state, PlayState::Stopped);
        // Repeat all wraps to the head.
        core.set_repeat(RepeatMode::All);
        core.toggle(t0);
        core.on_ended(t0);
        assert_eq!(core.snapshot(t0).index, Some(0));
        assert_eq!(core.snapshot(t0).state, PlayState::Playing);
    }

    #[test]
    fn shuffle_is_a_permutation_keeping_current_first() {
        let t0 = Instant::now();
        let mut core = Core::new(0.8, false);
        let entries: Vec<QueueEntry> = (0..10).map(|i| entry(&format!("t{i}.flac"), 60.0)).collect();
        core.open_queue(OwnerKind::Panel, None, entries, 4, 0.0, true, t0);
        core.set_shuffle(true, t0);
        let snap = core.snapshot(t0);
        let mut sorted = snap.order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..10).collect::<Vec<_>>());
        assert_eq!(snap.order[0], 4, "current entry leads the shuffle");
        assert_eq!(snap.index, Some(4));
        core.set_shuffle(false, t0);
        let snap = core.snapshot(t0);
        assert_eq!(snap.order, (0..10).collect::<Vec<_>>());
        assert_eq!(snap.index, Some(4));
    }

    #[test]
    fn queue_ops_keep_current_stable() {
        let t0 = Instant::now();
        let mut core = Core::new(0.8, false);
        core.open_queue(OwnerKind::Panel, None, queue3(), 1, 0.0, true, t0);
        // Enqueue-next lands right after the current in play order.
        core.enqueue(vec![entry("x.flac", 50.0)], true);
        let snap = core.snapshot(t0);
        assert_eq!(snap.queue.len(), 4);
        assert_eq!(snap.order, vec![0, 1, 3, 2]);
        // Removing a non-current entry keeps the current index.
        core.remove(0, t0);
        let snap = core.snapshot(t0);
        assert_eq!(snap.index, Some(0)); // b.flac shifted to 0
        assert_eq!(snap.queue[snap.index.unwrap()].name, "b.flac");
        // Reorder moves identity, not the playing entry.
        core.reorder(2, 0);
        let snap = core.snapshot(t0);
        assert_eq!(snap.queue[snap.index.unwrap()].name, "b.flac");
        // Removing the current entry advances with a load.
        let fx = core.remove(snap.index.unwrap(), t0);
        assert!(fx.load);
        // Clear with keep_current leaves exactly the current entry.
        core.clear_queue(true, t0);
        assert_eq!(core.snapshot(t0).queue.len(), 1);
        core.clear_queue(false, t0);
        assert_eq!(core.snapshot(t0).queue.len(), 0);
        assert_eq!(core.snapshot(t0).state, PlayState::Stopped);
    }

    #[test]
    fn drift_fact_adopts_element_truth() {
        let t0 = Instant::now();
        let mut core = Core::new(0.8, false);
        core.open_queue(OwnerKind::Panel, None, queue3(), 0, 0.0, true, t0);
        core.fact("position", 30.0, t0); // element says 30, clock says 0
        assert!((core.position(t0) - 30.0).abs() < 0.001);
        core.fact("position", 30.5, t0); // small drift: ignored
        assert!((core.position(t0) - 30.0).abs() < 0.001);
        core.fact("duration", 123.0, t0);
        assert_eq!(core.snapshot(t0).duration_secs, 123.0);
        assert_eq!(core.snapshot(t0).queue[0].duration_secs, Some(123.0));
    }

    #[test]
    fn resume_rule_pins() {
        // Short track: never resumes mid-way.
        assert_eq!(resume_position("song.flac", Some(200.0), 90.0), None);
        // Long track: resumes.
        assert_eq!(resume_position("mix.flac", Some(3600.0), 900.0), Some(900.0));
        // Audiobook extension qualifies without a known duration.
        assert_eq!(resume_position("book.m4b", None, 900.0), Some(900.0));
        // Barely started / at the credits: no resume.
        assert_eq!(resume_position("mix.flac", Some(3600.0), 3.0), None);
        assert_eq!(resume_position("mix.flac", Some(3600.0), 3595.0), None);
    }

    #[test]
    fn detach_gone_freezes_without_advancing() {
        let t0 = Instant::now();
        let mut core = Core::new(0.8, false);
        core.open_queue(OwnerKind::Mini, Some("player-x".into()), queue3(), 0, 0.0, true, t0);
        let t1 = t0 + Duration::from_secs(30);
        core.detach_gone(t1);
        let snap = core.snapshot(t1);
        assert_eq!(snap.owner, None);
        assert_eq!(snap.state, PlayState::Paused);
        assert_eq!(snap.index, Some(0));
        assert!((snap.position_secs - 30.0).abs() < 0.001);
    }
}
