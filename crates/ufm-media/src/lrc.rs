//! lrc: synced-lyrics (LRC) parsing, formatting, and playback
//! lookup — pure logic for Music Completion (Dickles, 2026-10-15).
//!
//! LRCLIB serves lyrics in two shapes: plain text and LRC — lines
//! stamped `[mm:ss.xx]` so a player can highlight the line being
//! sung. This module is the whole LRC story: parse (multi-stamp
//! lines, `[offset:]` shifts, metadata tags ignored), format back
//! to canonical LRC, and "which line is live at position X" for
//! the Quick Look / player highlight. No I/O, no clock — the UI
//! feeds it the audio element's position.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LrcLine {
    pub time_ms: u64,
    pub text: String,
}

/// One `[...]` group at the start of `s`: if it's a timestamp
/// (mm:ss with optional fraction), returns (Some(ms), rest);
/// anything else (metadata like `[ti:...]`, or not a bracket at
/// all) returns None and the caller treats the group as skippable
/// metadata or stops scanning the line.
fn parse_stamp(s: &str) -> Option<(u64, &str)> {
    let close = s.find(']')?;
    let inner = &s[1..close];
    let rest = &s[close + 1..];
    let (min_str, sec_part) = inner.split_once(':')?;
    let min: u64 = min_str.trim().parse().ok()?;
    let (sec_str, frac_ms) = match sec_part.split_once(['.', ',']) {
        Some((sec, f)) => {
            let f = f.trim();
            let ms = match f.len() {
                0 => 0,
                1 => f.parse::<u64>().ok()? * 100,
                2 => f.parse::<u64>().ok()? * 10,
                _ => f[..3].parse::<u64>().ok()?,
            };
            (sec, ms)
        }
        None => (sec_part, 0),
    };
    let sec: u64 = sec_str.trim().parse().ok()?;
    Some((min * 60_000 + sec * 1000 + frac_ms, rest))
}

/// Does this text look like LRC (at least one timestamped line)?
/// Lyrics stored in a tag are plain *or* LRC — this is how the
/// reader tells them apart.
pub fn looks_synced(text: &str) -> bool {
    text.lines().any(|l| {
        let l = l.trim_start();
        l.starts_with('[') && parse_stamp(l).is_some()
    })
}

/// Parse LRC text into sorted lines. Metadata tags (`[ar:]`,
/// `[ti:]`, …) are skipped; `[offset:±ms]` shifts every stamp by
/// the offset (positive = lyrics later). A line with several
/// stamps yields one entry per stamp. Lines with no stamp are
/// dropped (they can't be highlighted; the plain fallback is the
/// raw text itself, which the caller keeps separately).
pub fn parse_lrc(text: &str) -> Vec<LrcLine> {
    let mut offset: i64 = 0;
    // First pass: find the offset (it applies to the whole file,
    // wherever the tag sits).
    for raw in text.lines() {
        let l = raw.trim();
        if let Some(inner) = l.strip_prefix("[offset:").and_then(|s| s.strip_suffix(']')) {
            offset = inner.trim().parse::<i64>().unwrap_or(0);
        }
    }
    let mut out: Vec<LrcLine> = Vec::new();
    for raw in text.lines() {
        let mut rest = raw.trim_start();
        if rest.starts_with("[offset:") {
            continue;
        }
        let mut stamps: Vec<u64> = Vec::new();
        while rest.starts_with('[') {
            match parse_stamp(rest) {
                Some((ms, r)) => {
                    stamps.push(ms);
                    rest = r;
                }
                // Metadata tag (`[ti:...]`): skip past it and keep
                // scanning — a stamp may still follow on this line.
                None => match rest.find(']') {
                    Some(close) => rest = &rest[close + 1..],
                    None => break,
                },
            }
        }
        if stamps.is_empty() {
            continue;
        }
        let text = rest.trim().to_string();
        for ms in stamps {
            let shifted = (ms as i64 + offset).max(0) as u64;
            out.push(LrcLine {
                time_ms: shifted,
                text: text.clone(),
            });
        }
    }
    out.sort_by_key(|l| l.time_ms);
    out
}

/// Format lines back to canonical LRC (`[mm:ss.xx]text`).
pub fn to_lrc(lines: &[LrcLine]) -> String {
    let mut sorted: Vec<&LrcLine> = lines.iter().collect();
    sorted.sort_by_key(|l| l.time_ms);
    let mut out = String::new();
    for l in sorted {
        let total_cs = l.time_ms / 10;
        out.push_str(&format!(
            "[{:02}:{:02}.{:02}]{}\n",
            total_cs / 6000,
            (total_cs / 100) % 60,
            total_cs % 100,
            l.text
        ));
    }
    out
}

/// The index of the line live at `pos_ms` — the last line whose
/// stamp is at or before the position. None before the first line.
pub fn line_at(lines: &[LrcLine], pos_ms: u64) -> Option<usize> {
    let mut hit: Option<usize> = None;
    for (i, l) in lines.iter().enumerate() {
        if l.time_ms <= pos_ms {
            hit = Some(i);
        } else {
            break;
        }
    }
    hit
}

/// Strip the stamps, leaving readable plain text (for surfaces
/// that can't do sync, and for search).
pub fn to_plain(text: &str) -> String {
    parse_lrc(text)
        .iter()
        .map(|l| l.text.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "[ti:Creep]\n[ar:Radiohead]\n[00:12.00]When you were here before\n[00:17.50]Couldn't look you in the eye\n[00:23.25][00:45.00]You're just like an angel\n";

    #[test]
    fn parses_stamps_and_skips_metadata() {
        let lines = parse_lrc(SAMPLE);
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0].time_ms, 12_000);
        assert_eq!(lines[0].text, "When you were here before");
        assert_eq!(lines[1].time_ms, 17_500);
        // multi-stamp line produced two entries, same text
        assert_eq!(lines[2].time_ms, 23_250);
        assert_eq!(lines[3].time_ms, 45_000);
        assert_eq!(lines[2].text, lines[3].text);
    }

    #[test]
    fn offset_shifts_everything() {
        let lines = parse_lrc("[offset:+1000]\n[00:01.00]a\n[00:02.00]b\n");
        assert_eq!(lines[0].time_ms, 2_000);
        assert_eq!(lines[1].time_ms, 3_000);
        let neg = parse_lrc("[offset:-1500]\n[00:01.00]a\n[00:02.00]b\n");
        assert_eq!(neg[0].time_ms, 0); // clamped, never negative
        assert_eq!(neg[1].time_ms, 500);
    }

    #[test]
    fn fraction_widths() {
        assert_eq!(parse_lrc("[00:01.5]x\n")[0].time_ms, 1_500);
        assert_eq!(parse_lrc("[00:01.05]x\n")[0].time_ms, 1_050);
        assert_eq!(parse_lrc("[00:01.005]x\n")[0].time_ms, 1_005);
        assert_eq!(parse_lrc("[00:01]x\n")[0].time_ms, 1_000);
        assert_eq!(parse_lrc("[01:02,34]x\n")[0].time_ms, 62_340);
    }

    #[test]
    fn line_at_boundaries() {
        let lines = parse_lrc(SAMPLE);
        assert_eq!(line_at(&lines, 0), None);
        assert_eq!(line_at(&lines, 11_999), None);
        assert_eq!(line_at(&lines, 12_000), Some(0));
        assert_eq!(line_at(&lines, 20_000), Some(1));
        assert_eq!(line_at(&lines, 999_999), Some(3));
    }

    #[test]
    fn round_trip_is_stable() {
        let once = to_lrc(&parse_lrc(SAMPLE));
        let twice = to_lrc(&parse_lrc(&once));
        assert_eq!(once, twice);
        assert!(once.starts_with("[00:12.00]When you were here before"));
    }

    #[test]
    fn looks_synced_detects_lrc() {
        assert!(looks_synced(SAMPLE));
        assert!(!looks_synced("just some plain lyrics\nsecond line\n"));
        assert!(to_plain(SAMPLE).contains("When you were here before"));
    }
}
