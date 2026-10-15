//! musicid: MusicBrainz matching + tag-completion planning —
//! the pure half of Music Completion (Dickles, 2026-10-15), the
//! music sibling of videoid. Everything here is deterministic and
//! network-free: the MusicBrainz JSON → candidate mapping, the
//! confidence scorer (three tiers, evidence strings — the mediares
//! honesty rules), the music-filename parser, and the tag-plan
//! builder.
//!
//! THE LAW the plan builder enforces: fill ONLY empty fields.
//! A candidate value that would change a non-empty tag becomes an
//! overwrite proposal, flagged as such — batch apply skips those
//! unless a human confirmed that exact field.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// The tag view: the eleven fields Music Completion may complete.
// All strings (numbers ride as their decimal text) so plans,
// journals, and the review UI share one shape.
// ---------------------------------------------------------------------------

pub const FIELDS: [&str; 11] = [
    "title",
    "artist",
    "album",
    "albumArtist",
    "year",
    "track",
    "trackTotal",
    "disc",
    "discTotal",
    "genre",
    "composer",
];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioTags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub year: Option<String>,
    pub track: Option<String>,
    pub track_total: Option<String>,
    pub disc: Option<String>,
    pub disc_total: Option<String>,
    pub genre: Option<String>,
    pub composer: Option<String>,
}

impl AudioTags {
    pub fn get(&self, field: &str) -> Option<&str> {
        let v = match field {
            "title" => &self.title,
            "artist" => &self.artist,
            "album" => &self.album,
            "albumArtist" => &self.album_artist,
            "year" => &self.year,
            "track" => &self.track,
            "trackTotal" => &self.track_total,
            "disc" => &self.disc,
            "discTotal" => &self.disc_total,
            "genre" => &self.genre,
            "composer" => &self.composer,
            _ => return None,
        };
        v.as_deref().map(str::trim).filter(|s| !s.is_empty())
    }

    pub fn set(&mut self, field: &str, value: Option<String>) {
        let slot = match field {
            "title" => &mut self.title,
            "artist" => &mut self.artist,
            "album" => &mut self.album,
            "albumArtist" => &mut self.album_artist,
            "year" => &mut self.year,
            "track" => &mut self.track,
            "trackTotal" => &mut self.track_total,
            "disc" => &mut self.disc,
            "discTotal" => &mut self.disc_total,
            "genre" => &mut self.genre,
            "composer" => &mut self.composer,
            _ => return,
        };
        *slot = value.and_then(|v| {
            let t = v.trim().to_string();
            if t.is_empty() {
                None
            } else {
                Some(t)
            }
        });
    }

    /// The identity-bearing minimum: enough to search by.
    pub fn has_identity(&self) -> bool {
        self.get("title").is_some() && self.get("artist").is_some()
    }
}

// ---------------------------------------------------------------------------
// MusicBrainz candidates (mapped from the search / lookup JSON).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MbCandidate {
    /// MusicBrainz recording id.
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub release_id: Option<String>,
    pub release_group_id: Option<String>,
    pub year: Option<u32>,
    pub track: Option<u32>,
    pub track_total: Option<u32>,
    pub disc: Option<u32>,
    pub disc_total: Option<u32>,
    pub genre: Option<String>,
    pub composer: Option<String>,
    pub duration_ms: Option<u64>,
    /// MusicBrainz's own search score, 0–100.
    pub mb_score: u8,
}

fn artist_credit_name(v: &serde_json::Value) -> String {
    let mut out = String::new();
    if let Some(credits) = v.get("artist-credit").and_then(|c| c.as_array()) {
        for c in credits {
            let name = c
                .get("name")
                .and_then(|n| n.as_str())
                .or_else(|| c.get("artist").and_then(|a| a.get("name")).and_then(|n| n.as_str()))
                .unwrap_or_default();
            out.push_str(name);
            out.push_str(c.get("joinphrase").and_then(|j| j.as_str()).unwrap_or_default());
        }
    }
    out.trim().to_string()
}

fn year_of(date: Option<&str>) -> Option<u32> {
    date?.get(..4)?.parse().ok()
}

/// Pick the release a completion should describe: Official +
/// Album-primary first, then earliest date. Returns (release id,
/// release-group id, album title, year).
fn pick_release(
    releases: &[serde_json::Value],
) -> Option<(String, Option<String>, Option<String>, Option<u32>)> {
    let mut best: Option<(&serde_json::Value, i32)> = None;
    for r in releases {
        let mut rank = 0;
        if r.get("status").and_then(|s| s.as_str()) == Some("Official") {
            rank += 2;
        }
        let rg = r.get("release-group");
        if rg.and_then(|g| g.get("primary-type")).and_then(|t| t.as_str()) == Some("Album") {
            rank += 1;
        }
        // Earlier dates win ties: fold the year in negatively.
        let y = year_of(r.get("date").and_then(|d| d.as_str())).unwrap_or(9999) as i32;
        let score = rank * 10000 - y;
        if best.map(|(_, s)| score > s).unwrap_or(true) {
            best = Some((r, score));
        }
    }
    let (r, _) = best?;
    let id = r.get("id")?.as_str()?.to_string();
    let rg = r.get("release-group");
    Some((
        id,
        rg.and_then(|g| g.get("id")).and_then(|i| i.as_str()).map(str::to_string),
        r.get("title").and_then(|t| t.as_str()).map(str::to_string),
        year_of(r.get("date").and_then(|d| d.as_str())),
    ))
}

fn genre_of(v: &serde_json::Value) -> Option<String> {
    // Lookup responses carry `genres` (name + count); search
    // responses carry `tags`. Either way: highest count wins,
    // title-cased for tag duty.
    for key in ["genres", "tags"] {
        if let Some(arr) = v.get(key).and_then(|g| g.as_array()) {
            let mut best: Option<(&str, u64)> = None;
            for g in arr {
                let name = g.get("name").and_then(|n| n.as_str())?;
                let count = g.get("count").and_then(|c| c.as_u64()).unwrap_or(0);
                if best.map(|(_, c)| count > c).unwrap_or(true) {
                    best = Some((name, count));
                }
            }
            if let Some((name, _)) = best {
                return Some(title_case(name));
            }
        }
    }
    None
}

fn title_case(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(f) => f.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Map a MusicBrainz recording SEARCH response (`recordings[]`)
/// into candidates. Pure — unit-tested against the real shape.
pub fn map_search_response(json: &serde_json::Value) -> Vec<MbCandidate> {
    let mut out = Vec::new();
    let Some(recs) = json.get("recordings").and_then(|r| r.as_array()) else {
        return out;
    };
    for r in recs {
        let Some(id) = r.get("id").and_then(|i| i.as_str()) else {
            continue;
        };
        let releases = r
            .get("releases")
            .and_then(|x| x.as_array())
            .cloned()
            .unwrap_or_default();
        let (release_id, rg_id, album, year) = match pick_release(&releases) {
            Some((id, rg, t, y)) => (Some(id), rg, t, y),
            None => (None, None, None, None),
        };
        out.push(MbCandidate {
            id: id.to_string(),
            title: r
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string(),
            artist: artist_credit_name(r),
            album,
            album_artist: None,
            release_id,
            release_group_id: rg_id,
            year,
            track: None,
            track_total: None,
            disc: None,
            disc_total: None,
            genre: genre_of(r),
            composer: None,
            duration_ms: r.get("length").and_then(|l| l.as_u64()),
            mb_score: r.get("score").and_then(|s| s.as_u64()).unwrap_or(0) as u8,
        });
    }
    out
}

/// Merge a recording LOOKUP response (`inc=releases+genres+…`)
/// into a search candidate: track/disc numbers come from the
/// release's media listing, genre from the lookup's genre list,
/// composer from work relations when MusicBrainz exposes them.
pub fn merge_lookup(cand: &mut MbCandidate, json: &serde_json::Value) {
    if let Some(g) = genre_of(json) {
        cand.genre = Some(g);
    }
    // Composer: work relations occasionally carry the writer.
    if let Some(rels) = json.get("relations").and_then(|r| r.as_array()) {
        for rel in rels {
            if rel.get("type").and_then(|t| t.as_str()) == Some("composer") {
                if let Some(name) = rel
                    .get("artist")
                    .and_then(|a| a.get("name"))
                    .and_then(|n| n.as_str())
                {
                    cand.composer = Some(name.to_string());
                }
            }
        }
    }
    let Some(releases) = json.get("releases").and_then(|r| r.as_array()) else {
        return;
    };
    // Anchor on the release the search picked (else re-pick).
    let anchor = cand
        .release_id
        .clone()
        .and_then(|id| releases.iter().find(|r| r.get("id").and_then(|i| i.as_str()) == Some(&id)))
        .or_else(|| {
            pick_release(releases).and_then(|(id, _, _, _)| {
                releases.iter().find(|r| r.get("id").and_then(|i| i.as_str()) == Some(&id))
            })
        });
    let Some(rel) = anchor else { return };
    cand.release_id = rel
        .get("id")
        .and_then(|i| i.as_str())
        .map(str::to_string)
        .or(cand.release_id.take());
    if cand.album.is_none() {
        cand.album = rel.get("title").and_then(|t| t.as_str()).map(str::to_string);
    }
    if cand.year.is_none() {
        cand.year = year_of(rel.get("date").and_then(|d| d.as_str()));
    }
    let Some(media) = rel.get("media").and_then(|m| m.as_array()) else {
        return;
    };
    cand.disc_total = Some(media.len() as u32);
    for (mi, medium) in media.iter().enumerate() {
        let count = medium
            .get("track-count")
            .and_then(|c| c.as_u64())
            .unwrap_or(0) as u32;
        if let Some(tracks) = medium.get("tracks").and_then(|t| t.as_array()) {
            for t in tracks {
                let is_ours = t
                    .get("recording")
                    .and_then(|r| r.get("id"))
                    .and_then(|i| i.as_str())
                    == Some(cand.id.as_str())
                    || t.get("id").and_then(|i| i.as_str()) == Some(cand.id.as_str());
                if is_ours {
                    cand.track = t
                        .get("position")
                        .and_then(|p| p.as_u64())
                        .map(|p| p as u32)
                        .or_else(|| {
                            t.get("number")
                                .and_then(|n| n.as_str())
                                .and_then(|n| n.trim().parse().ok())
                        });
                    cand.track_total = Some(count);
                    cand.disc = Some(mi as u32 + 1);
                    return;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Filename parsing (the stage before any network): the conventions
// of real music libraries — "01 - Title", "Artist - Title",
// "Artist - Album - 01 - Title", "01. Artist - Title".
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedMusicName {
    pub track: Option<u32>,
    pub artist: Option<String>,
    pub title: Option<String>,
    pub album: Option<String>,
    pub year: Option<u32>,
}

fn leading_track(s: &str) -> (Option<u32>, &str) {
    let t = s.trim_start();
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() && digits.len() <= 3 {
        let rest = &t[digits.len()..];
        // Only a track number when a separator follows (or the
        // whole name is the number) — "1979" alone is a title.
        if rest.is_empty() || rest.starts_with(['.', ' ', '-', '_']) {
            if let Ok(n) = digits.parse::<u32>() {
                if n > 0 {
                    return (Some(n), rest.trim_start_matches(['.', ' ', '-', '_']));
                }
            }
        }
    }
    (None, s)
}

fn trailing_year(s: &str) -> (Option<u32>, String) {
    let t = s.trim();
    // "(1970)" or "[1970]" at the end.
    for (open, close) in [('(', ')'), ('[', ']')] {
        if t.ends_with(close) {
            if let Some(start) = t.rfind(open) {
                let inner = &t[start + 1..t.len() - 1];
                if inner.len() == 4 {
                    if let Ok(y) = inner.parse::<u32>() {
                        if (1900..=2099).contains(&y) {
                            return (Some(y), t[..start].trim().to_string());
                        }
                    }
                }
            }
        }
    }
    (None, t.to_string())
}

pub fn parse_music_filename(stem: &str) -> ParsedMusicName {
    let cleaned = stem.replace('_', " ");
    let (track, rest) = leading_track(&cleaned);
    let parts: Vec<String> = rest
        .split(" - ")
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    let mut out = ParsedMusicName {
        track,
        ..Default::default()
    };
    let mut assign_title = |out: &mut ParsedMusicName, raw: String| {
        let (year, title) = trailing_year(&raw);
        if year.is_some() {
            out.year = year;
        }
        if !title.is_empty() {
            out.title = Some(title);
        }
    };
    match parts.len() {
        0 => {}
        1 => assign_title(&mut out, parts[0].clone()),
        2 => {
            // "Artist - Title" (a leading track was already eaten).
            out.artist = Some(parts[0].clone());
            assign_title(&mut out, parts[1].clone());
        }
        _ => {
            out.artist = Some(parts[0].clone());
            // "Artist - 03 - Title" vs "Artist - Album - Title" vs
            // "Artist - Album - 03 - Title".
            let (mid_track, _) = leading_track(&parts[1]);
            if mid_track.is_some() && out.track.is_none() {
                out.track = mid_track;
                assign_title(&mut out, parts[2..].join(" - "));
            } else if parts.len() >= 4 {
                let (seg_track, seg_rest) = leading_track(&parts[2]);
                if seg_track.is_some() && seg_rest.is_empty() && out.track.is_none() {
                    out.track = seg_track;
                    let (year, album) = trailing_year(&parts[1]);
                    if year.is_some() {
                        out.year = year;
                    }
                    out.album = Some(album);
                    assign_title(&mut out, parts[3..].join(" - "));
                } else {
                    let (year, album) = trailing_year(&parts[1]);
                    if year.is_some() {
                        out.year = year;
                    }
                    out.album = Some(album);
                    assign_title(&mut out, parts[2..].join(" - "));
                }
            } else {
                let (year, album) = trailing_year(&parts[1]);
                if year.is_some() {
                    out.year = year;
                }
                out.album = Some(album);
                assign_title(&mut out, parts[2..].join(" - "));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Similarity + confidence scoring (mediares honesty: tiers plus
// the evidence strings that earned them).
// ---------------------------------------------------------------------------

pub const CONFIDENT_MIN: f64 = 0.85;
pub const PROBABLE_MIN: f64 = 0.55;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Confident,
    Probable,
    Unknown,
}

pub fn tier_for(score: f64) -> Tier {
    if score >= CONFIDENT_MIN {
        Tier::Confident
    } else if score >= PROBABLE_MIN {
        Tier::Probable
    } else {
        Tier::Unknown
    }
}

/// Normalize for comparison: lowercase, feat./ft. segments cut,
/// punctuation → space, whitespace collapsed.
pub fn normalize(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut cut = lower.as_str();
    for marker in [" feat. ", " feat ", " ft. ", " featuring "] {
        if let Some(i) = cut.find(marker) {
            cut = &cut[..i];
            break;
        }
    }
    let mut out = String::with_capacity(cut.len());
    let mut last_space = true;
    for c in cut.chars() {
        if c.is_alphanumeric() {
            out.push(c);
            last_space = false;
        } else if !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    out.trim().to_string()
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur: Vec<usize> = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// 0..1 similarity: exact = 1; else the better of token-Dice and
/// Levenshtein ratio, with a containment bonus for short extras
/// ("Creep" vs "Creep (Remastered)" after normalize is exact, but
/// "Creep Live" still deserves most of the credit).
pub fn similarity(a: &str, b: &str) -> f64 {
    let (a, b) = (normalize(a), normalize(b));
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    let at: Vec<&str> = a.split(' ').collect();
    let bt: Vec<&str> = b.split(' ').collect();
    let inter = at.iter().filter(|t| bt.contains(t)).count() as f64;
    let dice = 2.0 * inter / (at.len() + bt.len()) as f64;
    let lev = 1.0 - (levenshtein(&a, &b) as f64 / a.len().max(b.len()) as f64);
    let mut best = dice.max(lev);
    if (a.contains(&b) || b.contains(&a)) && a.len().min(b.len()) >= 3 {
        best = best.max(0.9);
    }
    best.clamp(0.0, 1.0)
}

/// What we know about the local file, for scoring.
#[derive(Debug, Clone, Default)]
pub struct LocalInfo {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_secs: Option<f64>,
}

/// Score one candidate against the local file. Returns the score
/// and the evidence that produced it (the UI shows these verbatim
/// — a score without reasons is a guess wearing a lab coat).
pub fn score_candidate(local: &LocalInfo, cand: &MbCandidate) -> (f64, Vec<String>) {
    let mut evidence: Vec<String> = Vec::new();
    let mut weighted = 0.0f64;
    let mut weights = 0.0f64;
    let mut add = |w: f64, v: f64, why: String| {
        weighted += w * v;
        weights += w;
        evidence.push(why);
    };

    add(
        0.30,
        cand.mb_score as f64 / 100.0,
        format!("MusicBrainz rates this match {} / 100", cand.mb_score),
    );
    if let Some(t) = &local.title {
        let sim = similarity(t, &cand.title);
        add(
            0.28,
            sim,
            if sim >= 0.99 {
                "title matches exactly".into()
            } else if sim >= 0.7 {
                format!("title is close (\"{t}\" vs \"{}\")", cand.title)
            } else {
                format!("title differs (\"{t}\" vs \"{}\")", cand.title)
            },
        );
    }
    if let Some(a) = &local.artist {
        let sim = similarity(a, &cand.artist);
        add(
            0.22,
            sim,
            if sim >= 0.99 {
                "artist matches exactly".into()
            } else if sim >= 0.7 {
                format!("artist is close (\"{a}\" vs \"{}\")", cand.artist)
            } else {
                format!("artist differs (\"{a}\" vs \"{}\")", cand.artist)
            },
        );
    }
    if let (Some(la), Some(ca)) = (&local.album, &cand.album) {
        let sim = similarity(la, ca);
        add(
            0.10,
            sim,
            if sim >= 0.99 {
                "album matches exactly".into()
            } else {
                format!("album differs (\"{la}\" vs \"{ca}\")")
            },
        );
    }
    if let (Some(d), Some(ms)) = (local.duration_secs, cand.duration_ms) {
        let cd = ms as f64 / 1000.0;
        let delta = (d - cd).abs();
        let v = if delta <= 2.0 {
            1.0
        } else if delta <= 5.0 {
            0.7
        } else if delta <= 10.0 {
            0.35
        } else {
            0.05
        };
        add(
            0.10,
            v,
            format!("running time is within {:.0}s of the recording's", delta),
        );
    }
    let score = if weights > 0.0 {
        (weighted / weights).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (score, evidence)
}

// ---------------------------------------------------------------------------
// The tag plan: candidate values vs current tags, per field.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagChange {
    pub field: String,
    /// What's in the file now (None = empty).
    pub current: Option<String>,
    /// What completion proposes.
    pub proposed: String,
    /// True when the field is NON-empty and the value differs —
    /// the law says these never apply without a human's explicit
    /// per-field confirmation.
    pub overwrite: bool,
}

fn num_str(v: Option<u32>) -> Option<String> {
    v.map(|n| n.to_string())
}

/// Build the completion plan for one matched file. Values that
/// agree with what's already tagged are not changes at all.
pub fn plan_changes(current: &AudioTags, cand: &MbCandidate) -> Vec<TagChange> {
    let proposed: [(&str, Option<String>); 11] = [
        ("title", Some(cand.title.clone())),
        ("artist", Some(cand.artist.clone())),
        ("album", cand.album.clone()),
        ("albumArtist", cand.album_artist.clone()),
        ("year", num_str(cand.year)),
        ("track", num_str(cand.track)),
        ("trackTotal", num_str(cand.track_total)),
        ("disc", num_str(cand.disc)),
        ("discTotal", num_str(cand.disc_total)),
        ("genre", cand.genre.clone()),
        ("composer", cand.composer.clone()),
    ];
    let mut out = Vec::new();
    for (field, prop) in proposed {
        let Some(prop) = prop.filter(|p| !p.trim().is_empty()) else {
            continue;
        };
        let cur = current.get(field).map(str::to_string);
        match &cur {
            None => out.push(TagChange {
                field: field.to_string(),
                current: None,
                proposed: prop,
                overwrite: false,
            }),
            Some(c) if !values_agree(field, c, &prop) => out.push(TagChange {
                field: field.to_string(),
                current: cur,
                proposed: prop,
                overwrite: true,
            }),
            _ => {}
        }
    }
    out
}

fn head_number(s: &str) -> &str {
    // "3/12" style combined fields: take the head number's text.
    s.split('/').next().unwrap_or(s)
}

fn parse_num(s: &str) -> Option<u64> {
    let t = head_number(s).trim();
    t.trim_start_matches('0').parse::<u64>().ok().or(if t.chars().all(|c| c == '0') && !t.is_empty() {
        Some(0)
    } else {
        None
    })
}

/// Do a current value and a proposed value "agree"? Text fields
/// compare normalized; numeric fields compare as numbers (so
/// "03" agrees with "3", "1970" with "1970").
fn values_agree(field: &str, current: &str, proposed: &str) -> bool {
    if matches!(field, "year" | "track" | "trackTotal" | "disc" | "discTotal") {
        let a = parse_num(current);
        a.is_some() && a == parse_num(proposed)
    } else {
        normalize(current) == normalize(proposed)
    }
}

#[cfg(test)]
mod tests;
