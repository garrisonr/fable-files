//! ufm-tts — Fable's read-aloud engine room (Ebook P3).
//!
//! The app never links the speech runtime: synthesis lives in the
//! `fable-tts` helper process (the only sherpa-linking artifact),
//! spawned on first Listen and killed on stop/idle, so the engine's
//! memory exists exactly when the process does and a runtime
//! failure can never take the app down. This crate carries the
//! parts both sides share: sentence segmentation (the engine and
//! the reader's highlighter consume the same units), the voice
//! inventory, the model archive pin, the wire protocol, and the
//! client that drives the helper.
//!
//! The engine choice is Dickles' ruling (2026-10-18): Kokoro fp32
//! via sherpa-onnx, buffer-ahead playback. int8 measured *slower*
//! than fp32 on the target CPU (Phase 0 spike) and is out; Piper
//! has no license-clean voice and is out for v1 — the [`TtsEngine`]
//! trait is the door a future clean voice walks through.

pub mod client;
pub mod model;
pub mod protocol;
pub mod segment;
pub mod voices;

use serde::{Deserialize, Serialize};

pub use client::HelperEngine;
pub use segment::{segment_chapter, unit_text, SentenceRange};
pub use voices::{voice_by_id, Voice, Lexicon, DEFAULT_VOICE_ID, VOICES};

/// One synthesized narration unit: mono PCM.
#[derive(Debug, Clone, PartialEq)]
pub struct Audio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

impl Audio {
    pub fn duration_secs(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.samples.len() as f64 / self.sample_rate as f64
        }
    }
}

/// Everything that can go wrong, as a value the UI can explain in
/// plain language — never a panic, never a crash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum TtsError {
    /// The helper binary itself could not be found.
    HelperMissing(String),
    /// Model files are absent or incomplete (the detail names what).
    ModelMissing(String),
    /// The helper could not be started.
    Spawn(String),
    /// The helper died or stopped making sense mid-conversation.
    Protocol(String),
    /// One synthesis request failed (the session may live on).
    Synth(String),
}

impl std::fmt::Display for TtsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TtsError::HelperMissing(d) => write!(f, "the narrator program is missing: {d}"),
            TtsError::ModelMissing(d) => write!(f, "the narrator's voice files are missing: {d}"),
            TtsError::Spawn(d) => write!(f, "the narrator couldn't start: {d}"),
            TtsError::Protocol(d) => write!(f, "the narrator stopped answering: {d}"),
            TtsError::Synth(d) => write!(f, "the narrator stumbled: {d}"),
        }
    }
}

impl std::error::Error for TtsError {}

/// A text-to-speech engine. v1's implementation is [`HelperEngine`]
/// (Kokoro in the helper process); the trait is where a future
/// engine plugs in without the playback core changing.
pub trait TtsEngine: Send {
    /// Synthesize one narration unit in `voice` at `speed`
    /// (1.0 = the narrator's natural pace).
    fn synthesize(&mut self, text: &str, voice: &Voice, speed: f32) -> Result<Audio, TtsError>;
    /// Release the engine and everything it holds.
    fn shutdown(&mut self);
}
