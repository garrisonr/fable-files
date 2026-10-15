//! videoid: filename resolution for film + TV — the video twin of the
//! ROM DAT identification. Pure, deterministic, no I/O, no network:
//!
//! 1. `parse_video_filename` turns release-soup names
//!    (`Show.Name.1x03.[1080p].mkv`, `Dune.Part.Two.2024.BluRay.x264`)
//!    into structured facts with the junk honestly recorded as tags.
//! 2. `score_candidate` / `rank_candidates` grade TMDB candidates
//!    against a parse with evidence strings, in three tiers —
//!    Confident / Probable / Unknown. Probable and Unknown are never
//!    auto-accepted anywhere in Fable (the confirm flow owns them).
//! 3. The `*_target_rel` builders produce the ONE canonical layout
//!    Plex, Emby and Jellyfin all accept (see the Media Resolution
//!    research note): `Title (Year)/Title (Year).ext` for films,
//!    `Show (Year)/Season NN/Show (Year) - S01E01-E02.ext` for TV.
//!
//! Hard honesty rules baked in: anime-style absolute numbering and
//! date-based episode markers are FLAGGED, never guessed into S/E
//! numbers; a spelled-out sequel part ("Dune Part Two") stays in the
//! title; nothing here ever invents an episode title or a year.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParsedKind {
    Movie,
    Episode,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Confident,
    Probable,
    Unknown,
}

/// Everything the parser could honestly extract from one file name.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParsedVideo {
    pub kind: ParsedKind,
    /// Cleaned title (show name for episodes). Empty when nothing
    /// usable survived cleaning — the caller treats that as unparsed.
    pub title: String,
    pub year: Option<u32>,
    pub season: Option<u32>,
    pub episode_first: Option<u32>,
    pub episode_last: Option<u32>,
    /// Split-file part label, normalized: "pt1" | "cd2" | "disc1" |
    /// "part1"… Only set for stack words, or for "part N" appearing
    /// AFTER the year (a "Part 2" before the year is a sequel title).
    pub part: Option<String>,
    /// Text after the episode marker, junk-stripped — a hint scored
    /// against TMDB episode names, never trusted on its own.
    pub episode_title_hint: Option<String>,
    /// Anime-style absolute episode number. Present = the name uses
    /// absolute numbering; season/episode stay None on purpose.
    pub absolute_episode: Option<u32>,
    /// Date-based episode marker, ISO form (YYYY-MM-DD).
    pub air_date: Option<String>,
    /// `{tmdb-123}` / `[tmdbid=123]` tag found in the name.
    pub explicit_tmdb_id: Option<i64>,
    /// `{imdb-tt123}` tag found in the name.
    pub explicit_imdb_id: Option<String>,
    /// Edition from an `{edition-…}` tag or a Directors Cut /
    /// Extended / Unrated / Final Cut phrase.
    pub edition: Option<String>,
    /// Junk tokens stripped while cleaning (lowercase), plus the
    /// contents of bracket groups. Evidence, not decoration.
    pub tags: Vec<String>,
    /// Machine-readable honesty flags: "absolute_numbering",
    /// "date_based", "no_year", "no_title".
    pub flags: Vec<String>,
}

impl Default for ParsedKind {
    fn default() -> Self {
        ParsedKind::Unknown
    }
}

// ---------------------------------------------------------------------------
// Token machinery
// ---------------------------------------------------------------------------

fn is_year_token(tok: &str) -> Option<u32> {
    if tok.len() == 4 && tok.chars().all(|c| c.is_ascii_digit()) {
        let y: u32 = tok.parse().ok()?;
        if (1920..=2035).contains(&y) {
            return Some(y);
        }
    }
    None
}

/// Junk vocabulary: quality/source/codec/audio/release tokens that are
/// never part of a title when they appear as whole tokens. Numeric
/// channel fragments ("5", "1") are handled contextually, not here —
/// a bare digit can be a title word ("Apollo 13" survives because 13
/// is not in this set and is never stripped mid-title).
fn is_junk(tok: &str) -> bool {
    matches!(
        tok,
        "480p" | "576p" | "720p" | "900p" | "1080p" | "1440p" | "2160p" | "4320p"
            | "4k" | "8k" | "uhd" | "fhd" | "hd" | "sd"
            | "bluray" | "blu" | "ray" | "bdrip" | "brrip" | "bd" | "dvdrip" | "hdrip"
            | "hdtv" | "webrip" | "webdl" | "web" | "dl" | "dvd" | "screener" | "dvdscr"
            | "cam" | "telesync" | "ts" | "r5" | "remux"
            | "x264" | "x265" | "h264" | "h265" | "hevc" | "avc" | "av1" | "xvid" | "divx"
            | "264" | "265" | "vc1" | "10bit" | "hi10p" | "8bit"
            | "aac" | "ac3" | "eac3" | "dts" | "dtshd" | "truehd" | "atmos" | "mp3"
            | "flac" | "dd" | "ddp" | "dd5" | "ddp5" | "dd51" | "ddp51" | "51" | "71"
            | "hdr" | "hdr10" | "hdr10plus" | "dv" | "dolby" | "vision"
            | "proper" | "repack" | "internal" | "limited" | "imax" | "oar" | "ws"
            | "multi" | "dual" | "dubbed" | "subbed" | "subs" | "vostfr"
            | "amzn" | "nf" | "hulu" | "dsnp" | "atvp" | "hmax" | "aptv" | "itunes"
            | "yts" | "yify" | "rarbg" | "eztv" | "ettv" | "galaxyrg" | "evo" | "fgt"
            | "complete" | "pack" | "collection"
            | "english" | "french" | "german" | "spanish" | "italian" | "japanese"
            | "korean" | "portuguese" | "russian"
    )
}

/// A token is junk when it is junk whole, or when every dash-glued
/// part of it is junk ("web-dl" → web+dl, "x264-evo" → x264+evo).
fn token_is_junk(tok: &str) -> bool {
    if is_junk(tok) {
        return true;
    }
    if tok.contains('-') {
        let parts: Vec<&str> = tok.split('-').filter(|p| !p.is_empty()).collect();
        return !parts.is_empty() && parts.iter().all(|p| is_junk(p));
    }
    false
}

/// A split-file part starting at token `k`: ("cd1", 1) for a glued
/// stack token, ("pt2", 2) for the pair "pt" "2". `word_part` gates
/// the ambiguous spelled word "part" (sequel titles!) — callers pass
/// false in title position, true after the year / episode marker.
fn part_at(lower: &[String], k: usize, word_part: bool) -> Option<(String, usize)> {
    if k >= lower.len() {
        return None;
    }
    let lt = lower[k].trim_matches('-');
    for w in ["cd", "disc", "disk", "dvd", "pt"] {
        if let Some(rest) = lt.strip_prefix(w) {
            if !rest.is_empty()
                && rest.len() <= 2
                && rest.chars().all(|c| c.is_ascii_digit())
            {
                return Some((format!("{w}{rest}"), 1));
            }
        }
        if lt == w {
            if k + 1 < lower.len() {
                let nt = lower[k + 1].trim_matches('-');
                if !nt.is_empty()
                    && nt.len() <= 2
                    && nt.chars().all(|c| c.is_ascii_digit())
                {
                    return Some((format!("{w}{nt}"), 2));
                }
            }
        }
    }
    if word_part && lt == "part" && k + 1 < lower.len() {
        let nt = lower[k + 1].trim_matches('-');
        if !nt.is_empty() && nt.len() <= 2 && nt.chars().all(|c| c.is_ascii_digit()) {
            return Some((format!("part{nt}"), 2));
        }
    }
    None
}

/// Word-numbers that show up in sequel titles, for scoring parity
/// ("Part Two" == "Part 2", "Chapter Three" == "Chapter 3").
fn word_number(tok: &str) -> Option<u32> {
    match tok {
        "one" => Some(1),
        "two" => Some(2),
        "three" => Some(3),
        "four" => Some(4),
        "five" => Some(5),
        "six" => Some(6),
        "seven" => Some(7),
        "eight" => Some(8),
        "nine" => Some(9),
        "ten" => Some(10),
        "eleven" => Some(11),
        "twelve" => Some(12),
        "ii" => Some(2),
        "iii" => Some(3),
        "iv" => Some(4),
        "vi" => Some(6),
        "vii" => Some(7),
        "viii" => Some(8),
        "ix" => Some(9),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// The parser
// ---------------------------------------------------------------------------

/// Parse one media file name (with or without extension) into
/// structured facts. Deterministic; the same name always parses the
/// same way.
pub fn parse_video_filename(file_name: &str) -> ParsedVideo {
    let mut out = ParsedVideo::default();
    let mut tags: Vec<String> = Vec::new();

    // --- strip extension -------------------------------------------------
    let stem = strip_extension(file_name);

    // --- explicit id / edition brace + bracket tags ----------------------
    let mut work = stem.to_string();
    for (open, close) in [('{', '}'), ('[', ']')] {
        let mut cleaned = String::with_capacity(work.len());
        let chars: Vec<char> = work.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == open {
                if let Some(end) = chars[i..].iter().position(|&c| c == close) {
                    let inner: String = chars[i + 1..i + end].iter().collect();
                    let low = inner.to_lowercase();
                    let mut consumed = false;
                    if let Some(id) = low
                        .strip_prefix("tmdb-")
                        .or_else(|| low.strip_prefix("tmdbid="))
                        .or_else(|| low.strip_prefix("tmdbid-"))
                    {
                        if let Ok(n) = id.trim().parse::<i64>() {
                            out.explicit_tmdb_id = Some(n);
                            consumed = true;
                        }
                    } else if let Some(id) = low
                        .strip_prefix("imdb-")
                        .or_else(|| low.strip_prefix("imdbid="))
                    {
                        out.explicit_imdb_id = Some(id.trim().to_string());
                        consumed = true;
                    } else if let Some(ed) = inner.strip_prefix("edition-") {
                        out.edition = Some(ed.trim().to_string());
                        consumed = true;
                    }
                    if consumed {
                        i += end + 1;
                        continue;
                    }
                    // A bracketed year ([2018]) is a year claim.
                    if let Some(y) = is_year_token(inner.trim()) {
                        if out.year.is_none() {
                            out.year = Some(y);
                        }
                        i += end + 1;
                        continue;
                    }
                    // Plain bracket group ([YTS], [1080p]): its content
                    // is release metadata, never title. Keep as a tag.
                    tags.push(low.trim().to_string());
                    i += end + 1;
                    continue;
                }
            }
            cleaned.push(chars[i]);
            i += 1;
        }
        work = cleaned;
    }

    // --- parenthesized year gets first claim ------------------------------
    let mut paren_year: Option<u32> = None;
    {
        let chars: Vec<char> = work.chars().collect();
        let mut cleaned = String::with_capacity(work.len());
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '(' {
                if let Some(end) = chars[i..].iter().position(|&c| c == ')') {
                    let inner: String = chars[i + 1..i + end].iter().collect();
                    if let Some(y) = is_year_token(inner.trim()) {
                        if paren_year.is_none() {
                            paren_year = Some(y);
                        }
                        i += end + 1;
                        continue;
                    }
                }
            }
            cleaned.push(chars[i]);
            i += 1;
        }
        work = cleaned;
    }

    // --- tokenize ----------------------------------------------------------
    let normalized = work.replace(['.', '_', '(', ')', '[', ']', '{', '}'], " ");
    let raw_tokens: Vec<String> = normalized
        .split_whitespace()
        .map(|t| t.to_string())
        .collect();
    let lower: Vec<String> = raw_tokens.iter().map(|t| t.to_lowercase()).collect();
    let n = raw_tokens.len();

    // --- episode markers ---------------------------------------------------
    // (marker_start, marker_end_exclusive, season, first, last)
    let mut marker: Option<(usize, usize, u32, u32, u32)> = None;
    let mut i = 0;
    while i < n && marker.is_none() {
        let t = &lower[i];
        // S01E01 / S01E01E02 / S1E3 — one token, or "S01 E01" pair.
        if let Some((s, e1)) = parse_se_token(t) {
            let mut last = e1;
            let mut end = i + 1;
            // chained: S01E01E02 packed in the same token
            if let Some(e2) = parse_second_e(t, s) {
                last = e2;
            }
            // chained across tokens: S01E01-E02, S01E01-02, S01E01 E02,
            // and longer runs (S01E01-E02-E03) — keep consuming while
            // the next token is an episode continuation.
            let mut j = end;
            loop {
                if j >= n {
                    break;
                }
                let raw = &lower[j];
                let nt = raw.trim_start_matches('-');
                if raw == "-" {
                    j += 1;
                    continue;
                }
                let dashed = nt != raw.as_str();
                if let Some(e2) = nt
                    .strip_prefix('e')
                    .filter(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
                    .and_then(|rest| rest.parse::<u32>().ok())
                {
                    last = e2;
                    end = j + 1;
                    j += 1;
                    continue;
                }
                if dashed && !nt.is_empty() && nt.chars().all(|c| c.is_ascii_digit()) {
                    if let Ok(e2) = nt.parse::<u32>() {
                        last = e2;
                        end = j + 1;
                        j += 1;
                        continue;
                    }
                }
                break;
            }
            marker = Some((i, end, s, e1, last));
            break;
        }
        // 1x03 form.
        if let Some((s, e)) = parse_nxn_token(t) {
            marker = Some((i, i + 1, s, e, e));
            break;
        }
        // "Season 2 Episode 5" word form.
        if t == "season" && i + 3 < n && lower[i + 2] == "episode" {
            if let (Ok(s), Ok(e)) = (
                lower[i + 1].parse::<u32>(),
                lower[i + 3].parse::<u32>(),
            ) {
                marker = Some((i, i + 4, s, e, e));
                break;
            }
        }
        i += 1;
    }

    let (head_end, tail_start) = match marker {
        Some((start, end, s, e1, e2)) => {
            out.kind = ParsedKind::Episode;
            out.season = Some(s);
            out.episode_first = Some(e1);
            out.episode_last = Some(e2);
            (start, end)
        }
        None => (n, n),
    };

    // --- date-based marker (only when no S/E marker) -----------------------
    if marker.is_none() {
        let mut k = 0;
        while k + 2 < n {
            if let Some(y) = is_year_token(&lower[k]) {
                if let (Ok(m), Ok(d)) = (
                    lower[k + 1].parse::<u32>(),
                    lower[k + 2].parse::<u32>(),
                ) {
                    if (1..=12).contains(&m) && (1..=31).contains(&d) {
                        out.kind = ParsedKind::Episode;
                        out.air_date = Some(format!("{y:04}-{m:02}-{d:02}"));
                        out.flags.push("date_based".into());
                        break;
                    }
                }
            }
            k += 1;
        }
    }
    let date_head_end = if out.air_date.is_some() {
        // title is whatever precedes the date
        let mut k = 0;
        while k + 2 < n {
            if is_year_token(&lower[k]).is_some() {
                break;
            }
            k += 1;
        }
        k
    } else {
        head_end
    };

    // --- year + title from the head ----------------------------------------
    let head_limit = if marker.is_some() { head_end } else { date_head_end };
    let mut year_idx: Option<usize> = None;
    if paren_year.is_some() {
        out.year = paren_year;
    } else {
        // Last plausible year token in the head that is not the only
        // token (a lone "2012" is a title, not a year).
        for k in (0..head_limit).rev() {
            if let Some(y) = is_year_token(&lower[k]) {
                if head_limit > 1 {
                    out.year = Some(y);
                    year_idx = Some(k);
                }
                break;
            }
        }
    }
    let title_end = year_idx.unwrap_or(head_limit);
    let mut title_words: Vec<String> = Vec::new();
    let mut title_part: Option<String> = None;
    for k in 0..title_end {
        let lt = &lower[k];
        // A stack-word part token (CD1, pt2…) ends the title — the
        // spelled word "part" deliberately does NOT (sequel titles).
        if let Some((label, _)) = part_at(&lower, k, false) {
            if lt != "part" {
                title_part = Some(label);
                break;
            }
        }
        if token_is_junk(lt) {
            tags.push(lt.clone());
            continue;
        }
        // trailing release group glued with a dash ("Title-EVO")
        if k == title_end - 1 {
            if let Some((head_part, grp)) = raw_tokens[k].rsplit_once('-') {
                if is_junk(&grp.to_lowercase()) && !head_part.is_empty() {
                    tags.push(grp.to_lowercase());
                    title_words.push(head_part.to_string());
                    continue;
                }
            }
        }
        title_words.push(raw_tokens[k].clone());
    }
    if title_part.is_some() {
        out.part = title_part.take();
    }
    // Trim junk off the title's edges (leading group tags etc.).
    while title_words.len() > 1
        && (token_is_junk(&title_words[0].to_lowercase()) || title_words[0] == "-")
    {
        let dropped = title_words.remove(0);
        tags.push(dropped.to_lowercase());
    }
    while title_words.len() > 1
        && (token_is_junk(&title_words[title_words.len() - 1].to_lowercase())
            || title_words[title_words.len() - 1] == "-")
    {
        let dropped = title_words.pop().unwrap();
        tags.push(dropped.to_lowercase());
    }
    out.title = title_words.join(" ").trim_matches(['-', ' ']).to_string();

    // --- year may also live in the tail (movies: "Title 1080p 2024") -------
    // (Not for date-based episodes: their year token is the air date,
    // not the show's year — using it as a year filter would mislead.)
    if out.year.is_none() && marker.is_none() && out.air_date.is_none() {
        for k in (0..n).rev() {
            if let Some(y) = is_year_token(&lower[k]) {
                out.year = Some(y);
                break;
            }
        }
    }

    // --- part detection -----------------------------------------------------
    // Stack words anywhere after the title; the word "part" counts
    // only in the marker tail or after a bare year token (after a
    // PAREN year the title loop already caught stack words, and a
    // spelled "Part N" there is a sequel, per the title-loop rule).
    if out.part.is_none() {
        let scan_from = if marker.is_some() {
            tail_start
        } else if let Some(y) = year_idx {
            y + 1
        } else {
            n
        };
        let mut k = scan_from;
        while k < n {
            if let Some((label, _)) = part_at(&lower, k, true) {
                out.part = Some(label);
                break;
            }
            k += 1;
        }
    }

    // --- episode title hint (tokens after the marker, junk-stripped) -------
    if marker.is_some() {
        let mut hint_words: Vec<String> = Vec::new();
        let mut k = tail_start;
        while k < n {
            if let Some((_, consumed)) = part_at(&lower, k, true) {
                k += consumed;
                continue;
            }
            let lt = &lower[k];
            if token_is_junk(lt) {
                tags.push(lt.clone());
                k += 1;
                continue;
            }
            // single stray letters are codec fragments ("H" of H.264)
            if raw_tokens[k].len() == 1 && raw_tokens[k].chars().all(|c| c.is_ascii_alphabetic()) {
                k += 1;
                continue;
            }
            hint_words.push(raw_tokens[k].clone());
            k += 1;
        }
        let hint = hint_words.join(" ").trim_matches(['-', ' ']).to_string();
        if !hint.is_empty() && hint.len() > 1 {
            out.episode_title_hint = Some(hint);
        }
    }

    // --- absolute numbering (anime), only when nothing else matched --------
    if marker.is_none() && out.air_date.is_none() {
        // "Show - 123", "Show [123]", "Show EP123", "Show E123"
        let mut abs: Option<u32> = None;
        for k in 0..n {
            let lt = &lower[k];
            if lt == "-" && k + 1 < n {
                let nt = &lower[k + 1];
                if nt.len() >= 2
                    && nt.chars().all(|c| c.is_ascii_digit())
                    && is_year_token(nt).is_none()
                {
                    if let Ok(v) = nt.parse::<u32>() {
                        // a bare resolution ("Show - 1080") is not
                        // an episode number
                        if !matches!(v, 480 | 720 | 1080 | 1440 | 2160 | 4320) {
                            abs = Some(v);
                        }
                    }
                }
            }
            if let Some(rest) = lt.strip_prefix("ep").or_else(|| lt.strip_prefix('e')) {
                if rest.len() >= 2
                    && !rest.is_empty()
                    && rest.chars().all(|c| c.is_ascii_digit())
                    && k > 0
                {
                    if let Ok(v) = rest.parse::<u32>() {
                        abs = Some(v);
                    }
                }
            }
        }
        if let Some(v) = abs {
            if v >= 1 && !out.title.is_empty() {
                out.absolute_episode = Some(v);
                out.kind = ParsedKind::Episode;
                out.flags.push("absolute_numbering".into());
                // The number was scooped into the title in the dash
                // form ("Show - 042"): pop the trailing number (and
                // a dangling dash) back off the title.
                let mut words: Vec<String> =
                    out.title.split(' ').map(|s| s.to_string()).collect();
                if let Some(last) = words.last() {
                    let lw = last.to_lowercase();
                    let bare = last.parse::<u32>().ok() == Some(v);
                    let ep_form = ["ep", "e"].iter().any(|pre| {
                        lw.strip_prefix(pre)
                            .and_then(|r| r.parse::<u32>().ok())
                            == Some(v)
                    });
                    if bare || ep_form {
                        words.pop();
                    }
                }
                if words.last().map(|w| w.as_str()) == Some("-") {
                    words.pop();
                }
                out.title = words.join(" ").trim().to_string();
            }
        }
    }

    // --- edition phrases ------------------------------------------------------
    // A phrase counts as an edition only when it sits in release
    // position — AFTER the year in the original name (or, with no
    // year, at least not as the whole title). "The Final Cut (2004)"
    // is a title; "Blade Runner (1982) Final Cut" is an edition.
    if out.edition.is_none() {
        let norm_stem = stem
            .to_lowercase()
            .replace(['.', '_'], " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let year_pos = out
            .year
            .and_then(|y| norm_stem.find(&y.to_string()));
        for (phrase, label) in [
            ("directors cut", "Director's Cut"),
            ("director's cut", "Director's Cut"),
            ("final cut", "Final Cut"),
            ("extended", "Extended"),
            ("unrated", "Unrated"),
            ("theatrical", "Theatrical"),
        ] {
            let Some(ppos) = norm_stem.find(phrase) else {
                continue;
            };
            let in_release_position = match year_pos {
                Some(yp) => ppos > yp,
                None => ppos > 0,
            };
            if !in_release_position {
                continue;
            }
            out.edition = Some(label.to_string());
            // A paren-year title zone can swallow the phrase into
            // the title ("Alien (1979) Directors Cut") — hand the
            // words back to the edition when they form its tail.
            let phrase_words: Vec<&str> = phrase.split(' ').collect();
            let mut words: Vec<String> =
                out.title.split(' ').map(|s| s.to_string()).collect();
            if words.len() > phrase_words.len() {
                let tail: Vec<String> = words[words.len() - phrase_words.len()..]
                    .iter()
                    .map(|w| w.to_lowercase())
                    .collect();
                if tail == phrase_words {
                    words.truncate(words.len() - phrase_words.len());
                    out.title = words.join(" ").trim().to_string();
                }
            }
            break;
        }
    }

    if out.kind == ParsedKind::Unknown && !out.title.is_empty() {
        out.kind = ParsedKind::Movie;
    }
    if out.title.is_empty() {
        out.flags.push("no_title".into());
    }
    if out.year.is_none() {
        out.flags.push("no_year".into());
    }
    out.tags = tags;
    out
}

fn strip_extension(name: &str) -> &str {
    if let Some(dot) = name.rfind('.') {
        let ext = &name[dot + 1..];
        if (2..=4).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()) {
            return &name[..dot];
        }
    }
    name
}

/// "s01e01" / "s1e3" token → (season, first episode). Also accepts the
/// packed "s01e01e02" via parse_second_e.
fn parse_se_token(tok: &str) -> Option<(u32, u32)> {
    let t = tok.strip_prefix('s')?;
    let epos = t.find('e')?;
    let (s, rest) = t.split_at(epos);
    if s.is_empty() || s.len() > 2 || !s.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let ep = &rest[1..];
    let digits: String = ep.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || digits.len() > 3 {
        return None;
    }
    Some((s.parse().ok()?, digits.parse().ok()?))
}

/// Last episode packed into the same token: "s01e01e02" → 2,
/// "s01e01-e02-e03" → 3. Splits the post-season remainder into its
/// digit groups and takes the last one.
fn parse_second_e(tok: &str, _season: u32) -> Option<u32> {
    let t = tok.strip_prefix('s')?;
    let first_e = t.find('e')?;
    let rest = &t[first_e + 1..];
    let groups: Vec<&str> = rest
        .split(|c| c == 'e' || c == '-')
        .filter(|g| !g.is_empty() && g.chars().all(|c| c.is_ascii_digit()))
        .collect();
    if groups.len() >= 2 {
        groups[groups.len() - 1].parse().ok()
    } else {
        None
    }
}

/// "1x03" / "12x104" token → (season, episode).
fn parse_nxn_token(tok: &str) -> Option<(u32, u32)> {
    let (a, b) = tok.split_once('x')?;
    if a.is_empty() || a.len() > 2 || !a.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if b.is_empty() || b.len() > 3 || !b.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((a.parse().ok()?, b.parse().ok()?))
}

/// Folder context: a `Season NN` / `Specials` parent folder supplies
/// the season when the file name didn't, and confirms it when it did.
/// A show-folder name can also supply the year ("Show (2019)").
pub fn apply_folder_context(p: &mut ParsedVideo, parent_dir: &str) {
    let low = parent_dir.to_lowercase();
    let mut season: Option<u32> = None;
    if low.trim() == "specials" {
        season = Some(0);
    } else {
        let toks: Vec<&str> = low.split(|c: char| !c.is_alphanumeric()).collect();
        for w in toks.windows(2) {
            if w[0] == "season" {
                if let Ok(v) = w[1].parse::<u32>() {
                    season = Some(v);
                }
            }
        }
        if season.is_none() {
            if let Some((s, _)) = parse_se_token(&low.replace(' ', "")) {
                season = Some(s);
            }
        }
    }
    if let Some(s) = season {
        if p.season.is_none() && p.episode_first.is_some() {
            p.season = Some(s);
        }
        // A bare "E05" inside a Season folder is an episode of that
        // season, not an absolute number — the folder is the evidence
        // that disambiguates. (A season-less absolute number stays
        // absolute and flagged.)
        if p.season.is_none() && p.episode_first.is_none() {
            if let Some(abs) = p.absolute_episode.take() {
                p.season = Some(s);
                p.episode_first = Some(abs);
                p.episode_last = Some(abs);
                p.flags.retain(|f| f != "absolute_numbering");
                p.flags.push("absolute_in_season_folder".into());
            }
        }
    }
    if p.year.is_none() {
        // "(2019)" in the folder name
        if let Some(open) = parent_dir.rfind('(') {
            if let Some(close) = parent_dir[open..].find(')') {
                let inner = &parent_dir[open + 1..open + close];
                if let Some(y) = is_year_token(inner) {
                    p.year = Some(y);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Canonical server naming (Plex form; Emby/Jellyfin accept the same)
// ---------------------------------------------------------------------------

/// Strip characters no server or filesystem wants in a name component.
pub fn sanitize_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => out.push(' '),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn with_year(title: &str, year: Option<u32>) -> String {
    match year {
        Some(y) => format!("{} ({})", sanitize_component(title), y),
        None => sanitize_component(title),
    }
}

pub fn movie_dir_name(title: &str, year: Option<u32>) -> String {
    with_year(title, year)
}

/// File stem for a movie: `Title (Year)`, plus ` {edition-…}` when an
/// edition is known, plus ` - pt1` for split parts (Plex stack form).
pub fn movie_file_stem(
    title: &str,
    year: Option<u32>,
    edition: Option<&str>,
    part: Option<&str>,
) -> String {
    let mut s = with_year(title, year);
    if let Some(ed) = edition {
        s.push_str(&format!(" {{edition-{}}}", sanitize_component(ed)));
    }
    if let Some(p) = part {
        s.push_str(&format!(" - {p}"));
    }
    s
}

pub fn show_dir_name(title: &str, year: Option<u32>) -> String {
    with_year(title, year)
}

pub fn season_dir_name(season: u32) -> String {
    format!("Season {season:02}")
}

/// File stem for an episode: `Show (Year) - S01E02`, multi-episode
/// `Show (Year) - S01E01-E02`, with ` - Episode Title` appended when
/// known (both titles for a two-parter when they fit).
pub fn episode_file_stem(
    show: &str,
    year: Option<u32>,
    season: u32,
    episodes: &[u32],
    episode_titles: &[String],
) -> String {
    let base = with_year(show, year);
    let marker = match episodes {
        [] => format!("S{season:02}"),
        [only] => format!("S{season:02}E{only:02}"),
        [first, ..] => {
            let last = episodes[episodes.len() - 1];
            format!("S{season:02}E{first:02}-E{last:02}")
        }
    };
    let mut s = format!("{base} - {marker}");
    let titles: Vec<String> = episode_titles
        .iter()
        .map(|t| sanitize_component(t))
        .filter(|t| !t.is_empty())
        .collect();
    if !titles.is_empty() {
        let joined = titles.join(" - ");
        if s.len() + 3 + joined.len() <= 140 {
            s.push_str(" - ");
            s.push_str(&joined);
        }
    }
    s
}

/// Relative target path for a movie under its library root.
pub fn movie_target_rel(
    title: &str,
    year: Option<u32>,
    edition: Option<&str>,
    part: Option<&str>,
    ext: &str,
) -> PathBuf {
    let dir = movie_dir_name(title, year);
    let stem = movie_file_stem(title, year, edition, part);
    PathBuf::from(dir).join(format!("{stem}.{ext}"))
}

/// Relative target path for an episode under its library root.
pub fn episode_target_rel(
    show: &str,
    year: Option<u32>,
    season: u32,
    episodes: &[u32],
    episode_titles: &[String],
    ext: &str,
) -> PathBuf {
    let dir = show_dir_name(show, year);
    let stem = episode_file_stem(show, year, season, episodes, episode_titles);
    PathBuf::from(dir)
        .join(season_dir_name(season))
        .join(format!("{stem}.{ext}"))
}

// ---------------------------------------------------------------------------
// TMDB candidate scoring (pure; the client lives in src-tauri)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmdbCandidate {
    pub id: i64,
    /// "movie" | "tv"
    pub media_type: String,
    pub title: String,
    pub original_title: String,
    pub year: Option<u32>,
    pub popularity: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmdbEpisode {
    pub episode_number: u32,
    pub name: String,
    pub air_date: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateScore {
    pub candidate: TmdbCandidate,
    pub score: f32,
    pub tier: Tier,
    pub reasons: Vec<String>,
}

/// Normalize a title for comparison: lowercase, punctuation out,
/// word-numbers folded to digits, articles dropped.
pub fn normalize_title(s: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    for tok in cleaned.to_lowercase().split_whitespace() {
        if matches!(tok, "the" | "a" | "an") {
            continue;
        }
        if let Some(n) = word_number(tok) {
            out.push(n.to_string());
        } else if let Ok(v) = tok.parse::<u32>() {
            out.push(v.to_string());
        } else {
            out.push(tok.to_string());
        }
    }
    out.join(" ")
}

/// Title similarity in 0..=1: exact-normalized = 1; otherwise token
/// Dice with a containment bonus (a parse that dropped a subtitle
/// still scores its parent title well).
pub fn title_similarity(a: &str, b: &str) -> f32 {
    let na = normalize_title(a);
    let nb = normalize_title(b);
    if na.is_empty() || nb.is_empty() {
        return 0.0;
    }
    if na == nb {
        return 1.0;
    }
    let ta: Vec<&str> = na.split(' ').collect();
    let tb: Vec<&str> = nb.split(' ').collect();
    let set_a: std::collections::HashSet<&str> = ta.iter().copied().collect();
    let set_b: std::collections::HashSet<&str> = tb.iter().copied().collect();
    let inter = set_a.intersection(&set_b).count() as f32;
    let dice = 2.0 * inter / (set_a.len() + set_b.len()) as f32;
    let containment = if na.contains(&nb) || nb.contains(&na) {
        0.92
    } else {
        0.0
    };
    dice.max(containment)
}

/// The tier thresholds, shared with the app layer: ≥0.85 Confident,
/// ≥0.55 Probable, below that Unknown.
pub fn tier_for(score: f32) -> Tier {
    if score >= 0.85 {
        Tier::Confident
    } else if score >= 0.55 {
        Tier::Probable
    } else {
        Tier::Unknown
    }
}

/// Score one TMDB candidate against a parse. Evidence strings are
/// user-facing (the resolution view shows WHY), so they stay plain.
pub fn score_candidate(parsed: &ParsedVideo, cand: &TmdbCandidate) -> CandidateScore {
    // An explicit {tmdb-…} tag in the file name is the user (or a
    // previous tool) asserting identity — decisive when it matches.
    if let Some(id) = parsed.explicit_tmdb_id {
        if id == cand.id {
            return CandidateScore {
                candidate: cand.clone(),
                score: 1.0,
                tier: Tier::Confident,
                reasons: vec!["file name carries this TMDB id".into()],
            };
        }
    }
    let mut reasons: Vec<String> = Vec::new();
    let sim_main = title_similarity(&parsed.title, &cand.title);
    let sim_orig = if cand.original_title != cand.title {
        title_similarity(&parsed.title, &cand.original_title)
    } else {
        0.0
    };
    let sim = sim_main.max(sim_orig);
    if sim >= 0.99 {
        reasons.push("title matches exactly".into());
    } else if sim >= 0.7 {
        reasons.push("title is a close match".into());
    } else {
        reasons.push("title differs".into());
    }
    let mut score = sim * 0.75;
    match (parsed.year, cand.year) {
        (Some(py), Some(cy)) if py == cy => {
            score += 0.25;
            reasons.push(format!("year matches ({cy})"));
        }
        (Some(py), Some(cy)) if (py as i64 - cy as i64).abs() == 1 => {
            score += 0.08;
            reasons.push(format!("year is one off ({py} vs {cy})"));
        }
        (Some(py), Some(cy)) => {
            score -= 0.30;
            reasons.push(format!("year differs ({py} vs {cy})"));
        }
        (None, Some(cy)) => {
            score += 0.03;
            reasons.push(format!("no year in the file name; TMDB says {cy}"));
        }
        _ => {}
    }
    // Popularity is only a whisper of a tiebreak, never evidence.
    score += (cand.popularity.min(100.0) as f32) * 0.0002;
    let score = score.clamp(0.0, 1.0);
    CandidateScore {
        candidate: cand.clone(),
        score,
        tier: tier_for(score),
        reasons,
    }
}

/// Rank candidates best-first (stable for equal scores).
pub fn rank_candidates(
    parsed: &ParsedVideo,
    cands: &[TmdbCandidate],
) -> Vec<CandidateScore> {
    let mut scored: Vec<CandidateScore> =
        cands.iter().map(|c| score_candidate(parsed, c)).collect();
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    scored
}

/// Verify a parsed episode against TMDB's episode list for the
/// season: returns a score adjustment and a plain-language note.
/// Out-of-range episode numbers cap the match at Probable — the file
/// may belong to a different season ordering, and Fable says so
/// instead of guessing.
pub fn verify_episode(
    parsed: &ParsedVideo,
    episodes: &[TmdbEpisode],
) -> (f32, Option<String>) {
    let Some(first) = parsed.episode_first else {
        return (0.0, None);
    };
    let found = episodes.iter().find(|e| e.episode_number == first);
    match found {
        None => (
            -0.25,
            Some(format!(
                "TMDB lists {} episodes this season; this file claims E{first:02}",
                episodes.len()
            )),
        ),
        Some(ep) => {
            let mut bonus = 0.05;
            let mut note = format!("episode E{first:02} exists on TMDB");
            if let Some(hint) = &parsed.episode_title_hint {
                let sim = title_similarity(hint, &ep.name);
                if sim >= 0.7 {
                    bonus += 0.05;
                    note = format!("episode title matches TMDB (\"{}\")", ep.name);
                }
            }
            (bonus, Some(note))
        }
    }
}

#[cfg(test)]
mod tests;
