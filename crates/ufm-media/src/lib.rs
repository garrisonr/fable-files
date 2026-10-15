//! ufm-media: media identity — the zero-config Media Library.
//!
//! Spec §§14–15: type "super mario" and the ROM is found and playable; type a
//! movie and it plays; type "Hitchcock" and every local Hitchcock plus
//! TMDB/IMDb/Wikipedia links appear whether owned or not.
//!
//! Standing product rule: media identification is a **classifier over the
//! already-global search index**. There is no "point me at your media folders"
//! setup screen. The matching datasets are small, ship with the app, and
//! update silently in the background.

use serde::{Deserialize, Serialize};

pub mod oshash;
pub mod romdat;
pub mod videoid;

// ---------------------------------------------------------------------------
// MediaIdentity — what the classifier decided a file is, with evidence.
// Matches are never silent guesses: evidence + override history are stored
// (spec §26 core records).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaIdentity {
    pub kind: MediaKind,
    pub title: String,
    pub year: Option<u32>,
    /// External references: tmdb, imdb, wikipedia, nointro…
    pub refs: Vec<ExternalRef>,
    pub confidence: f32, // 0.0–1.0
    pub evidence: Vec<String>,
    /// Human overrides win over the classifier, permanently.
    pub overridden: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Movie,
    Episode,
    Music,
    Audiobook,
    Ebook,
    Rom,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalRef {
    pub source: String, // "tmdb" | "imdb" | "wikipedia" | "nointro"
    pub id: String,
    pub url: String,
}

impl MediaIdentity {
    pub fn tmdb_url(&self) -> Option<&str> {
        self.refs
            .iter()
            .find(|r| r.source == "tmdb")
            .map(|r| r.url.as_str())
    }
}

// ---------------------------------------------------------------------------
// Filename parsing — the first classifier stage. `Dune.Part.Two.2024.1080p`
// → title "Dune Part Two", year 2024, tags [1080p].
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParsedName {
    pub title: String,
    pub year: Option<u32>,
    pub tags: Vec<String>,
}

/// Parse a media filename into title/year/tags. Heuristic, deterministic,
/// no network — the TMDB lookup is a later stage that consumes this.
pub fn parse_filename(stem: &str) -> ParsedName {
    let mut title = stem.replace(['.', '_'], " ");
    let mut year = None;
    let mut tags = Vec::new();

    // Year: last 4-digit group in 1900..=2099.
    let words: Vec<&str> = title.split_whitespace().collect();
    let mut title_words = Vec::new();
    for w in words {
        if w.len() == 4 && w.chars().all(|c| c.is_ascii_digit()) {
            if let Ok(y) = w.parse::<u32>() {
                if (1900..=2099).contains(&y) && year.is_none() {
                    year = Some(y);
                    continue;
                }
            }
        }
        let lw = w.to_lowercase();
        if ["1080p", "720p", "2160p", "4k", "bluray", "web-dl", "x264", "x265", "hevc"]
            .contains(&lw.as_str())
        {
            tags.push(lw);
            continue;
        }
        title_words.push(w);
    }
    title = title_words.join(" ");
    // Strip common group tags like [YTS] at the edges.
    title = title
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();

    ParsedName { title, year, tags }
}

// ---------------------------------------------------------------------------
// ROM datasets — compact No-Intro-style hash sets shipped with the app and
// updated silently. Matching is hash + filename; the user never configures
// which folders "are games".
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RomDatasetInfo {
    pub platform: String, // "nes" | "snes" | "genesis" | …
    pub version: String,
    pub entry_count: u64,
    pub updated: chrono::DateTime<chrono::Utc>,
}

/// Stub: real datasets ship as compressed sidecar files in P1.
pub fn bundled_rom_platforms() -> Vec<&'static str> {
    vec![
        "nes", "snes", "n64", "genesis", "mastersystem", "gb", "gbc", "gba", "psx",
        "atari2600", "c64", "amiga", "arcade",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_movie_filename() {
        let p = parse_filename("Dune.Part.Two.2024.1080p.BluRay.x264");
        assert_eq!(p.title, "Dune Part Two");
        assert_eq!(p.year, Some(2024));
        assert!(p.tags.contains(&"1080p".to_string()));
        assert!(p.tags.contains(&"x264".to_string()));
    }

    #[test]
    fn parses_plain_title() {
        let p = parse_filename("The Greasy Strangler");
        assert_eq!(p.title, "The Greasy Strangler");
        assert_eq!(p.year, None);
    }
}
