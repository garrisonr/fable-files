//! Sentence segmentation for read-aloud (Ebook P3).
//!
//! One implementation, two consumers that must never disagree: the
//! playback engine (which synthesizes unit by unit) and the reader's
//! highlighter (which lights the unit being spoken). Both take the
//! ranges from here, computed over the exact chapter text the
//! reader's `ChapterView` carries.
//!
//! Ranges are CHAR indices into the chapter text, `[start, end)`,
//! trimmed of surrounding whitespace. Rules, in order:
//!  - A paragraph break (blank line) always ends a unit, punctuated
//!    or not — headings and verse lines are their own units.
//!  - Terminal punctuation (. ! ? …) ends a unit when followed by
//!    whitespace and an opener (capital, digit, quote, bracket,
//!    dash), or by the end of the text. Closing quotes/brackets
//!    after the punctuation belong to the unit they close.
//!  - Decimals (3.5), initials (J. R. R.), and a shield list of
//!    abbreviations (Dr., Mr., e.g., U.S., …) never end a unit.
//!  - A unit longer than [`MAX_UNIT_CHARS`] is split at the last
//!    clause boundary (semicolon, dash, colon, comma) before the
//!    cap, else at the last space — narration engines slow down on
//!    very long inputs, and the split is part of the unit system
//!    everywhere, so highlight and voice stay in step.
//!  - Units with no letter or digit at all (a lone "***" scene
//!    break) are dropped: there is nothing to say.

/// Char index range of one narration unit in the chapter text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SentenceRange {
    pub start: usize,
    pub end: usize,
}

/// Longest narration unit before a clause split (chars).
pub const MAX_UNIT_CHARS: usize = 240;
/// Shortest piece a long-unit split may leave on either side.
const MIN_SPLIT_SIDE: usize = 60;

const SHIELD: &[&str] = &[
    "mr", "mrs", "ms", "dr", "st", "prof", "sr", "jr", "vs", "etc", "e.g", "i.e", "a.m", "p.m",
    "no", "vol", "ch", "fig", "approx", "dept", "est", "u.s", "u.k", "d.c", "ph.d", "m.d", "b.c",
    "a.d", "gen", "sen", "rep", "gov", "lt", "col", "sgt", "capt", "rev", "hon", "pres", "jan",
    "feb", "mar", "apr", "jun", "jul", "aug", "sep", "sept", "oct", "nov", "dec",
];

fn is_closer(c: char) -> bool {
    matches!(c, '"' | '\'' | '\u{201D}' | '\u{2019}' | ')' | ']' | '}' | '\u{00BB}')
}

fn is_opener(c: char) -> bool {
    c.is_uppercase()
        || c.is_ascii_digit()
        || matches!(
            c,
            '"' | '\u{201C}' | '\u{2018}' | '(' | '[' | '{' | '\u{2014}' | '\u{2013}' | '*'
        )
}

/// The word (letters and dots) ending at char index `i`, lowercased,
/// with any trailing dot stripped — the shield lookup key.
fn word_ending_at(chars: &[char], i: usize) -> String {
    let mut j = i + 1;
    while j > 0 && (chars[j - 1].is_alphabetic() || chars[j - 1] == '.') {
        j -= 1;
    }
    let w: String = chars[j..=i].iter().collect::<String>().to_lowercase();
    w.trim_end_matches('.').to_string()
}

/// True when a paragraph break (a blank line) starts at or before
/// char index `i` — i.e. the whitespace run containing `i` holds
/// two newlines.
fn paragraph_break_in(chars: &[char], from: usize, to: usize) -> bool {
    let mut newlines = 0;
    for c in &chars[from..=to.min(chars.len().saturating_sub(1))] {
        if *c == '\n' {
            newlines += 1;
            if newlines >= 2 {
                return true;
            }
        } else if !c.is_whitespace() {
            return false;
        }
    }
    newlines >= 2
}

/// Split one over-long unit at clause boundaries: the last clause
/// mark (semicolon, colon, dash, comma) inside the window wins; the
/// last space is the fallback; a hard cut at the cap is the last
/// resort.
fn split_long(chars: &[char], start: usize, end: usize, out: &mut Vec<SentenceRange>) {
    let mut s = start;
    while end - s > MAX_UNIT_CHARS {
        let cap = s + MAX_UNIT_CHARS;
        let mut clause_cut: Option<usize> = None;
        let mut space_cut: Option<usize> = None;
        let mut i = cap;
        while i > s + MIN_SPLIT_SIDE {
            let c = chars[i - 1];
            if (c == ';' || c == ':' || c == '\u{2014}' || c == ',')
                && i < chars.len()
                && chars[i].is_whitespace()
            {
                clause_cut = Some(i);
                break;
            }
            if c == ' ' && space_cut.is_none() {
                space_cut = Some(i);
            }
            i -= 1;
        }
        let cut = clause_cut.or(space_cut).unwrap_or(cap);
        push_unit(chars, s, cut, out);
        s = cut;
        while s < end && chars[s].is_whitespace() {
            s += 1;
        }
    }
    push_unit(chars, s, end, out);
}

fn push_unit(chars: &[char], start: usize, end: usize, out: &mut Vec<SentenceRange>) {
    let mut s = start;
    let mut e = end;
    while s < e && chars[s].is_whitespace() {
        s += 1;
    }
    while e > s && chars[e - 1].is_whitespace() {
        e -= 1;
    }
    if s >= e {
        return;
    }
    // Nothing to say (scene-break ornaments etc.) — no unit.
    if !chars[s..e].iter().any(|c| c.is_alphanumeric()) {
        return;
    }
    out.push(SentenceRange { start: s, end: e });
}

/// Segment a chapter's text into narration units.
pub fn segment_chapter(text: &str) -> Vec<SentenceRange> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut out: Vec<SentenceRange> = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < n {
        let c = chars[i];
        if c == '\n' {
            // Paragraph break ends the unit here (at the last
            // non-whitespace char before the break).
            let mut j = i;
            let mut newlines = 0;
            while j < n && (chars[j] == '\n' || chars[j] == ' ' || chars[j] == '\t' || chars[j] == '\r')
            {
                if chars[j] == '\n' {
                    newlines += 1;
                }
                j += 1;
            }
            if newlines >= 2 {
                if i > start {
                    // Split the accumulated unit if it grew too long.
                    let (s0, e0) = (start, i);
                    if e0 - s0 > MAX_UNIT_CHARS {
                        split_long(&chars, s0, e0, &mut out);
                    } else {
                        push_unit(&chars, s0, e0, &mut out);
                    }
                }
                start = j;
                i = j;
                continue;
            }
            i += 1;
            continue;
        }
        if c == '.' || c == '!' || c == '?' || c == '\u{2026}' {
            // Decimal point between digits: not a boundary.
            if c == '.'
                && i > 0
                && i + 1 < n
                && chars[i - 1].is_ascii_digit()
                && chars[i + 1].is_ascii_digit()
            {
                i += 1;
                continue;
            }
            // A run of dots (ellipsis typed out) is one mark.
            let mut punct_end = i;
            if c == '.' {
                while punct_end + 1 < n && chars[punct_end + 1] == '.' {
                    punct_end += 1;
                }
            }
            // Shielded abbreviation / initial: not a boundary.
            if c == '.' {
                let word = word_ending_at(&chars, punct_end);
                if SHIELD.contains(&word.as_str()) {
                    i = punct_end + 1;
                    continue;
                }
                // Single capital initial: "J." followed by a name.
                if punct_end >= 1 {
                    let prev = chars[punct_end - 1];
                    let before_ok = punct_end < 2
                        || chars[punct_end - 2].is_whitespace()
                        || chars[punct_end - 2] == '.';
                    if prev.is_uppercase() && prev.is_alphabetic() && before_ok {
                        i = punct_end + 1;
                        continue;
                    }
                }
            }
            // Consume closing quotes/brackets after the mark.
            let mut k = punct_end + 1;
            while k < n && is_closer(chars[k]) {
                k += 1;
            }
            // Boundary iff the rest is whitespace then an opener,
            // or the text ends.
            let mut m = k;
            let mut saw_ws = false;
            while m < n && chars[m].is_whitespace() {
                saw_ws = true;
                m += 1;
            }
            let boundary = m >= n || (saw_ws && (is_opener(chars[m]) || paragraph_break_in(&chars, k, m)));
            if boundary {
                let (s0, e0) = (start, k);
                if e0 - s0 > MAX_UNIT_CHARS {
                    split_long(&chars, s0, e0, &mut out);
                } else {
                    push_unit(&chars, s0, e0, &mut out);
                }
                start = m;
                i = m;
                continue;
            }
            i = punct_end + 1;
            continue;
        }
        i += 1;
    }
    if start < n {
        let (s0, e0) = (start, n);
        if e0 - s0 > MAX_UNIT_CHARS {
            split_long(&chars, s0, e0, &mut out);
        } else {
            push_unit(&chars, s0, e0, &mut out);
        }
    }
    out
}

/// The text of one unit, as synthesized (the exact slice).
pub fn unit_text(text: &str, range: &SentenceRange) -> String {
    text.chars()
        .skip(range.start)
        .take(range.end - range.start)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(text: &str) -> Vec<String> {
        segment_chapter(text)
            .iter()
            .map(|r| unit_text(text, r))
            .collect()
    }

    #[test]
    fn frozen_passage_is_five_units() {
        // The Phase 0 ear-test passage, verbatim from
        // voice-samples/passage.txt (Dracula ch. 1): dates, an
        // abbreviation ("p.m."), times with colons, a semicolon
        // clause. The spike's count for this passage: 5 sentences.
        let text = "3 May. Bistritz.\u{FEFF}—Left Munich at 8:35 p.m., on 1st May, arriving at Vienna early next morning; should have arrived at 6:46, but train was an hour late. Budapest seems a wonderful place, from the glimpse which I got of it from the train and the little I could walk through the streets. I feared to go very far from the station, as we had arrived late and would start as near the correct time as possible. The impression I had was that we were leaving the West and entering the East; the most western of splendid bridges over the Danube, which is here of noble width and depth, took us among the traditions of Turkish rule.";
        let u = units(text);
        assert_eq!(u.len(), 5, "{u:?}");
        assert_eq!(u[0], "3 May.");
        assert!(u[1].contains("Left Munich at 8:35 p.m."), "{u:?}");
        assert!(u[4].starts_with("The impression I had"), "{u:?}");
    }

    #[test]
    fn abbreviations_and_decimals_do_not_split() {
        let text = "Dr. Van Helsing arrived at 6.30 in the evening. Mr. Harker was 3.5 miles away, i.e. close. They met at St. Mark's Square.";
        let u = units(text);
        assert_eq!(u.len(), 3, "{u:?}");
    }

    #[test]
    fn initials_do_not_split() {
        let text = "J. R. R. Tolkien wrote it. Then he stopped.";
        let u = units(text);
        assert_eq!(u.len(), 2, "{u:?}");
        assert!(u[0].contains("Tolkien"), "{u:?}");
    }

    #[test]
    fn dialogue_closers_belong_to_the_sentence() {
        let text = "\"Stop!\" he cried. \"You shall not pass!\" The bridge held.";
        let u = units(text);
        assert_eq!(u.len(), 3, "{u:?}");
        assert_eq!(u[0], "\"Stop!\" he cried.");
        assert_eq!(u[1], "\"You shall not pass!\"");
    }

    #[test]
    fn paragraph_break_ends_unpunctuated_unit() {
        let text = "Chapter the First\n\nIt was a bright day. The sun shone on.\n\n***\n\nLater, all was quiet.";
        let u = units(text);
        assert_eq!(
            u,
            vec![
                "Chapter the First",
                "It was a bright day.",
                "The sun shone on.",
                "Later, all was quiet."
            ]
        );
    }

    #[test]
    fn long_unit_splits_at_clause_boundary() {
        let clause = "the rain fell on the roof, ";
        let text = format!("{}and then it stopped. Short one.", clause.repeat(12));
        let u = units(&text);
        assert!(u.len() >= 3, "{u:?}");
        for s in &u {
            assert!(s.chars().count() <= MAX_UNIT_CHARS + 40, "unit too long: {}", s.chars().count());
        }
        // No words were lost or duplicated by the split.
        let joined: String = u.concat();
        let original: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        let rejoined: String = joined.chars().filter(|c| !c.is_whitespace()).collect();
        assert_eq!(original, rejoined);
    }

    #[test]
    fn ranges_are_char_exact() {
        let text = "Üne phrase très simple. La deuxième suit — avec un tiret. Fin.";
        let ranges = segment_chapter(text);
        assert_eq!(ranges.len(), 3);
        for r in &ranges {
            let s = unit_text(text, r);
            assert!(!s.starts_with(char::is_whitespace));
            assert!(!s.ends_with(char::is_whitespace));
        }
        assert!(unit_text(text, &ranges[0]).starts_with('Ü'));
    }
}
