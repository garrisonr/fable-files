//! auditor: the File Auditor (Dickles, 2026-10-15 06:45–06:57) —
//! a special section that audits the library and reports. It never
//! silently changes anything: it flags what is CERTAIN as fact,
//! ASKS about what is heuristic (the ask lane — a quiet question in
//! the review surface, never a modal), remembers every "this one is
//! OK" forever, and turns ask-lane answers into standing class law.
//!
//! Laws (DESIGN.md, FILE AUDITOR + EXTENSIONS):
//! - Typo hunt is dictionary-first: a name that is MOSTLY dictionary
//!   words with one near-miss token is a question with a suggested
//!   correction; a name that is mostly NOT words is gibberish and is
//!   left alone. The word list is the app's own bundled SCOWL
//!   derivative (assets/words-en.txt.gz — see WORDS-NOTICE.txt),
//!   extended by the user's confirmed names.
//! - Style drift is FOLDER-RELATIVE, never global taste: "If you
//!   are a moron and love all caps then be consistently stupid."
//! - Space proposals label their honesty: LOSSLESS vs RE-ENCODE
//!   (generational), estimates labeled as estimates, true sizes
//!   reported after execution by the job's step notes.
//! - Audits run manually or on the shared background scheduler
//!   (mediacomplete's agent loop calls `agent_tick`); a run leaves
//!   a durable summary + checklist in the catalog.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use ufm_catalog::{AuditorFlagRow, AuditorRunRow, Catalog};

pub use crate::mediacomplete::data_base;

// ---------------------------------------------------------------------------
// Settings + schedules (JSON sidecar, atomic writes — house pattern)
// ---------------------------------------------------------------------------

fn settings_path(base: &Path) -> PathBuf {
    base.join("auditor-settings.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditorFamilies {
    pub typo: bool,
    pub pattern: bool,
    pub corrupt: bool,
    /// OPT-IN (spec item 7: "an option") — default OFF.
    pub compat: bool,
    pub space: bool,
    pub junk: bool,
    /// The "more cleanup families" bundle (empties, stale partials,
    /// OS junk, copy chains, orphan sidecars, archive pairs, stale
    /// installers).
    pub clutter: bool,
    pub style: bool,
    pub editions: bool,
    pub cold: bool,
}

impl Default for AuditorFamilies {
    fn default() -> Self {
        Self {
            typo: true,
            pattern: true,
            corrupt: true,
            compat: false,
            space: true,
            junk: true,
            clutter: true,
            style: true,
            editions: true,
            cold: true,
        }
    }
}

/// An audit schedule — same shape vocabulary as
/// mediacomplete::Schedule so the agent's tested state machine
/// evaluates it (converted, never reimplemented).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditSchedule {
    pub id: String,
    pub name: String,
    /// Drive or folder tree to audit.
    pub target: String,
    /// Local time of day, "HH:MM".
    pub time: String,
    /// Seven flags, Monday first. Empty/all-false = every day.
    #[serde(default)]
    pub weekdays: Vec<bool>,
    #[serde(default)]
    pub idle_minutes: u32,
    #[serde(default)]
    pub require_on_ac: bool,
    pub enabled: bool,
    #[serde(default)]
    pub last_started: Option<String>,
    #[serde(default)]
    pub last_status: Option<String>,
}

impl AuditSchedule {
    fn as_mc(&self) -> crate::mediacomplete::Schedule {
        crate::mediacomplete::Schedule {
            id: self.id.clone(),
            name: self.name.clone(),
            target: self.target.clone(),
            steps: crate::mediacomplete::ScheduleSteps::default(),
            time: self.time.clone(),
            weekdays: self.weekdays.clone(),
            idle_minutes: self.idle_minutes,
            require_on_ac: self.require_on_ac,
            enabled: self.enabled,
            last_started: self.last_started.clone(),
            last_status: self.last_status.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditorSettings {
    #[serde(default)]
    pub families: AuditorFamilies,
    /// A partial/temp file older than this many days is stale.
    #[serde(default = "default_stale_days")]
    pub stale_days: u32,
    /// Files untouched longer than this many years are cold storage.
    #[serde(default = "default_cold_years")]
    pub cold_years: u32,
    #[serde(default)]
    pub schedules: Vec<AuditSchedule>,
}

fn default_stale_days() -> u32 {
    7
}
fn default_cold_years() -> u32 {
    3
}

impl Default for AuditorSettings {
    fn default() -> Self {
        Self {
            families: AuditorFamilies::default(),
            stale_days: default_stale_days(),
            cold_years: default_cold_years(),
            schedules: Vec::new(),
        }
    }
}

pub fn load_settings(base: &Path) -> AuditorSettings {
    std::fs::read_to_string(settings_path(base))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_settings(base: &Path, settings: &AuditorSettings) -> Result<(), String> {
    std::fs::create_dir_all(base).map_err(|e| e.to_string())?;
    let tmp = base.join("auditor-settings.json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, settings_path(base)).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Time helpers (the state machine's stamp format — mediacomplete's)
// ---------------------------------------------------------------------------

pub fn now_local() -> chrono::NaiveDateTime {
    chrono::Local::now().naive_local()
}

pub fn stamp(t: chrono::NaiveDateTime) -> String {
    t.format("%Y-%m-%dT%H:%M:%S").to_string()
}

fn rfc(t: SystemTime) -> String {
    let dt: chrono::DateTime<chrono::Local> = t.into();
    dt.format("%Y-%m-%dT%H:%M:%S").to_string()
}

fn age_days(mtime: Option<SystemTime>) -> Option<u64> {
    let m = mtime?;
    SystemTime::now()
        .duration_since(m)
        .ok()
        .map(|d| d.as_secs() / 86_400)
}

// ---------------------------------------------------------------------------
// Dictionary: the app's bundled SCOWL derivative + user words.
// ---------------------------------------------------------------------------

static BUNDLED_WORDS: std::sync::OnceLock<HashSet<String>> = std::sync::OnceLock::new();

/// The bundled word list (assets/words-en.txt.gz — SCOWL en_US,
/// normalized; provenance + license in WORDS-NOTICE.txt). Loaded
/// once, lazily. The app NEVER reads a system dictionary: user
/// machines may not have one.
pub fn bundled_words() -> &'static HashSet<String> {
    BUNDLED_WORDS.get_or_init(|| {
        let gz = include_bytes!("../assets/words-en.txt.gz");
        let mut text = String::new();
        let mut set = HashSet::new();
        if flate2::read::GzDecoder::new(&gz[..])
            .read_to_string(&mut text)
            .is_ok()
        {
            for line in text.lines() {
                let w = line.trim();
                if !w.is_empty() {
                    set.insert(w.to_string());
                }
            }
        }
        set
    })
}

/// Effective dictionary for a run: bundled ∪ the user's confirmed
/// words (his files teach it — spec item 1).
pub fn effective_words(catalog: &Catalog) -> HashSet<String> {
    let mut set = bundled_words().clone();
    if let Ok(user) = catalog.auditor_user_words() {
        for w in user {
            set.insert(w);
        }
    }
    set
}

/// Damerau–Levenshtein (optimal string alignment) distance.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for i in 0..=n {
        d[i][0] = i;
    }
    for j in 0..=m {
        d[0][j] = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            d[i][j] = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[n][m]
}

/// Best dictionary correction for a token: same first letter
/// preferred, then smallest distance, then alphabetical (stable,
/// explainable). None when nothing is close enough to propose.
fn best_correction(token: &str, words: &HashSet<String>) -> Option<String> {
    let max_dist = if token.len() <= 5 { 1 } else { 2 };
    let first = token.chars().next()?;
    let mut best: Option<(usize, bool, String)> = None;
    for w in words {
        if w.len() < 3 || (w.len() as i64 - token.len() as i64).abs() > max_dist as i64 {
            continue;
        }
        let d = edit_distance(token, w);
        if d == 0 || d > max_dist {
            continue;
        }
        let cand = (d, w.chars().next() != Some(first), w.clone());
        match &best {
            None => best = Some(cand),
            Some(b) => {
                if (cand.0, cand.1, cand.2.clone()) < (b.0, b.1, b.2.clone()) {
                    best = Some(cand);
                }
            }
        }
    }
    best.map(|(_, _, w)| w)
}

/// Match a replacement word to the original token's case style
/// (Richrd → Richard, RICHRD → RICHARD, richrd → richard).
fn match_case(original: &str, replacement: &str) -> String {
    let mut chars = original.chars();
    let all_upper = original.chars().all(|c| !c.is_lowercase())
        && original.chars().any(|c| c.is_uppercase());
    if all_upper {
        return replacement.to_uppercase();
    }
    if chars.next().map(|c| c.is_uppercase()).unwrap_or(false)
        && original.chars().skip(1).all(|c| !c.is_uppercase())
    {
        let mut out = replacement.to_string();
        if let Some(f) = out.get_mut(0..1) {
            f.make_ascii_uppercase();
        }
        return out;
    }
    replacement.to_string()
}

// ---------------------------------------------------------------------------
// The walk: one pass over the target tree, rails up front.
// ---------------------------------------------------------------------------

pub const WALK_CAP: usize = 250_000;
const SKIP_DIRS: [&str; 3] = [".git", ".svn", ".hg"];

#[derive(Debug, Clone)]
pub struct EntryInfo {
    pub path: PathBuf,
    pub name: String,
    pub parent: PathBuf,
    pub is_dir: bool,
    pub is_symlink: bool,
    /// Symlink whose target cannot be resolved.
    pub broken_link: bool,
    pub size: u64,
    pub mtime: Option<SystemTime>,
}

impl EntryInfo {
    pub fn path_str(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
    pub fn ext(&self) -> String {
        self.path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    }
    pub fn stem(&self) -> String {
        self.path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.name.clone())
    }
}

#[derive(Debug, Default)]
pub struct ScanTree {
    pub entries: Vec<EntryInfo>,
    pub truncated: bool,
}

fn walk(root: &Path) -> ScanTree {
    let mut tree = ScanTree::default();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut children: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        children.sort();
        for path in children {
            if tree.entries.len() >= WALK_CAP {
                tree.truncated = true;
                return tree;
            }
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let Ok(md) = path.symlink_metadata() else {
                continue;
            };
            let ft = md.file_type();
            let is_symlink = ft.is_symlink();
            let mut is_dir = ft.is_dir();
            let mut broken = false;
            let mut size = if ft.is_file() { md.len() } else { 0 };
            if is_symlink {
                match std::fs::metadata(&path) {
                    Ok(t) => {
                        is_dir = t.is_dir();
                        size = if t.is_file() { t.len() } else { 0 };
                    }
                    Err(_) => broken = true,
                }
            }
            tree.entries.push(EntryInfo {
                parent: dir.clone(),
                name,
                path: path.clone(),
                is_dir,
                is_symlink,
                broken_link: broken,
                size,
                mtime: md.modified().ok(),
            });
            // Recurse into real directories only — never through a
            // symlink (loops), never into VCS internals.
            if ft.is_dir() {
                let base = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if !SKIP_DIRS.contains(&base.as_str()) {
                    stack.push(path);
                }
            }
        }
    }
    tree
}

/// Direct children of each folder in the tree: parent → entry idxs.
fn children_by_parent(tree: &ScanTree) -> HashMap<PathBuf, Vec<usize>> {
    let mut map: HashMap<PathBuf, Vec<usize>> = HashMap::new();
    for (i, e) in tree.entries.iter().enumerate() {
        map.entry(e.parent.clone()).or_default().push(i);
    }
    map
}

// ---------------------------------------------------------------------------
// Media probing (ffprobe) — injectable so tests never shell out.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct ProbeResult {
    /// ffprobe read the file and found a format/streams.
    pub readable: bool,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
}

pub trait MediaProbe: Send + Sync {
    fn available(&self) -> bool;
    /// None = the probe could not run at all (distinct from
    /// Some(readable=false), which is a verdict about the file).
    fn probe(&self, path: &Path) -> Option<ProbeResult>;
}

/// Run a command to completion with a hard timeout; returns
/// (success, stdout) or None when it can't run / times out.
fn run_capture(program: &str, args: &[&str], timeout: Duration) -> Option<(bool, String)> {
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let out = child.wait_with_output().ok()?;
                return Some((
                    out.status.success(),
                    String::from_utf8_lossy(&out.stdout).into_owned(),
                ));
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => return None,
        }
    }
}

pub struct FfprobeProbe {
    available: bool,
}

impl FfprobeProbe {
    pub fn new() -> Self {
        let available =
            run_capture("ffprobe", &["-version"], Duration::from_secs(5))
                .map(|(ok, _)| ok)
                .unwrap_or(false);
        Self { available }
    }
}

impl Default for FfprobeProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl MediaProbe for FfprobeProbe {
    fn available(&self) -> bool {
        self.available
    }

    fn probe(&self, path: &Path) -> Option<ProbeResult> {
        if !self.available {
            return None;
        }
        let (ok, out) = run_capture(
            "ffprobe",
            &[
                "-v",
                "error",
                "-show_entries",
                "stream=codec_type,codec_name",
                "-of",
                "json",
                &path.to_string_lossy(),
            ],
            Duration::from_secs(15),
        )?;
        let v: serde_json::Value = serde_json::from_str(&out).ok()?;
        let streams = v.get("streams").and_then(|s| s.as_array());
        let (mut video, mut audio) = (None, None);
        if let Some(streams) = streams {
            for s in streams {
                let codec = s.get("codec_name").and_then(|c| c.as_str());
                match s.get("codec_type").and_then(|c| c.as_str()) {
                    Some("video") => video = codec.map(|c| c.to_string()),
                    Some("audio") => audio = codec.map(|c| c.to_string()),
                    _ => {}
                }
            }
        }
        Some(ProbeResult {
            readable: ok && (video.is_some() || audio.is_some()),
            video_codec: video,
            audio_codec: audio,
        })
    }
}

/// A probe that never runs — tests and tool-absent runs.
pub struct NoProbe;
impl MediaProbe for NoProbe {
    fn available(&self) -> bool {
        false
    }
    fn probe(&self, _path: &Path) -> Option<ProbeResult> {
        None
    }
}

// ---------------------------------------------------------------------------
// Candidates: one detector finding, before memory filtering.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Candidate {
    pub path: String,
    pub family: &'static str,
    /// "certain" | "question"
    pub lane: &'static str,
    pub class_key: String,
    pub title: String,
    pub detail: String,
    pub proposal: serde_json::Value,
}

fn prop_none() -> serde_json::Value {
    serde_json::json!({ "kind": "none" })
}
fn prop_trash() -> serde_json::Value {
    serde_json::json!({ "kind": "trash" })
}
fn prop_rename(to: &str) -> serde_json::Value {
    serde_json::json!({ "kind": "rename", "to": to })
}

pub const VIDEO_EXTS: [&str; 12] = [
    "mkv", "mp4", "avi", "mov", "wmv", "webm", "m4v", "ts", "m2ts", "mpg", "mpeg", "flv",
];
pub const AUDIO_EXTS: [&str; 8] = ["mp3", "flac", "wav", "wave", "ogg", "opus", "m4a", "aac"];
pub const IMAGE_EXTS: [&str; 8] = ["jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff"];

fn is_video(e: &EntryInfo) -> bool {
    VIDEO_EXTS.contains(&e.ext().as_str())
}
fn is_audio(e: &EntryInfo) -> bool {
    AUDIO_EXTS.contains(&e.ext().as_str())
}

// ---------------------------------------------------------------------------
// 1. TYPO HUNT — dictionary-first, gibberish left alone.
// ---------------------------------------------------------------------------

/// THE release-vocabulary exclusion list: tokens that are never
/// typos, so the typo hunt never scores them. Release-world words
/// legitimately aren't in a general dictionary — technical/source
/// tags, streaming-service tags, and release-group/uploader brands.
/// (Dickles, 2026-10-15: the engine asked "Did you mean 'yes'?" at
/// the [YTS.MX] tag on every YTS file in his library — "YTS, we
/// know what that means. Exclude it by default.") This is the ONE
/// vocabulary source for the typo family; the junk family below
/// recognizes the same uploader brands (yts, rarbg) by name
/// signature — keep the two in step when adding a brand. Grow the
/// list here, in one place, rather than teaching detectors ad hoc.
const NEUTRAL_TOKENS: [&str; 50] = [
    // technical / source / quality tags
    "bluray", "remux", "webrip", "webdl", "bdrip", "brrip", "hdrip", "dvdrip",
    "hdtv", "hdcam", "hdts", "x264", "x265", "h264", "h265", "hevc", "avc", "av1",
    "xvid", "divx", "aac", "dts", "ddp", "truehd", "ac3", "atmos", "uhd", "hdr",
    "sdr", "fps", "720p", "1080p", "2160p", "480p", "imax", "3d",
    // streaming-service tags (scene abbreviations)
    "amzn", "hulu", "dsnp", "atvp", "pcok", "pmtp", "hbo", "starz",
    // release groups / uploader brands — never typos
    "yts", "yify", "rarbg", "eztv", "galaxyrg", "tgx",
];

/// Split a name stem into word tokens (ORIGINAL case): non-alpha
/// separators plus camelCase humps. Pure-digit and digit-mixed
/// chunks drop out.
fn name_tokens_raw(stem: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in stem.chars() {
        if c.is_alphabetic() {
            if c.is_uppercase() && prev_lower && !cur.is_empty() {
                tokens.push(std::mem::take(&mut cur));
            }
            cur.push(c);
            prev_lower = c.is_lowercase();
        } else {
            if !cur.is_empty() {
                tokens.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
        .into_iter()
        .filter(|t| t.len() >= 3 && t.chars().all(|c| c.is_alphabetic()))
        .collect()
}

/// Lowercase scored tokens for the dictionary check.
fn name_tokens(stem: &str) -> Vec<String> {
    name_tokens_raw(stem)
        .into_iter()
        .map(|t| t.to_lowercase())
        .collect()
}

fn detect_typos(tree: &ScanTree, words: &HashSet<String>) -> Vec<Candidate> {
    let mut out = Vec::new();
    for e in &tree.entries {
        if e.is_symlink {
            continue;
        }
        let tokens = name_tokens(&e.stem());
        if tokens.len() < 3 {
            continue;
        }
        let mut dict_hits = 0usize;
        let mut misses: Vec<&String> = Vec::new();
        for t in &tokens {
            if NEUTRAL_TOKENS.contains(&t.as_str()) || words.contains(t) {
                dict_hits += 1;
            } else {
                misses.push(t);
            }
        }
        let scored = tokens.len();
        let ratio = dict_hits as f64 / scored as f64;
        // Gibberish rule: mostly NOT words → not a typo, a hash/ID/
        // scene string. Leave it alone, silently.
        if ratio < 0.4 || dict_hits < 2 {
            continue;
        }
        // The typo shape: mostly words, exactly one near miss.
        if ratio < 0.6 || misses.len() != 1 {
            continue;
        }
        let miss = misses[0];
        let Some(fix) = best_correction(miss, words) else {
            continue;
        };
        // Rebuild the file name with the token replaced in place,
        // preserving the original's case style for that token.
        let fixed_stem = replace_token(&e.stem(), miss, &fix);
        let new_name = match e.path.extension() {
            Some(ext) if !e.is_dir => format!("{}.{}", fixed_stem, ext.to_string_lossy()),
            _ => fixed_stem,
        };
        if new_name == e.name {
            continue;
        }
        // ONE rendering of the suggestion (Dickles, 2026-10-15):
        // the suggested word carries the CASE PATTERN of the token
        // it replaces — Capitalized → Capitalized, ALL-CAPS →
        // ALL-CAPS, lowercase → lowercase — computed once here and
        // used by the headline, the detail, and (via replace_token,
        // which match-cases the same way) the rename proposal, so
        // the three spots can never disagree again. The bug was the
        // headline printing the raw lowercase dictionary form.
        let shown = display_token(&e.stem(), miss);
        let fix_cased = match_case(&shown, &fix);
        out.push(Candidate {
            path: e.path_str(),
            family: "typo",
            lane: "question",
            class_key: format!("typo:{miss}"),
            title: format!("Did you mean “{fix_cased}”?"),
            detail: format!(
                "“{shown}” looks like a misspelling of “{fix_cased}” in an otherwise ordinary name."
            ),
            proposal: prop_rename(&new_name),
        });
    }
    out
}

/// The token as it appears in the stem (original case), for copy.
fn display_token(stem: &str, lower_token: &str) -> String {
    name_tokens_raw(stem)
        .into_iter()
        .find(|t| t.to_lowercase() == lower_token)
        .unwrap_or_else(|| lower_token.to_string())
}

/// Replace the (single) case-insensitive occurrence of `miss` as a
/// whole token inside `stem`, keeping separators intact.
fn replace_token(stem: &str, miss: &str, fix: &str) -> String {
    let lower = stem.to_lowercase();
    let mut start = 0usize;
    while let Some(pos) = lower[start..].find(miss) {
        let at = start + pos;
        let before_ok = at == 0 || !lower[..at].ends_with(|c: char| c.is_alphabetic());
        let end = at + miss.len();
        let after_ok =
            end >= lower.len() || !lower[end..].starts_with(|c: char| c.is_alphabetic());
        if before_ok && after_ok {
            let orig = &stem[at..end];
            let styled = match_case(orig, fix);
            return format!("{}{}{}", &stem[..at], styled, &stem[end..]);
        }
        start = at + 1;
    }
    stem.to_string()
}

// ---------------------------------------------------------------------------
// 2. PATTERN BREAKS — series inference inside one folder.
// ---------------------------------------------------------------------------

/// Parse a stem into (alpha prefix, number, tail) around its LAST
/// digit run: "XY12" → ("XY", 12, ""), "Show S01E02" has two runs —
/// the last ("02") is the sequence slot this detector keys on.
fn split_series(stem: &str) -> Option<(String, u64, String)> {
    let bytes: Vec<(usize, char)> = stem.char_indices().collect();
    let mut last_run: Option<(usize, usize)> = None;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].1.is_ascii_digit() {
            let s = i;
            while i < bytes.len() && bytes[i].1.is_ascii_digit() {
                i += 1;
            }
            last_run = Some((s, i));
        } else {
            i += 1;
        }
    }
    let (s, e) = last_run?;
    let start_byte = bytes[s].0;
    let end_byte = if e < bytes.len() {
        bytes[e].0
    } else {
        stem.len()
    };
    let num: u64 = stem[start_byte..end_byte].parse().ok()?;
    Some((
        stem[..start_byte].to_string(),
        num,
        stem[end_byte..].to_string(),
    ))
}

fn detect_patterns(tree: &ScanTree, by_parent: &HashMap<PathBuf, Vec<usize>>) -> Vec<Candidate> {
    let mut out = Vec::new();
    for (parent, idxs) in by_parent {
        // Series key: (prefix shape, tail, ext). The prefix shape
        // keeps letters only so "XY" and "X.Y." don't merge.
        let mut series: HashMap<(String, String, String), Vec<(u64, usize)>> = HashMap::new();
        for &i in idxs {
            let e = &tree.entries[i];
            if e.is_dir || e.is_symlink {
                continue;
            }
            if let Some((prefix, num, tail)) = split_series(&e.stem()) {
                let alpha: String = prefix
                    .chars()
                    .filter(|c| c.is_alphabetic())
                    .collect::<String>()
                    .to_lowercase();
                if alpha.len() < 2 {
                    continue;
                }
                series
                    .entry((alpha, tail.to_lowercase(), e.ext()))
                    .or_default()
                    .push((num, i));
            }
        }
        for ((alpha, tail, ext), mut members) in series {
            if members.len() < 3 {
                continue;
            }
            members.sort();
            let nums: Vec<u64> = members.iter().map(|m| m.0).collect();
            let (min, max) = (*nums.first().unwrap(), *nums.last().unwrap());
            let span = max - min + 1;
            if span > 500 || (nums.len() as f64) / (span as f64) < 0.7 {
                continue;
            }
            // Display stem from a real member (original case).
            let sample = &tree.entries[members[0].1];
            let (disp_prefix, _, _) = split_series(&sample.stem()).unwrap();
            let present: HashSet<u64> = nums.iter().copied().collect();
            // Gaps: observed fact, stated plainly (certain lane).
            for missing in min..=max {
                if !present.contains(&missing) {
                    out.push(Candidate {
                        path: parent.to_string_lossy().into_owned(),
                        family: "pattern",
                        lane: "certain",
                        class_key: format!("pattern:gap:{alpha}"),
                        title: format!("{disp_prefix}{missing} is missing from this series"),
                        detail: format!(
                            "This folder holds a run from {disp_prefix}{min} to {disp_prefix}{max}, but {disp_prefix}{missing} isn't there."
                        ),
                        proposal: prop_none(),
                    });
                }
            }
            // Outliers, form 1: a MINORITY raw-prefix form inside
            // the series — "XY-2" among "XY1, XY2" (a stray
            // separator in the number slot). The dominant form is
            // the folder's own convention; minorities are asked
            // about, with the series form proposed.
            {
                let mut prefix_forms: HashMap<String, (usize, String)> = HashMap::new();
                for &(num, i) in &members {
                    let _ = num;
                    let e = &tree.entries[i];
                    if let Some((raw, _, _)) = split_series(&e.stem()) {
                        let entry = prefix_forms
                            .entry(raw.to_lowercase())
                            .or_insert((0, raw));
                        entry.0 += 1;
                    }
                }
                if prefix_forms.len() >= 2 {
                    let (dom_key, (dom_count, dom_raw)) = prefix_forms
                        .iter()
                        .max_by_key(|(_, (n, _))| *n)
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .unwrap();
                    if (dom_count as f64) / (members.len() as f64) >= 0.6 {
                        for &(num, i) in &members {
                            let e = &tree.entries[i];
                            let Some((raw, _, tail_raw)) = split_series(&e.stem()) else {
                                continue;
                            };
                            if raw.to_lowercase() == dom_key {
                                continue;
                            }
                            let proposed = format!("{dom_raw}{num}{tail_raw}");
                            let new_name = if ext.is_empty() {
                                proposed
                            } else {
                                format!("{proposed}.{ext}")
                            };
                            out.push(Candidate {
                                path: e.path_str(),
                                family: "pattern",
                                lane: "question",
                                class_key: format!("pattern:outlier:{alpha}"),
                                title: format!(
                                    "Does “{}” belong to the {disp_prefix} series?",
                                    e.name
                                ),
                                detail: format!(
                                    "It sits with the {disp_prefix}{min}–{disp_prefix}{max} run but its number slot is shaped differently. The series form would be “{new_name}”."
                                ),
                                proposal: prop_rename(&new_name),
                            });
                        }
                    }
                }
            }
            // Outliers, form 2: extra letters before the digits —
            // "XYY3" next to the XY series. Not a series member at
            // all (its own stem differs), but close enough to ask.
            for &i in idxs {
                let e = &tree.entries[i];
                if e.is_dir || e.is_symlink || e.ext() != ext {
                    continue;
                }
                if members.iter().any(|m| m.1 == i) {
                    continue;
                }
                let stem_l = e.stem().to_lowercase();
                let alpha_stem: String = stem_l
                    .chars()
                    .take_while(|c| c.is_alphabetic())
                    .collect();
                if !alpha_stem.starts_with(&alpha)
                    || alpha_stem.len() <= alpha.len()
                    || alpha_stem.len() - alpha.len() > 2
                {
                    continue;
                }
                if let Some((_, num, t)) = split_series(&e.stem()) {
                    if t.to_lowercase() != tail {
                        continue;
                    }
                    let proposed = format!("{disp_prefix}{num}{t}");
                    let new_name = if ext.is_empty() {
                        proposed
                    } else {
                        format!("{proposed}.{ext}")
                    };
                    out.push(Candidate {
                        path: e.path_str(),
                        family: "pattern",
                        lane: "question",
                        class_key: format!("pattern:outlier:{alpha}"),
                        title: format!("Does “{}” belong to the {disp_prefix} series?", e.name),
                        detail: format!(
                            "It sits with the {disp_prefix}{min}–{disp_prefix}{max} run but its number slot is shaped differently. The series form would be “{new_name}”."
                        ),
                        proposal: prop_rename(&new_name),
                    });
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 3. CORRUPT FILES — layered, honest severity.
// ---------------------------------------------------------------------------

/// Extension → magic prefix (with optional offset check for
/// RIFF/ftyp containers handled in code).
fn magic_for(ext: &str) -> Option<&'static [u8]> {
    Some(match ext {
        "png" => b"\x89PNG",
        "jpg" | "jpeg" => b"\xff\xd8\xff",
        "gif" => b"GIF8",
        "pdf" => b"%PDF",
        "zip" | "epub" | "jar" | "docx" | "xlsx" | "pptx" => b"PK",
        "mp3" => b"ID3",
        "flac" => b"fLaC",
        "mkv" | "mka" | "webm" => b"\x1a\x45\xdf\xa3",
        "7z" => b"\x37\x7a\xbc\xaf\x27\x1c",
        "rar" => b"Rar!",
        "gz" | "tgz" => b"\x1f\x8b",
        "sqlite3" => b"SQLite format 3",
        _ => return None,
    })
}

fn head_bytes(path: &Path, n: usize) -> std::io::Result<Vec<u8>> {
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; n];
    let read = f.read(&mut buf)?;
    buf.truncate(read);
    Ok(buf)
}

fn detect_corrupt(tree: &ScanTree, probe: &dyn MediaProbe) -> (Vec<Candidate>, Vec<String>) {
    let mut out = Vec::new();
    let mut notes = Vec::new();
    let mut probe_missing_noted = false;
    for e in &tree.entries {
        if e.broken_link {
            out.push(Candidate {
                path: e.path_str(),
                family: "corrupt",
                lane: "certain",
                class_key: "corrupt:broken-link".into(),
                title: "Broken symbolic link".into(),
                detail: "This link points at something that isn't there anymore.".into(),
                proposal: prop_trash(),
            });
            continue;
        }
        if e.is_dir || e.is_symlink {
            continue;
        }
        // Zero-byte: cannot contain anything.
        if e.size == 0 {
            out.push(Candidate {
                path: e.path_str(),
                family: "corrupt",
                lane: "certain",
                class_key: "corrupt:empty".into(),
                title: "Empty file (0 bytes)".into(),
                detail: "This file is empty — whatever was meant to be in it isn't."
                    .into(),
                proposal: prop_trash(),
            });
            continue;
        }
        // Readable at all?
        let head = match head_bytes(&e.path, 16) {
            Ok(h) => h,
            Err(err) => {
                out.push(Candidate {
                    path: e.path_str(),
                    family: "corrupt",
                    lane: "certain",
                    class_key: "corrupt:unreadable".into(),
                    title: "File can't be read".into(),
                    detail: format!("Opening it fails: {err}"),
                    proposal: prop_none(),
                });
                continue;
            }
        };
        // Magic vs extension.
        let ext = e.ext();
        if let Some(magic) = magic_for(&ext) {
            let mp3_sync = ext == "mp3"
                && head.len() >= 2
                && head[0] == 0xff
                && (head[1] & 0xe0) == 0xe0;
            if !head.starts_with(magic) && !mp3_sync {
                out.push(Candidate {
                    path: e.path_str(),
                    family: "corrupt",
                    lane: "certain",
                    class_key: "corrupt:magic-mismatch".into(),
                    title: format!("Contents don't match the .{ext} extension"),
                    detail: "The file's header isn't what its name claims — it may be truncated, mislabeled, or not this kind of file at all.".into(),
                    proposal: prop_none(),
                });
                continue;
            }
        }
        // RIFF containers: WAV must say WAVE, WEBP must say WEBP.
        if (ext == "wav" || ext == "wave" || ext == "webp") && head.len() >= 12 {
            let tag = &head[8..12];
            let want: &[u8] = if ext == "webp" { b"WEBP" } else { b"WAVE" };
            if tag != want {
                out.push(Candidate {
                    path: e.path_str(),
                    family: "corrupt",
                    lane: "certain",
                    class_key: "corrupt:magic-mismatch".into(),
                    title: format!("Contents don't match the .{ext} extension"),
                    detail: "The RIFF header names a different format than the file extension does.".into(),
                    proposal: prop_none(),
                });
                continue;
            }
        }
        // Media probe for audio/video.
        if is_video(e) || is_audio(e) {
            if !probe.available() {
                if !probe_missing_noted {
                    notes.push(
                        "ffprobe wasn't available, so audio/video files weren't probe-checked this run."
                            .to_string(),
                    );
                    probe_missing_noted = true;
                }
                continue;
            }
            match probe.probe(&e.path) {
                Some(r) if !r.readable => {
                    out.push(Candidate {
                        path: e.path_str(),
                        family: "corrupt",
                        lane: "certain",
                        class_key: "corrupt:probe-failed".into(),
                        title: "Media file can't be opened".into(),
                        detail: "ffprobe couldn't read any playable stream from it — it's likely truncated or damaged.".into(),
                        proposal: prop_none(),
                    });
                }
                _ => {}
            }
        }
    }
    (out, notes)
}

// ---------------------------------------------------------------------------
// 4. COMPATIBILITY (opt-in) — names that can't travel.
// ---------------------------------------------------------------------------

const RESERVED_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
    "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

fn compat_clean(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        match c {
            '<' | '>' | ':' | '\\' | '|' => out.push('-'),
            '"' | '?' | '*' => {}
            c if (c as u32) < 0x20 || (c as u32) == 0x7f => {}
            c => out.push(c),
        }
    }
    out.trim_end_matches(['.', ' ']).to_string()
}

fn detect_compat(tree: &ScanTree, by_parent: &HashMap<PathBuf, Vec<usize>>) -> Vec<Candidate> {
    let mut out = Vec::new();
    for e in &tree.entries {
        if e.is_symlink {
            continue;
        }
        let name = &e.name;
        let mut issues: Vec<&str> = Vec::new();
        if name.chars().any(|c| "<>:\"\\|?*".contains(c)) {
            issues.push("characters Windows forbids (< > : \" \\ | ? *)");
        }
        if name.chars().any(|c| (c as u32) < 0x20 || (c as u32) == 0x7f) {
            issues.push("control characters");
        }
        if name.ends_with('.') || name.ends_with(' ') {
            issues.push("a trailing dot or space (Windows strips them, changing the name)");
        }
        let base = e.stem().to_uppercase();
        let reserved = RESERVED_NAMES.contains(&base.as_str());
        if reserved {
            issues.push("a reserved Windows device name");
        }
        if issues.is_empty() {
            continue;
        }
        let mut fixed = compat_clean(name);
        if reserved {
            // CON.txt → CON_.txt (stem gains an underscore), then
            // re-cleaned so any other issue in the name is fixed in
            // the same proposal.
            let stem = e.stem();
            let with_fix = match e.path.extension() {
                Some(ext) if !e.is_dir => format!("{}_.{}", stem, ext.to_string_lossy()),
                _ => format!("{stem}_"),
            };
            fixed = compat_clean(&with_fix);
        }
        if fixed.is_empty() || fixed == *name {
            continue;
        }
        out.push(Candidate {
            path: e.path_str(),
            family: "compat",
            lane: "certain",
            class_key: "compat:illegal-name".into(),
            title: "This name can't travel to Windows or macOS".into(),
            detail: format!("It has {}. The nearest safe name is “{fixed}”.", issues.join(" and ")),
            proposal: prop_rename(&fixed),
        });
    }
    // Case-insensitive collisions within one folder.
    for (_parent, idxs) in by_parent {
        let mut seen: HashMap<String, Vec<usize>> = HashMap::new();
        for &i in idxs {
            let e = &tree.entries[i];
            if e.is_symlink {
                continue;
            }
            seen.entry(e.name.to_lowercase()).or_default().push(i);
        }
        for (_lower, group) in seen {
            if group.len() < 2 {
                continue;
            }
            let mut sorted: Vec<usize> = group.clone();
            sorted.sort_by_key(|&i| tree.entries[i].name.clone());
            for &i in sorted.iter().skip(1) {
                let e = &tree.entries[i];
                let fixed = collision_name(e);
                out.push(Candidate {
                    path: e.path_str(),
                    family: "compat",
                    lane: "certain",
                    class_key: "compat:case-collision".into(),
                    title: "Name collides on case-insensitive systems".into(),
                    detail: format!(
                        "Another item in this folder differs only by letter case — on Windows or a Mac they'd be the same file. Renaming this one to “{fixed}” keeps both."
                    ),
                    proposal: prop_rename(&fixed),
                });
            }
        }
    }
    out
}

/// "name (2).ext" in the style Windows itself uses.
fn collision_name(e: &EntryInfo) -> String {
    match e.path.extension() {
        Some(ext) if !e.is_dir => format!("{} (2).{}", e.stem(), ext.to_string_lossy()),
        _ => format!("{} (2)", e.stem()),
    }
}

// ---------------------------------------------------------------------------
// 5. SPACE — verified duplicates + honestly-labeled conversions.
// ---------------------------------------------------------------------------

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

const FULL_HASH_MAX: u64 = 1 << 30; // 1 GiB
const SPOT_CHUNK: u64 = 4 << 20; // 4 MiB

/// Content hash for duplicate verification, cached by
/// (path, size, mtime) so re-audits are cheap. Files up to 1 GiB
/// are hashed whole; larger files use a spot hash (size + head,
/// middle, tail chunks) — the method is DISCLOSED in the flag.
fn content_hash(
    catalog: &Catalog,
    e: &EntryInfo,
) -> Option<(String, &'static str)> {
    let mtime_str = e.mtime.map(rfc).unwrap_or_default();
    if let Ok(Some(h)) = catalog.auditor_hash(&e.path_str(), e.size, &mtime_str) {
        return Some((h, if e.size <= FULL_HASH_MAX { "full" } else { "spot" }));
    }
    use sha2::{Digest, Sha256};
    let mut f = std::fs::File::open(&e.path).ok()?;
    let mut hasher = Sha256::new();
    let method: &'static str;
    if e.size <= FULL_HASH_MAX {
        method = "full";
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = f.read(&mut buf).ok()?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
    } else {
        method = "spot";
        use std::io::{Seek, SeekFrom};
        hasher.update(e.size.to_be_bytes());
        for off in [0u64, e.size / 2, e.size.saturating_sub(SPOT_CHUNK)] {
            f.seek(SeekFrom::Start(off)).ok()?;
            let mut buf = vec![0u8; SPOT_CHUNK as usize];
            let mut read_total = 0usize;
            while read_total < buf.len() {
                let n = f.read(&mut buf[read_total..]).ok()?;
                if n == 0 {
                    break;
                }
                read_total += n;
            }
            buf.truncate(read_total);
            hasher.update(&buf);
        }
    }
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let _ = catalog.set_auditor_hash(&e.path_str(), e.size, &mtime_str, &hex);
    Some((hex, method))
}

/// Which copy of a duplicate set to keep: the most "settled" path —
/// never a Downloads/temp path when a library path exists, then
/// the shortest path, then alphabetical (stable and explainable).
fn keep_rank(path: &str) -> (u8, usize, String) {
    let transient = path.contains("/Downloads/")
        || path.contains("/tmp/")
        || path.contains("/.cache/")
        || path.contains("/Temp/");
    (transient as u8, path.len(), path.to_string())
}

fn detect_space(
    tree: &ScanTree,
    catalog: &Catalog,
    probe: &dyn MediaProbe,
    volumes: &[(String, String)],
) -> Vec<Candidate> {
    let mut out = Vec::new();
    // --- verified duplicates -------------------------------------
    let mut by_size: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, e) in tree.entries.iter().enumerate() {
        if !e.is_dir && !e.is_symlink && e.size > 0 {
            by_size.entry(e.size).or_default().push(i);
        }
    }
    for (size, idxs) in by_size {
        if idxs.len() < 2 {
            continue;
        }
        let mut by_hash: HashMap<(String, &'static str), Vec<usize>> = HashMap::new();
        for i in idxs {
            let e = &tree.entries[i];
            if let Some(h) = content_hash(catalog, e) {
                by_hash.entry(h).or_default().push(i);
            }
        }
        for ((hash, method), group) in by_hash {
            if group.len() < 2 {
                continue;
            }
            let _ = hash;
            let mut members: Vec<&EntryInfo> =
                group.iter().map(|&i| &tree.entries[i]).collect();
            members.sort_by_key(|e| keep_rank(&e.path_str()));
            let keep = members[0];
            let keep_label = volume_label(volumes, &keep.path_str());
            for dup in members.iter().skip(1) {
                let dup_label = volume_label(volumes, &dup.path_str());
                let method_note = if method == "full" {
                    "SHA-256 of the full contents matches".to_string()
                } else {
                    "size + head/middle/tail spot hash matches (huge file — not hashed end to end)".to_string()
                };
                out.push(Candidate {
                    path: dup.path_str(),
                    family: "space",
                    lane: "certain",
                    class_key: "space:duplicate".into(),
                    title: format!("Duplicate of “{}”", keep.name),
                    detail: format!(
                        "{method_note}. Keeping the copy at {} ({}) frees {} here ({}). Same file, two places — one can go.",
                        keep.path_str(),
                        keep_label,
                        human_bytes(size),
                        dup_label
                    ),
                    proposal: prop_trash(),
                });
            }
        }
    }
    // --- conversions ----------------------------------------------
    for e in &tree.entries {
        if e.is_dir || e.is_symlink {
            continue;
        }
        let ext = e.ext();
        // WAV → FLAC: lossless.
        if (ext == "wav" || ext == "wave") && e.size >= 1 << 20 {
            if probe.available() {
                match probe.probe(&e.path) {
                    Some(r) => {
                        let codec = r.audio_codec.unwrap_or_default();
                        if !codec.starts_with("pcm") {
                            continue; // not PCM WAV — don't guess
                        }
                    }
                    None => continue,
                }
            }
            let est_after = e.size * 55 / 100;
            let dst = format!("{}.flac", e.stem());
            out.push(Candidate {
                path: e.path_str(),
                family: "space",
                lane: "certain",
                class_key: "space:convert-flac".into(),
                title: "Convert WAV to FLAC (lossless)".into(),
                detail: format!(
                    "FLAC is lossless — the audio comes back bit-for-bit identical. Estimated saving: about {} (a typical FLAC lands near 55% of the WAV; the job reports the true before/after sizes).",
                    human_bytes(e.size - est_after)
                ),
                proposal: serde_json::json!({
                    "kind": "convert", "convertKind": "flac",
                    "dstName": dst, "replace": true,
                    "estSaving": e.size - est_after
                }),
            });
            continue;
        }
        // H.264 → H.265: a RE-ENCODE, labeled as one.
        if is_video(e) && e.size >= 50 * (1 << 20) && probe.available() {
            if let Some(r) = probe.probe(&e.path) {
                if r.video_codec.as_deref() == Some("h264") {
                    let est_saving = e.size / 2;
                    let dst = if ext == "mkv" {
                        format!("{} (H.265).mkv", e.stem())
                    } else {
                        format!("{}.mkv", e.stem())
                    };
                    let replaces = ext == "mkv"
                        || VIDEO_EXTS.contains(&ext.as_str());
                    let _ = replaces;
                    out.push(Candidate {
                        path: e.path_str(),
                        family: "space",
                        lane: "question",
                        class_key: "space:convert-hevc".into(),
                        title: "Re-encode H.264 to H.265?".into(),
                        detail: format!(
                            "This is a RE-ENCODE, not a lossless conversion: the picture is decoded and re-compressed, so it's a generational copy — most people can't tell, but it isn't bit-identical, and it can't be undone. Estimated saving: about {} (half is typical; the job reports the true before/after sizes).",
                            human_bytes(est_saving)
                        ),
                        proposal: serde_json::json!({
                            "kind": "convert", "convertKind": "hevc",
                            "dstName": dst, "replace": true,
                            "estSaving": est_saving
                        }),
                    });
                }
            }
            continue;
        }
        // BMP/TIFF → PNG: lossless.
        if (ext == "bmp" || ext == "tif" || ext == "tiff") && e.size >= 200 * 1024 {
            let dst = format!("{}.png", e.stem());
            out.push(Candidate {
                path: e.path_str(),
                family: "space",
                lane: "certain",
                class_key: "space:convert-png".into(),
                title: "Convert to PNG (lossless)".into(),
                detail: "PNG compresses losslessly — same pixels, usually much smaller than BMP/TIFF. The saving varies too much to estimate honestly; the job reports the true before/after sizes.".into(),
                proposal: serde_json::json!({
                    "kind": "convert", "convertKind": "png",
                    "dstName": dst, "replace": true
                }),
            });
        }
    }
    out
}

/// Volume label for a path, from the catalog's known volumes
/// (longest mount-prefix match; "this computer" when unknown).
fn volume_label(volumes: &[(String, String)], path: &str) -> String {
    let mut best: Option<&(String, String)> = None;
    for v in volumes {
        if path == v.0 || path.starts_with(&format!("{}/", v.0.trim_end_matches('/'))) {
            if best.map(|b| v.0.len() > b.0.len()).unwrap_or(true) {
                best = Some(v);
            }
        }
    }
    best.map(|v| v.1.clone())
        .unwrap_or_else(|| "this computer".to_string())
}

/// (mount prefix, label) pairs from the catalog's volume memory.
fn volume_prefixes(catalog: &Catalog) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Ok(vols) = catalog.volumes() {
        for v in vols {
            let label = v
                .label
                .as_deref()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .unwrap_or_else(|| "a drive".to_string());
            for m in &v.mount_history {
                out.push((m.path.clone(), label.clone()));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 6. JUNK / PROMO — signatures + folder context.
// ---------------------------------------------------------------------------

fn folder_is_video_majority(
    by_parent: &HashMap<PathBuf, Vec<usize>>,
    tree: &ScanTree,
    parent: &Path,
) -> bool {
    let Some(idxs) = by_parent.get(parent) else {
        return false;
    };
    let files: Vec<&EntryInfo> = idxs
        .iter()
        .map(|&i| &tree.entries[i])
        .filter(|e| !e.is_dir)
        .collect();
    if files.is_empty() {
        return false;
    }
    let vids = files.iter().filter(|e| is_video(e)).count();
    vids * 2 > files.len()
}

fn detect_junk(tree: &ScanTree, by_parent: &HashMap<PathBuf, Vec<usize>>) -> Vec<Candidate> {
    let mut out = Vec::new();
    for e in &tree.entries {
        if e.is_dir || e.is_symlink {
            continue;
        }
        let lower = e.name.to_lowercase();
        let ext = e.ext();
        let push = |out: &mut Vec<Candidate>, class: &str, title: &str, detail: String| {
            out.push(Candidate {
                path: e.path_str(),
                family: "junk",
                lane: "certain",
                class_key: class.to_string(),
                title: title.to_string(),
                detail,
                proposal: prop_trash(),
            });
        };
        // --- signatures (certain) ---
        // Uploader brands recognized here (yts, rarbg, …) are the
        // same vocabulary as NEUTRAL_TOKENS above: the junk family
        // matches them as name signatures, the typo family excludes
        // them as known tokens. One vocabulary, two mechanisms.
        if IMAGE_EXTS.contains(&ext.as_str())
            && (lower.contains("yts.")
                || lower.starts_with("yts")
                || lower.contains("yts.mx")
                || lower.contains("yts.lt"))
        {
            push(&mut out, "junk:yts-image", "Download-site ad image",
                "This is one of the ad images a torrent site packs into its releases — not part of your movie.".into());
            continue;
        }
        if lower.contains("rarbg") && IMAGE_EXTS.contains(&ext.as_str())
            || lower.starts_with("rarbg")
        {
            push(&mut out, "junk:rarbg", "Download-site branding",
                "Uploader branding packed into a release — not part of your media.".into());
            continue;
        }
        if lower.starts_with("donotmirror") || lower == "do not mirror.txt" {
            push(&mut out, "junk:donotmirror", "“DoNotMirror” promo file",
                "A promo notice download sites slip into releases so mirrors keep their ads attached.".into());
            continue;
        }
        if lower.contains("downloaded from") || lower.contains("downloaded via") {
            push(&mut out, "junk:downloaded-from", "“Downloaded from…” notice",
                "A credit notice for the site this came from — safe to throw away.".into());
            continue;
        }
        // Small text files get a content sniff for the same notices.
        if ext == "txt" && e.size > 0 && e.size <= 8192 {
            if let Ok(content) = std::fs::read_to_string(&e.path) {
                let c = content.to_lowercase();
                if (c.contains("downloaded from") || c.contains("downloaded via"))
                    && (c.contains("torrent") || c.contains(".com") || c.contains(".io"))
                {
                    push(&mut out, "junk:downloaded-from", "“Downloaded from…” notice",
                        "This little text file is a credit notice for the site this came from.".into());
                    continue;
                }
            }
        }
        // .url ads: certain inside media folders, a question elsewhere.
        if ext == "url" {
            if folder_is_video_majority(by_parent, tree, &e.parent) {
                push(&mut out, "junk:url-media", "Website shortcut among videos",
                    "A .url shortcut is an ad for the site this came from, not a video.".into());
            } else {
                out.push(Candidate {
                    path: e.path_str(),
                    family: "junk",
                    lane: "question",
                    class_key: "junk:url".into(),
                    title: "Website shortcut — keep it?".into(),
                    detail: ".url files are usually site ads slipped into downloads, but sometimes they're bookmarks you made.".into(),
                    proposal: prop_trash(),
                });
            }
            continue;
        }
        // --- folder context (question) ---
        if ["exe", "com", "bat", "scr"].contains(&ext.as_str())
            && folder_is_video_majority(by_parent, tree, &e.parent)
        {
            out.push(Candidate {
                path: e.path_str(),
                family: "junk",
                lane: "question",
                class_key: "junk:exe-in-video-folder".into(),
                title: "A program file among videos — intentional?".into(),
                detail: "Executables tucked into video folders are usually download-site junk (or worse). If you didn't put it there on purpose, trash it.".into(),
                proposal: prop_trash(),
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 7. CLUTTER — empties, stale partials, OS junk, chains, orphans…
// ---------------------------------------------------------------------------

const OS_JUNK_NAMES: [&str; 6] = [
    ".ds_store", "thumbs.db", "ehthumbs.db", "desktop.ini", ".directory", ".appledouble",
];
const PARTIAL_EXTS: [&str; 8] = [
    "part", "crdownload", "download", "tmp", "temp", "!ut", "bc!", "opdownload",
];
const SIDECAR_EXTS: [&str; 10] = [
    "srt", "sub", "idx", "nfo", "ass", "ssa", "vtt", "jpg", "jpeg", "png",
];
const MEDIA_STEM_EXTS: [&str; 22] = [
    "mkv", "mp4", "avi", "mov", "wmv", "webm", "m4v", "ts", "m2ts", "mpg", "mpeg", "flv",
    "mp3", "flac", "wav", "ogg", "opus", "m4a", "epub", "pdf", "cbz", "cbr",
];
const KEEP_SIDECAR_NAMES: [&str; 8] = [
    "poster", "fanart", "folder", "cover", "backdrop", "banner", "logo", "thumb",
];
const ARCHIVE_EXTS: [&str; 7] = ["zip", "rar", "7z", "tar", "tgz", "tbz2", "txz"];
const INSTALLER_EXTS: [&str; 5] = ["dmg", "iso", "img", "msi", "pkg"];

fn detect_clutter(
    tree: &ScanTree,
    by_parent: &HashMap<PathBuf, Vec<usize>>,
    settings: &AuditorSettings,
    target: &Path,
) -> Vec<Candidate> {
    let mut out = Vec::new();
    // Empty folders: a directory whose subtree holds no files at
    // all. Only the TOPMOST such folder is flagged (its children go
    // with it when trashed).
    {
        let mut files_under: HashMap<PathBuf, usize> = HashMap::new();
        for e in &tree.entries {
            if !e.is_dir {
                let mut p = e.parent.clone();
                loop {
                    *files_under.entry(p.clone()).or_insert(0) += 1;
                    if p == target || !p.pop() {
                        break;
                    }
                }
            }
        }
        let mut empty_dirs: Vec<&EntryInfo> = tree
            .entries
            .iter()
            .filter(|e| e.is_dir && !e.is_symlink)
            .filter(|e| files_under.get(&e.path).copied().unwrap_or(0) == 0)
            .collect();
        empty_dirs.sort_by_key(|e| e.path.components().count());
        let mut flagged: Vec<PathBuf> = Vec::new();
        for d in empty_dirs {
            if flagged.iter().any(|f| d.path.starts_with(f)) {
                continue;
            }
            flagged.push(d.path.clone());
            out.push(Candidate {
                path: d.path_str(),
                family: "clutter",
                lane: "certain",
                class_key: "clutter:empty-folder".into(),
                title: "Empty folder".into(),
                detail: "There's nothing in this folder or anything under it.".into(),
                proposal: prop_trash(),
            });
        }
        // Empty entries (files are the corrupt family's zero-byte;
        // here: nothing more — files covered there).
        let _ = by_parent;
    }
    for e in &tree.entries {
        if e.is_symlink {
            continue;
        }
        let lower = e.name.to_lowercase();
        let ext = e.ext();
        if !e.is_dir {
            // OS junk.
            if OS_JUNK_NAMES.contains(&lower.as_str()) || e.name.starts_with("._") {
                out.push(Candidate {
                    path: e.path_str(),
                    family: "clutter",
                    lane: "certain",
                    class_key: "clutter:os-junk".into(),
                    title: "Operating-system junk".into(),
                    detail: "This is a housekeeping file another OS left behind (.DS_Store, Thumbs.db, AppleDouble…). It holds nothing of yours.".into(),
                    proposal: prop_trash(),
                });
                continue;
            }
            // Stale partials / temp debris.
            let age = age_days(e.mtime).unwrap_or(0);
            if PARTIAL_EXTS.contains(&ext.as_str()) && age > settings.stale_days as u64 {
                out.push(Candidate {
                    path: e.path_str(),
                    family: "clutter",
                    lane: "certain",
                    class_key: "clutter:stale-partial".into(),
                    title: "Stale partial download".into(),
                    detail: format!(
                        "This looks like an unfinished download or temp file, untouched for {age} days."
                    ),
                    proposal: prop_trash(),
                });
                continue;
            }
            // "Copy of" / "(1)" chains — a question, no presumption.
            let stem_l = e.stem().to_lowercase();
            let copyish = stem_l.starts_with("copy of ")
                || stem_l.ends_with(" - copy")
                || stem_l.ends_with(" copy")
                || (stem_l.ends_with(')')
                    && stem_l
                        .rfind(" (")
                        .map(|p| {
                            stem_l[p + 2..stem_l.len() - 1]
                                .chars()
                                .all(|c| c.is_ascii_digit())
                                && stem_l.len() - p > 3
                        })
                        .unwrap_or(false));
            if copyish {
                out.push(Candidate {
                    path: e.path_str(),
                    family: "clutter",
                    lane: "question",
                    class_key: "clutter:copy-chain".into(),
                    title: "Looks like a stray duplicate copy".into(),
                    detail: "The name follows the “Copy of…” / “(1)” pattern systems make when a file is duplicated in place. If the original is here too, this one may be redundant — check before trashing.".into(),
                    proposal: prop_none(),
                });
                continue;
            }
            // Orphaned sidecars: subtitle/art/nfo whose stem matches
            // nothing playable in the folder.
            if SIDECAR_EXTS.contains(&ext.as_str())
                && !KEEP_SIDECAR_NAMES.contains(&e.stem().to_lowercase().as_str())
            {
                let stems: HashSet<String> = by_parent
                    .get(&e.parent)
                    .map(|idxs| {
                        idxs.iter()
                            .map(|&i| &tree.entries[i])
                            .filter(|o| MEDIA_STEM_EXTS.contains(&o.ext().as_str()))
                            .map(|o| o.stem().to_lowercase())
                            .collect()
                    })
                    .unwrap_or_default();
                let my_stem = e.stem().to_lowercase();
                // Language-suffixed subtitles: movie.en.srt → movie.
                let base_stem = my_stem
                    .rsplit_once('.')
                    .filter(|(_, lang)| lang.len() >= 2 && lang.len() <= 3)
                    .map(|(b, _)| b.to_string())
                    .unwrap_or_else(|| my_stem.clone());
                if !stems.contains(&my_stem) && !stems.contains(&base_stem) {
                    out.push(Candidate {
                        path: e.path_str(),
                        family: "clutter",
                        lane: "certain",
                        class_key: "clutter:orphan-sidecar".into(),
                        title: "Orphaned companion file".into(),
                        detail: "This subtitle/art/info file's name matches nothing playable in its folder — the media it belonged to is gone.".into(),
                        proposal: prop_trash(),
                    });
                    continue;
                }
            }
            // Archive + extracted-folder pair.
            if ARCHIVE_EXTS.contains(&ext.as_str()) {
                let has_dir = by_parent.get(&e.parent).map(|idxs| {
                    idxs.iter().any(|&i| {
                        let o = &tree.entries[i];
                        o.is_dir && o.name.to_lowercase() == e.stem().to_lowercase()
                    })
                }).unwrap_or(false);
                if has_dir {
                    out.push(Candidate {
                        path: e.path_str(),
                        family: "clutter",
                        lane: "question",
                        class_key: "clutter:archive-pair".into(),
                        title: "Archive next to its extracted folder".into(),
                        detail: "Both this archive and a folder with its name are here — usually the archive can go once you've checked the extraction is complete.".into(),
                        proposal: prop_none(),
                    });
                    continue;
                }
            }
            // Stale installers.
            if INSTALLER_EXTS.contains(&ext.as_str())
                && e.size > 50 * (1 << 20)
                && age_days(e.mtime).unwrap_or(0) > 90
            {
                out.push(Candidate {
                    path: e.path_str(),
                    family: "clutter",
                    lane: "question",
                    class_key: "clutter:stale-installer".into(),
                    title: "Old installer image — still needed?".into(),
                    detail: format!(
                        "A {} installer untouched for over 90 days. If the software is installed (or moved on), this is dead weight.",
                        human_bytes(e.size)
                    ),
                    proposal: prop_trash(),
                });
                continue;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 8. STYLE DRIFT — folder-relative, never global taste.
// ---------------------------------------------------------------------------

fn case_class(stem: &str) -> &'static str {
    let alpha: Vec<char> = stem.chars().filter(|c| c.is_alphabetic()).collect();
    if alpha.is_empty() {
        return "none";
    }
    if alpha.iter().all(|c| !c.is_lowercase()) {
        return "upper";
    }
    if alpha.iter().all(|c| !c.is_uppercase()) {
        return "lower";
    }
    let first = alpha[0];
    if first.is_uppercase() && alpha.iter().skip(1).all(|c| !c.is_uppercase()) {
        return "title";
    }
    "mixed"
}

fn detect_style(tree: &ScanTree, by_parent: &HashMap<PathBuf, Vec<usize>>) -> Vec<Candidate> {
    let mut out = Vec::new();
    for (_parent, idxs) in by_parent {
        let files: Vec<&EntryInfo> = idxs
            .iter()
            .map(|&i| &tree.entries[i])
            .filter(|e| !e.is_dir && !e.is_symlink)
            .collect();
        if files.len() < 5 {
            continue;
        }
        // Dominant case convention in THIS folder.
        let mut case_counts: HashMap<&'static str, usize> = HashMap::new();
        for f in &files {
            let c = case_class(&f.stem());
            if c != "none" && c != "mixed" {
                *case_counts.entry(c).or_insert(0) += 1;
            }
        }
        let scored: usize = case_counts.values().sum();
        if scored >= 5 {
            if let Some((&dom, &n)) = case_counts.iter().max_by_key(|(_, &n)| n) {
                if (n as f64) / (scored as f64) >= 0.6 {
                    let mut flagged = 0;
                    for f in &files {
                        if flagged >= 20 {
                            break;
                        }
                        let c = case_class(&f.stem());
                        if c == "none" || c == "mixed" || c == dom {
                            continue;
                        }
                        if (case_counts.get(c).copied().unwrap_or(0) as f64)
                            / (scored as f64)
                            > 0.25
                        {
                            continue; // a real sub-convention, not drift
                        }
                        // Only unambiguous transforms get a
                        // rename proposal (upper/lower); anything
                        // else is asked about without one.
                        let proposal = match dom {
                            "upper" => {
                                let name = rename_with_stem(f, &f.stem().to_uppercase());
                                prop_rename(&name)
                            }
                            "lower" => {
                                let name = rename_with_stem(f, &f.stem().to_lowercase());
                                prop_rename(&name)
                            }
                            _ => prop_none(),
                        };
                        flagged += 1;
                        out.push(Candidate {
                            path: f.path_str(),
                            family: "style",
                            lane: "question",
                            class_key: "style:case".into(),
                            title: "Breaks this folder's naming style".into(),
                            detail: format!(
                                "Most names in this folder are {}-case; this one is {}-case. Only this folder's habit counts — a folder that's consistently one style gets no flags.",
                                dom, c
                            ),
                            proposal,
                        });
                    }
                }
            }
        }
        // Dominant separator convention.
        let seps = ['_', '-', ' ', '.'];
        let mut sep_counts: HashMap<char, usize> = HashMap::new();
        for f in &files {
            for s in seps {
                if f.stem().contains(s) {
                    *sep_counts.entry(s).or_insert(0) += 1;
                }
            }
        }
        if let Some((&dom_sep, &n)) = sep_counts.iter().max_by_key(|(_, &n)| n) {
            if n >= 5 && (n as f64) / (files.len() as f64) >= 0.6 {
                let mut flagged = 0;
                for f in &files {
                    if flagged >= 20 {
                        break;
                    }
                    let stem = f.stem();
                    if stem.contains(dom_sep) {
                        continue;
                    }
                    // A file using exactly one OTHER separator
                    // consistently is a separator outlier.
                    let others: Vec<char> = seps
                        .iter()
                        .copied()
                        .filter(|&s| s != dom_sep && stem.contains(s))
                        .collect();
                    if others.len() != 1 {
                        continue;
                    }
                    let fixed_stem: String = stem
                        .chars()
                        .map(|c| if c == others[0] { dom_sep } else { c })
                        .collect();
                    let new_name = rename_with_stem(f, &fixed_stem);
                    flagged += 1;
                    out.push(Candidate {
                        path: f.path_str(),
                        family: "style",
                        lane: "question",
                        class_key: "style:separator".into(),
                        title: "Breaks this folder's separator style".into(),
                        detail: format!(
                            "Most names here use “{dom_sep}” between words; this one uses “{}”. The folder's form would be “{new_name}”.",
                            others[0]
                        ),
                        proposal: prop_rename(&new_name),
                    });
                }
            }
        }
    }
    out
}

fn rename_with_stem(e: &EntryInfo, new_stem: &str) -> String {
    match e.path.extension() {
        Some(ext) => format!("{}.{}", new_stem, ext.to_string_lossy()),
        None => new_stem.to_string(),
    }
}

// ---------------------------------------------------------------------------
// 9. NEAR-DUPLICATE EDITIONS — suggest only.
// ---------------------------------------------------------------------------

fn detect_editions(tree: &ScanTree) -> Vec<Candidate> {
    use ufm_media::videoid::{parse_video_filename, ParsedKind};
    let mut groups: HashMap<(String, Option<u32>), Vec<&EntryInfo>> = HashMap::new();
    for e in &tree.entries {
        if e.is_dir || e.is_symlink || !is_video(e) {
            continue;
        }
        let p = parse_video_filename(&e.name);
        if p.kind != ParsedKind::Movie || p.title.is_empty() {
            continue;
        }
        groups
            .entry((p.title.to_lowercase(), p.year))
            .or_default()
            .push(e);
    }
    let mut out = Vec::new();
    let mut sorted_groups: Vec<_> = groups.into_iter().collect();
    sorted_groups.sort_by(|a, b| a.0.cmp(&b.0));
    for ((title, year), members) in sorted_groups {
        if members.len() < 2 {
            continue;
        }
        let mut biggest = members[0];
        for m in &members {
            if m.size > biggest.size {
                biggest = m;
            }
        }
        let display = match year {
            Some(y) => format!("{} ({})", capitalize(&title), y),
            None => capitalize(&title),
        };
        let list = members
            .iter()
            .map(|m| format!("{} ({})", m.path_str(), human_bytes(m.size)))
            .collect::<Vec<_>>()
            .join("; ");
        out.push(Candidate {
            path: biggest.path_str(),
            family: "editions",
            lane: "question",
            class_key: format!("editions:{title}"),
            title: format!("{display} appears {} times", members.len()),
            detail: format!(
                "Several copies or editions of the same film: {list}. Different cuts and quality tiers are legitimate — this is a consolidation note, not an accusation."
            ),
            proposal: prop_none(),
        });
    }
    out
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

// ---------------------------------------------------------------------------
// 10. COLD STORAGE — suggest-only report.
// ---------------------------------------------------------------------------

const COLD_FLAG_CAP: usize = 25;

fn detect_cold(tree: &ScanTree, settings: &AuditorSettings) -> Vec<Candidate> {
    let threshold_days = settings.cold_years as u64 * 365;
    let mut cold: Vec<&EntryInfo> = tree
        .entries
        .iter()
        .filter(|e| !e.is_dir && !e.is_symlink)
        .filter(|e| age_days(e.mtime).unwrap_or(0) > threshold_days)
        .collect();
    cold.sort_by_key(|e| std::cmp::Reverse(e.size));
    cold.iter()
        .take(COLD_FLAG_CAP)
        .map(|e| {
            let years = age_days(e.mtime).unwrap_or(0) / 365;
            Candidate {
                path: e.path_str(),
                family: "cold",
                lane: "question",
                class_key: "cold:untouched".into(),
                title: format!("Untouched for about {years} years"),
                detail: format!(
                    "This {} file hasn't been modified in over {} year(s). A candidate for cold storage (an external drive, an archive folder) — a suggestion only; nothing moves without you.",
                    human_bytes(e.size),
                    settings.cold_years
                ),
                proposal: prop_none(),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The run engine: walk → detect → remember-filter → checklist.
// ---------------------------------------------------------------------------

pub struct RunRequest {
    pub target: String,
    pub schedule_id: Option<String>,
    pub schedule_name: String,
    pub late: bool,
}

/// Run one audit now, synchronously (callers put it on a blocking
/// thread / the agent process). GUI-free by construction — every
/// input arrives as a parameter, every output lands in the catalog.
pub fn run_audit(
    catalog: &Catalog,
    req: &RunRequest,
    settings: &AuditorSettings,
    probe: &dyn MediaProbe,
) -> Result<AuditorRunRow, String> {
    let root = PathBuf::from(&req.target);
    if !root.is_dir() {
        return Err(format!("audit target is not a folder: {}", req.target));
    }
    let started = now_local();
    let mut row = AuditorRunRow {
        id: uuid::Uuid::new_v4().to_string(),
        schedule_id: req.schedule_id.clone(),
        schedule_name: req.schedule_name.clone(),
        target: req.target.clone(),
        started: stamp(started),
        finished: None,
        was_late: req.late,
        status: "running".into(),
        summary_json: "{}".into(),
    };
    catalog
        .upsert_auditor_run(&row)
        .map_err(|e| format!("audit run could not be recorded: {e}"))?;

    let tree = walk(&root);
    let by_parent = children_by_parent(&tree);
    let words = effective_words(catalog);
    let volumes = volume_prefixes(catalog);
    let fams = &settings.families;

    let mut candidates: Vec<Candidate> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    if fams.typo {
        candidates.extend(detect_typos(&tree, &words));
    }
    if fams.pattern {
        candidates.extend(detect_patterns(&tree, &by_parent));
    }
    if fams.corrupt {
        let (c, n) = detect_corrupt(&tree, probe);
        candidates.extend(c);
        notes.extend(n);
    }
    if fams.compat {
        candidates.extend(detect_compat(&tree, &by_parent));
    }
    if fams.space {
        candidates.extend(detect_space(&tree, catalog, probe, &volumes));
    }
    if fams.junk {
        candidates.extend(detect_junk(&tree, &by_parent));
    }
    if fams.clutter {
        candidates.extend(detect_clutter(&tree, &by_parent, settings, &root));
    }
    if fams.style {
        candidates.extend(detect_style(&tree, &by_parent));
    }
    if fams.editions {
        candidates.extend(detect_editions(&tree));
    }
    if fams.cold {
        candidates.extend(detect_cold(&tree, settings));
    }
    if tree.truncated {
        notes.push(format!(
            "This audit stopped at {} entries — the target is bigger than one pass covers, so some corners weren't checked.",
            WALK_CAP
        ));
    }

    // Memory filter: dismissals and standing rules are applied
    // BEFORE anything is written — remembered answers never
    // re-flag, ever.
    let dismissals: HashSet<(String, String)> = catalog
        .auditor_dismissals()
        .unwrap_or_default()
        .into_iter()
        .map(|(p, f, _)| (p, f))
        .collect();
    let rules: HashMap<String, String> = catalog
        .auditor_rules()
        .unwrap_or_default()
        .into_iter()
        .map(|(k, d, _)| (k, d))
        .collect();
    let key_of = |path: &str, family: &str, class_key: &str| {
        (path.to_string(), family.to_string(), class_key.to_string())
    };
    // Memory filter FIRST: what survives is what this run stands
    // behind. Supersede then compares against the survivors — so a
    // dismissal or standing rule also retires a stale open flag
    // left over from before the answer was given.
    let accepted: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| {
            if dismissals.contains(&(c.path.clone(), c.family.to_string())) {
                return false;
            }
            match rules.get(&c.class_key).map(|d| d.as_str()) {
                Some("suppress") | Some("accept") => false,
                _ => true,
            }
        })
        .collect();
    let existing = catalog
        .open_auditor_flags_under(&req.target)
        .unwrap_or_default();
    let accepted_keys: HashSet<(String, String, String)> = accepted
        .iter()
        .map(|c| key_of(&c.path, c.family, &c.class_key))
        .collect();
    let existing_keys: HashSet<(String, String, String)> = existing
        .iter()
        .map(|f| key_of(&f.path, &f.family, &f.class_key))
        .collect();
    let now = stamp(now_local());
    let mut superseded = 0usize;
    for f in &existing {
        if !accepted_keys.contains(&key_of(&f.path, &f.family, &f.class_key)) {
            if catalog
                .set_auditor_flag_state(&f.id, "superseded", &now)
                .is_ok()
            {
                superseded += 1;
            }
        } else if f.lane == "question"
            && rules.get(&f.class_key).map(|d| d.as_str()) == Some("fix")
        {
            // Carried flag under a "fix" standing rule (Fix All
            // Similar): it stops being a question — it is a fact
            // with the fix proposed, from now on.
            let _ = catalog.set_auditor_flag_lane(&f.id, "certain", &now);
        }
    }

    let mut inserted = 0usize;
    let mut carried = 0usize;
    let mut per_family: HashMap<&'static str, usize> = HashMap::new();
    for c in &accepted {
        if existing_keys.contains(&key_of(&c.path, c.family, &c.class_key)) {
            carried += 1;
            *per_family.entry(c.family).or_insert(0) += 1;
            continue;
        }
        // A "fix" standing rule (recorded by Fix All Similar)
        // promotes the match: it surfaces as a FACT with the fix
        // proposed — never silently applied, never re-asked.
        let lane = if c.lane == "question"
            && rules.get(&c.class_key).map(|d| d.as_str()) == Some("fix")
        {
            "certain"
        } else {
            c.lane
        };
        let flag = AuditorFlagRow {
            id: uuid::Uuid::new_v4().to_string(),
            run_id: row.id.clone(),
            path: c.path.clone(),
            family: c.family.to_string(),
            lane: lane.to_string(),
            class_key: c.class_key.clone(),
            title: c.title.clone(),
            detail: c.detail.clone(),
            proposal_json: Some(c.proposal.to_string()),
            state: "open".into(),
            created: now.clone(),
            updated: now.clone(),
        };
        if catalog.insert_auditor_flag(&flag).is_ok() {
            inserted += 1;
            *per_family.entry(c.family).or_insert(0) += 1;
        }
    }

    let files = tree.entries.iter().filter(|e| !e.is_dir).count();
    let dirs = tree.entries.iter().filter(|e| e.is_dir).count();
    let summary = serde_json::json!({
        "scannedFiles": files,
        "scannedDirs": dirs,
        "truncated": tree.truncated,
        "notes": notes,
        "families": per_family,
        "flagsNew": inserted,
        "flagsCarried": carried,
        "flagsSuperseded": superseded,
        "flagsOpen": inserted + carried,
    });
    row.finished = Some(stamp(now_local()));
    row.status = "completed".into();
    row.summary_json = summary.to_string();
    catalog
        .upsert_auditor_run(&row)
        .map_err(|e| format!("audit summary could not be saved: {e}"))?;
    let _ = catalog.prune_auditor_runs(50);
    Ok(row)
}

// ---------------------------------------------------------------------------
// Resolution planning: what confirming a flag WOULD do (the command
// layer turns the plan into a journaled job).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum FixPlan {
    Rename { to_name: String },
    Trash,
    Convert { dst_name: String, kind: String, replace: bool },
    None,
}

pub fn plan_for(flag: &AuditorFlagRow) -> FixPlan {
    let Some(raw) = &flag.proposal_json else {
        return FixPlan::None;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        return FixPlan::None;
    };
    match v.get("kind").and_then(|k| k.as_str()) {
        Some("rename") => match v.get("to").and_then(|t| t.as_str()) {
            Some(to) if !to.is_empty() => FixPlan::Rename {
                to_name: to.to_string(),
            },
            _ => FixPlan::None,
        },
        Some("trash") => FixPlan::Trash,
        Some("convert") => {
            let dst = v
                .get("dstName")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string();
            let kind = v
                .get("convertKind")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string();
            if dst.is_empty() || kind.is_empty() {
                return FixPlan::None;
            }
            FixPlan::Convert {
                dst_name: dst,
                kind,
                replace: v.get("replace").and_then(|b| b.as_bool()).unwrap_or(false),
            }
        }
        _ => FixPlan::None,
    }
}

// ---------------------------------------------------------------------------
// Scheduler: one tick of the shared background agent. Converts our
// schedules into mediacomplete's tested state machine — never a
// second timing implementation.
// ---------------------------------------------------------------------------

pub fn agent_tick(base: &Path) {
    let settings_snapshot = load_settings(base);
    if settings_snapshot.schedules.iter().all(|s| !s.enabled) {
        return;
    }
    let catalog = match Catalog::open(&crate::mediacomplete::catalog_path()) {
        Ok(c) => c,
        Err(_) => return,
    };
    let mut settings = settings_snapshot.clone();
    let mut changed = false;
    let now = now_local();
    let probe = FfprobeProbe::new();
    for sched in settings.schedules.iter_mut().filter(|s| s.enabled) {
        let mc = sched.as_mc();
        let late = match crate::mediacomplete::evaluate(&mc, now) {
            crate::mediacomplete::Decision::Due { late } => late,
            crate::mediacomplete::Decision::NotDue => continue,
        };
        if crate::mediacomplete::defer_reason(&mc).is_some() {
            continue;
        }
        sched.last_started = Some(stamp(now));
        let req = RunRequest {
            target: sched.target.clone(),
            schedule_id: Some(sched.id.clone()),
            schedule_name: sched.name.clone(),
            late,
        };
        sched.last_status = Some(match run_audit(&catalog, &req, &settings_snapshot, &probe) {
            Ok(_) => "completed".to_string(),
            Err(e) => format!("failed: {e}"),
        });
        changed = true;
    }
    if changed {
        let _ = save_settings(base, &settings);
    }
}

#[cfg(test)]
mod tests;
