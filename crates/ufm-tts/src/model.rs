//! The Kokoro model pin. The archive identity was measured in
//! Phase 0 (sha256 read off the downloaded bytes on the G9), and
//! the pin is the contract: a download that doesn't hash to this
//! is not the narrator, whatever the server called it.

use std::path::{Path, PathBuf};

/// sherpa-onnx's tts-models release, the fp32 multi-lang v1_0
/// archive (Apache-2.0; see the Credits page).
pub const ARCHIVE_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/kokoro-multi-lang-v1_0.tar.bz2";
pub const ARCHIVE_FILE: &str = "kokoro-multi-lang-v1_0.tar.bz2";
pub const ARCHIVE_SHA256: &str =
    "c5f7e2d2caf082bc1d20fb70334a61d99d20b484500aad32e7cf84c128ea3298";
pub const ARCHIVE_BYTES: u64 = 349_906_910;
/// The folder the archive extracts to (and the model dir's name).
pub const MODEL_DIR_NAME: &str = "kokoro-multi-lang-v1_0";
pub const MODEL_FILE: &str = "model.onnx";

/// Every path the runtime validates BEFORE constructing a session
/// (sherpa-rs swallows creation failure into a null session that
/// segfaults on first use — pre-validation is the only safe door).
/// Relative to the model dir.
pub const REQUIRED_PATHS: &[&str] = &[
    "model.onnx",
    "voices.bin",
    "tokens.txt",
    "lexicon-us-en.txt",
    "lexicon-gb-en.txt",
    "dict",
    "espeak-ng-data/phontab",
];

/// What's missing from `model_dir` (empty = ready to narrate).
pub fn missing_paths(model_dir: &Path) -> Vec<&'static str> {
    REQUIRED_PATHS
        .iter()
        .copied()
        .filter(|rel| !model_dir.join(rel).exists())
        .collect()
}

pub fn model_ready(model_dir: &Path) -> bool {
    missing_paths(model_dir).is_empty()
}

/// The model dir inside the app's TTS store (`<base>/kokoro-…`).
pub fn model_dir(base: &Path) -> PathBuf {
    base.join(MODEL_DIR_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_dir_is_missing_everything() {
        let dir = std::env::temp_dir().join("ufm-tts-no-such-dir-xyz");
        assert_eq!(missing_paths(&dir).len(), REQUIRED_PATHS.len());
        assert!(!model_ready(&dir));
    }
}
