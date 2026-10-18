//! The narrator inventory — the seven voices of the Kokoro
//! multi-lang **v1_0** cast (Phase 0 measured their speaker ids
//! from the model's own metadata; the v1_1 archive is a different
//! cast entirely — archives are pinned by cast, never by version).
//!
//! `af_heart` is the placeholder default until Dickles picks a
//! narrator by ear from the Phase 0 samples.

use serde::{Deserialize, Serialize};

/// Which lexicon the phonemizer loads for a voice. The helper
/// builds its session per lexicon and rebuilds when a request
/// crosses the line (a voice switch is a user gesture; the ~3 s
/// rebuild hides inside it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lexicon {
    Us,
    Gb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Voice {
    pub id: &'static str,
    pub label: &'static str,
    pub sid: i32,
    pub lexicon: Lexicon,
}

pub const VOICES: &[Voice] = &[
    Voice { id: "af_heart", label: "Heart · US, female", sid: 3, lexicon: Lexicon::Us },
    Voice { id: "af_bella", label: "Bella · US, female", sid: 2, lexicon: Lexicon::Us },
    Voice { id: "am_adam", label: "Adam · US, male", sid: 11, lexicon: Lexicon::Us },
    Voice { id: "am_michael", label: "Michael · US, male", sid: 16, lexicon: Lexicon::Us },
    Voice { id: "bf_emma", label: "Emma · British, female", sid: 21, lexicon: Lexicon::Gb },
    Voice { id: "bm_george", label: "George · British, male", sid: 26, lexicon: Lexicon::Gb },
    Voice { id: "bm_fable", label: "Fable · British, male", sid: 25, lexicon: Lexicon::Gb },
];

pub const DEFAULT_VOICE_ID: &str = "af_heart";

pub fn voice_by_id(id: &str) -> Option<&'static Voice> {
    VOICES.iter().find(|v| v.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_seven_voices_of_the_v1_0_cast() {
        assert_eq!(VOICES.len(), 7);
        let mut sids: Vec<i32> = VOICES.iter().map(|v| v.sid).collect();
        sids.sort_unstable();
        assert_eq!(sids, vec![2, 3, 11, 16, 21, 25, 26]);
        assert_eq!(voice_by_id(DEFAULT_VOICE_ID).map(|v| v.sid), Some(3));
        assert!(voice_by_id("af_maple").is_none(), "v1_1 cast must not resolve");
    }
}
