//! mediacomplete: Media Completion + the background scheduler
//! (Dickles, 2026-10-15) — the second and third thirds of the
//! completion trio (poster + subtitles) and the overnight machine
//! that runs the whole pipeline over a film drive while the house
//! sleeps.
//!
//! Laws (DESIGN.md):
//! - Artwork/subtitles are fetched ONLY for confirmed or
//!   Confident-tier identities — never for a guess. Placement never
//!   overwrites anything the user already has; skips are reported.
//!   In the app they ride the journaled job engine (FetchArtwork /
//!   FetchSubtitle steps); in a scheduled run every move and placed
//!   file is recorded in the run journal, which is what Undo reads.
//! - The background agent is a separate headless process
//!   (`fable-files --background`): autostarted at login via a
//!   systemd user unit the user controls in Preferences, quiet,
//!   never any UI. Missed runs CATCH UP on next start and the run
//!   record admits it was late.
//! - UNATTENDED AUTONOMY: a scheduled run processes confident
//!   matches end-to-end and queues Probable/Unknown items into the
//!   run's pending list — the morning confirmation checklist. It
//!   never, ever guesses at 3 AM.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use ufm_catalog::Catalog;

// ---------------------------------------------------------------------------
// Data location (mirrors the GUI's app-data layout so the headless
// agent and the GUI share one catalog + one settings file).
// ---------------------------------------------------------------------------

pub fn data_base() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".local/share/com.garrisonr.fablefiles/fable-files")
}

pub fn catalog_path() -> PathBuf {
    data_base().join("catalog.db")
}

fn settings_path(base: &Path) -> PathBuf {
    base.join("mediacomplete-settings.json")
}

// ---------------------------------------------------------------------------
// Settings + schedules (JSON sidecar, atomic writes — rommedia pattern)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleSteps {
    pub organize: bool,
    pub artwork: bool,
    pub subtitles: bool,
}

impl Default for ScheduleSteps {
    fn default() -> Self {
        Self {
            organize: true,
            artwork: true,
            subtitles: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Schedule {
    pub id: String,
    pub name: String,
    /// Drive or folder tree to complete.
    pub target: String,
    pub steps: ScheduleSteps,
    /// Local time of day, "HH:MM".
    pub time: String,
    /// Seven flags, Monday first. Empty/all-false = every day.
    #[serde(default)]
    pub weekdays: Vec<bool>,
    /// Only run when the PC has been idle at least this long
    /// (0 = no idle requirement).
    #[serde(default)]
    pub idle_minutes: u32,
    /// Only run on AC power (desktops with no battery read as on AC).
    #[serde(default)]
    pub require_on_ac: bool,
    pub enabled: bool,
    /// Runner bookkeeping (RFC 3339 local) — the state machine's
    /// memory across restarts.
    #[serde(default)]
    pub last_started: Option<String>,
    #[serde(default)]
    pub last_status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McSettings {
    /// Organize jobs also fetch artwork (default on).
    pub artwork_on_organize: bool,
    /// Include backdrops as fanart.jpg (default on).
    pub fanart: bool,
    /// Organize jobs also fetch subtitles (default off — downloads
    /// are quota'd; the user opts in knowing the allowance is real).
    pub subtitles_on_organize: bool,
    /// Preferred subtitle language code.
    pub subtitle_language: String,
    /// Background agent master switch (drives the systemd unit).
    pub agent_enabled: bool,
    #[serde(default)]
    pub schedules: Vec<Schedule>,
}

impl Default for McSettings {
    fn default() -> Self {
        Self {
            artwork_on_organize: true,
            fanart: true,
            subtitles_on_organize: false,
            subtitle_language: "en".into(),
            agent_enabled: false,
            schedules: Vec::new(),
        }
    }
}

pub fn load_settings(base: &Path) -> McSettings {
    std::fs::read_to_string(settings_path(base))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_settings(base: &Path, settings: &McSettings) -> Result<(), String> {
    std::fs::create_dir_all(base).map_err(|e| e.to_string())?;
    let tmp = base.join("mediacomplete-settings.json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, settings_path(base)).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Scheduler state machine (pure — the part that must never be wrong)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    NotDue,
    /// A run is owed. `late` when the start is more than ten
    /// minutes past the slot, or any catch-up after downtime.
    Due { late: bool },
}

fn parse_time(s: &str) -> Option<chrono::NaiveTime> {
    chrono::NaiveTime::parse_from_str(s, "%H:%M").ok()
}

fn day_enabled(sched: &Schedule, date: chrono::NaiveDate) -> bool {
    if sched.weekdays.len() != 7 || sched.weekdays.iter().all(|w| !*w) {
        return true;
    }
    use chrono::Datelike;
    let idx = date.weekday().num_days_from_monday() as usize;
    sched.weekdays.get(idx).copied().unwrap_or(true)
}

/// The latest scheduled slot at or before `now` that this schedule
/// has not yet started a run for — its owed slot, if any.
pub fn owed_slot(
    sched: &Schedule,
    now: chrono::NaiveDateTime,
) -> Option<chrono::NaiveDateTime> {
    if !sched.enabled {
        return None;
    }
    let time = parse_time(&sched.time)?;
    let last = sched
        .last_started
        .as_deref()
        .and_then(|s| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").ok());
    // Walk back at most 8 days: a machine off for a week still owes
    // exactly one catch-up run (the newest missed slot), not eight.
    for back in 0..8 {
        let date = now.date() - chrono::Duration::days(back);
        if !day_enabled(sched, date) {
            continue;
        }
        let slot = date.and_time(time);
        if slot > now {
            continue;
        }
        if let Some(l) = last {
            if l >= slot {
                return None; // newest slot already started — nothing owed
            }
        }
        return Some(slot);
    }
    None
}

pub fn evaluate(sched: &Schedule, now: chrono::NaiveDateTime) -> Decision {
    match owed_slot(sched, now) {
        None => Decision::NotDue,
        Some(slot) => Decision::Due {
            late: now - slot > chrono::Duration::minutes(10),
        },
    }
}

/// The next future slot (for "next run" display), searching forward.
pub fn next_slot(sched: &Schedule, now: chrono::NaiveDateTime) -> Option<chrono::NaiveDateTime> {
    if !sched.enabled {
        return None;
    }
    let time = parse_time(&sched.time)?;
    for fwd in 0..8 {
        let date = now.date() + chrono::Duration::days(fwd);
        if !day_enabled(sched, date) {
            continue;
        }
        let slot = date.and_time(time);
        if slot > now {
            return Some(slot);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Trigger conditions (thin, honest about what they measure)
// ---------------------------------------------------------------------------

/// AC power: reads /sys/class/power_supply. A machine with no AC
/// adapter node (a desktop) reads as on AC — the condition simply
/// doesn't apply to it.
pub fn on_ac() -> bool {
    let dir = Path::new("/sys/class/power_supply");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return true;
    };
    for e in entries.flatten() {
        // AC adapters report type "Mains" in sysfs.
        let is_mains = std::fs::read_to_string(e.path().join("type"))
            .map(|t| t.trim() == "Mains")
            .unwrap_or(false);
        if is_mains {
            if let Ok(v) = std::fs::read_to_string(e.path().join("online")) {
                return v.trim() == "1";
            }
        }
    }
    true
}

/// Idle minutes: xprintidle (milliseconds) when the tool exists;
/// otherwise a system-load fallback (1-minute load average below
/// 1.0 counts as idle for this purpose — the copy in Preferences
/// says exactly this). None = couldn't measure.
pub fn idle_minutes() -> Option<u32> {
    if let Ok(out) = std::process::Command::new("xprintidle").output() {
        if out.status.success() {
            if let Ok(ms) = String::from_utf8_lossy(&out.stdout).trim().parse::<u64>() {
                return Some((ms / 60000) as u32);
            }
        }
    }
    let load = std::fs::read_to_string("/proc/loadavg").ok()?;
    let one: f64 = load.split_whitespace().next()?.parse().ok()?;
    Some(if one < 1.0 { 9999 } else { 0 })
}

/// Why a due run is being deferred this tick, if it is.
pub fn defer_reason(sched: &Schedule) -> Option<String> {
    if sched.require_on_ac && !on_ac() {
        return Some("waiting for AC power".into());
    }
    if sched.idle_minutes > 0 {
        match idle_minutes() {
            Some(m) if m >= sched.idle_minutes => {}
            Some(m) => {
                return Some(format!(
                    "waiting for {} idle minutes (currently {m})",
                    sched.idle_minutes
                ))
            }
            None => return Some("idle time can't be measured right now".into()),
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Artwork: placement plan (pure) + fetch (shared by jobs and runs)
// ---------------------------------------------------------------------------

/// One artwork sidecar to place: (target path, TMDB image kind).
/// kind: "poster" | "fanart" | "season"
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtTarget {
    pub path: PathBuf,
    pub kind: &'static str,
}

/// Where artwork goes for a video, in the layout the servers read.
/// Movie: poster.jpg + fanart.jpg beside the file. Episode at
/// Show/Season NN/file: show poster + fanart at the show root,
/// season poster inside the Season folder. An episode NOT in a
/// Season folder gets just a poster beside it (flat layout).
pub fn artwork_targets(video: &Path, media_type: &str, fanart: bool) -> Vec<ArtTarget> {
    let mut out = Vec::new();
    let parent = video.parent().unwrap_or(Path::new("/"));
    if media_type == "episode" {
        let parent_name = parent
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if parent_name.to_lowercase().starts_with("season") {
            if let Some(show_root) = parent.parent() {
                out.push(ArtTarget {
                    path: show_root.join("poster.jpg"),
                    kind: "poster",
                });
                if fanart {
                    out.push(ArtTarget {
                        path: show_root.join("fanart.jpg"),
                        kind: "fanart",
                    });
                }
                out.push(ArtTarget {
                    path: parent.join("poster.jpg"),
                    kind: "season",
                });
                return out;
            }
        }
    }
    out.push(ArtTarget {
        path: parent.join("poster.jpg"),
        kind: "poster",
    });
    if fanart {
        out.push(ArtTarget {
            path: parent.join("fanart.jpg"),
            kind: "fanart",
        });
    }
    out
}

#[derive(Debug)]
pub enum FetchVerdict {
    /// Placed files (absolute paths).
    Done(Vec<String>),
    /// Nothing to do — with the honest reason.
    Skipped(String),
    Failed(String),
}

fn download_bytes(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>, String> {
    let resp = agent
        .get(url)
        .set("User-Agent", "FableFiles v0.1")
        .call()
        .map_err(|e| format!("download failed: {e}"))?;
    let mut buf = Vec::new();
    use std::io::Read;
    resp.into_reader()
        .take(32 * 1024 * 1024)
        .read_to_end(&mut buf)
        .map_err(|e| format!("download failed: {e}"))?;
    if buf.is_empty() {
        return Err("the server sent an empty file".into());
    }
    Ok(buf)
}

/// Write bytes beside a target via .part + rename; NEVER overwrite.
fn place_bytes(target: &Path, bytes: &[u8]) -> Result<(), String> {
    if target.exists() {
        return Err(format!("{} already exists", target.display()));
    }
    let part = target.with_extension("jpg.part");
    std::fs::write(&part, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&part, target).map_err(|e| e.to_string())
}

/// Fetch artwork for one confirmed video. Used by the FetchArtwork
/// job step and by scheduled runs — one implementation, two doors.
pub fn fetch_artwork(
    video_path: &Path,
    media_type: &str,
    tmdb_id: i64,
    season: Option<u32>,
    fanart: bool,
    tmdb_key: Option<String>,
    cancel: &dyn Fn() -> bool,
) -> FetchVerdict {
    let targets = artwork_targets(video_path, media_type, fanart);
    let missing: Vec<&ArtTarget> = targets.iter().filter(|t| !t.path.exists()).collect();
    if missing.is_empty() {
        return FetchVerdict::Skipped(
            "artwork already in place — your existing files win".into(),
        );
    }
    let Some(key) = tmdb_key else {
        return FetchVerdict::Failed(
            "artwork lookup needs your free TMDB key (Preferences → Media Resolution)".into(),
        );
    };
    let client = crate::mediares::TmdbClient::new(key);
    // Resolve the image paths this video needs (one details call).
    let (poster, backdrop, season_poster) = if media_type == "episode" {
        let (p, b) = match client.tv_art(tmdb_id) {
            Ok(v) => v,
            Err(e) => return FetchVerdict::Failed(e),
        };
        let sp = season.and_then(|s| client.season_poster(tmdb_id, s).ok().flatten());
        (p, b, sp)
    } else {
        match client.movie_art(tmdb_id) {
            Ok((p, b)) => (p, b, None),
            Err(e) => return FetchVerdict::Failed(e),
        }
    };
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(30))
        .build();
    let mut placed: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    for t in missing {
        if cancel() {
            return FetchVerdict::Failed("cancelled".into());
        }
        let (img, size) = match t.kind {
            "fanart" => (&backdrop, "w1280"),
            "season" => (&season_poster, "w500"),
            _ => (&poster, "w500"),
        };
        let Some(img) = img else {
            notes.push(format!("TMDB has no {} for this title", t.kind));
            continue;
        };
        let url = crate::mediares::tmdb_image_url(size, img);
        match download_bytes(&agent, &url).and_then(|b| place_bytes(&t.path, &b)) {
            Ok(()) => placed.push(t.path.to_string_lossy().into_owned()),
            Err(e) => notes.push(format!("{}: {e}", t.path.display())),
        }
    }
    if placed.is_empty() {
        if notes.is_empty() {
            FetchVerdict::Skipped("TMDB has no artwork for this title".into())
        } else {
            FetchVerdict::Failed(notes.join("; "))
        }
    } else {
        FetchVerdict::Done(placed)
    }
}

// ---------------------------------------------------------------------------
// Subtitles: target naming (pure) + fetch
// ---------------------------------------------------------------------------

/// The server sidecar name for a subtitle: `<stem>.<lang>.srt`,
/// with `.forced` / `.sdh` labels after the language, Plex-style.
pub fn subtitle_target(
    video: &Path,
    language: &str,
    hearing_impaired: bool,
    foreign_parts: bool,
) -> PathBuf {
    let stem = video
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut name = format!("{stem}.{language}");
    if foreign_parts {
        name.push_str(".forced");
    } else if hearing_impaired {
        name.push_str(".sdh");
    }
    name.push_str(".srt");
    video.parent().unwrap_or(Path::new("/")).join(name)
}

/// Any subtitle already beside this video for this language (or a
/// bare `<stem>.srt`)? Existing subtitles are never overwritten.
pub fn has_subtitle(video: &Path, language: &str) -> bool {
    let parent = video.parent().unwrap_or(Path::new("/"));
    let stem = video
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    for cand in [
        format!("{stem}.{language}.srt"),
        format!("{stem}.{language}.forced.srt"),
        format!("{stem}.{language}.sdh.srt"),
        format!("{stem}.srt"),
    ] {
        if parent.join(cand).exists() {
            return true;
        }
    }
    false
}

#[derive(Debug)]
pub enum SubVerdict {
    Done {
        path: String,
        quota: crate::opensubs::QuotaState,
    },
    Skipped(String),
    QuotaReached(crate::opensubs::QuotaState),
    Failed(String),
}

fn is_quota_message(msg: &str) -> bool {
    msg.contains("allowance") || msg.contains("403") || msg.contains("406")
}

/// Fetch one subtitle for one confirmed video. Shared by the
/// FetchSubtitle job step and scheduled runs.
pub fn fetch_subtitle(
    video_path: &Path,
    language: &str,
    media_type: &str,
    tmdb_id: Option<i64>,
    season: Option<u32>,
    episode: Option<u32>,
) -> SubVerdict {
    if has_subtitle(video_path, language) {
        return SubVerdict::Skipped(
            "a subtitle is already beside this file — yours wins".into(),
        );
    }
    let client = match crate::opensubs::OpenSubsClient::from_keyring() {
        Ok(c) => c,
        Err(e) => return SubVerdict::Failed(e),
    };
    let found = match client.find_for_file(
        video_path, language, media_type, tmdb_id, season, episode,
    ) {
        Ok(f) => f,
        Err(e) => return SubVerdict::Failed(e),
    };
    let Some(best) = found.into_iter().next() else {
        return SubVerdict::Skipped(format!(
            "no {language} subtitle found on OpenSubtitles for this file"
        ));
    };
    match client.download(best.file_id) {
        Ok((bytes, _name, quota)) => {
            let target = subtitle_target(
                video_path,
                language,
                best.hearing_impaired,
                best.foreign_parts_only,
            );
            if target.exists() {
                return SubVerdict::Skipped(
                    "a subtitle is already beside this file — yours wins".into(),
                );
            }
            let part = target.with_extension("srt.part");
            match std::fs::write(&part, &bytes)
                .and_then(|_| std::fs::rename(&part, &target))
            {
                Ok(()) => SubVerdict::Done {
                    path: target.to_string_lossy().into_owned(),
                    quota,
                },
                Err(e) => SubVerdict::Failed(format!("could not save the subtitle: {e}")),
            }
        }
        Err(e) => {
            if is_quota_message(&e) {
                SubVerdict::QuotaReached(crate::opensubs::QuotaState {
                    remaining: Some(0),
                    message: Some(e),
                    ..Default::default()
                })
            } else {
                SubVerdict::Failed(e)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The completion run (headless): scan → organize → artwork →
// subtitles, with every action recorded in the run journal.
// ---------------------------------------------------------------------------

fn now_local() -> chrono::NaiveDateTime {
    chrono::Local::now().naive_local()
}

fn stamp(t: chrono::NaiveDateTime) -> String {
    t.format("%Y-%m-%dT%H:%M:%S").to_string()
}

fn rfc_local(t: chrono::NaiveDateTime) -> String {
    // Run rows are read by humans in the morning report, in local
    // time — the same naive-local stamp the scheduler state uses.
    stamp(t)
}

/// Move one file: rename on the same device, verified
/// copy-then-delete across devices (the house decomposition).
pub fn move_file(src: &Path, dst: &Path) -> Result<(), String> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(_) => {
            // Cross-device (or rename refused): copy, verify the
            // length, then remove the source. A crash mid-way can
            // duplicate, never lose.
            std::fs::copy(src, dst).map_err(|e| format!("copy failed: {e}"))?;
            let a = std::fs::metadata(src).map(|m| m.len()).unwrap_or(0);
            let b = std::fs::metadata(dst).map(|m| m.len()).unwrap_or(u64::MAX);
            if a != b {
                let _ = std::fs::remove_file(dst);
                return Err("copied file size does not match the source".into());
            }
            std::fs::remove_file(src).map_err(|e| format!("could not remove source: {e}"))
        }
    }
}

fn eligible(idn: &crate::mediares::IdentityView) -> bool {
    // The autonomy law: confirmed by the user, or a Confident-tier
    // TMDB match (≥ 0.85 — mediares::CONFIDENT_MIN). Name-derived
    // identities are guesses until confirmed and never qualify.
    idn.confirmed || idn.confidence.map(|c| c >= 0.85).unwrap_or(false)
}

/// Execute one completion run for a schedule (also used for manual
/// "run now"). Always returns the finished run row — failures are
/// recorded IN the row, not lost as an Err, except when the catalog
/// itself cannot be opened.
pub fn run_completion(
    sched: &Schedule,
    late: bool,
) -> Result<ufm_catalog::MediaCompleteRunRow, String> {
    let base = data_base();
    let cat_path = catalog_path();
    let catalog = Catalog::open(&cat_path).map_err(|e| format!("catalog open failed: {e}"))?;
    let settings = load_settings(&base);
    let started = now_local();
    let run_id = uuid::Uuid::new_v4().to_string();
    let mut row = ufm_catalog::MediaCompleteRunRow {
        id: run_id.clone(),
        schedule_id: Some(sched.id.clone()),
        schedule_name: sched.name.clone(),
        target: sched.target.clone(),
        started: rfc_local(started),
        finished: None,
        was_late: late,
        status: "running".into(),
        summary_json: "{}".into(),
        actions_json: "[]".into(),
        pending_json: "[]".into(),
        quota_json: None,
        undone_at: None,
    };
    let _ = catalog.upsert_mediacomplete_run(&row);

    let mut failures: Vec<String> = Vec::new();
    let mut actions: Vec<serde_json::Value> = Vec::new();
    let mut pending: Vec<serde_json::Value> = Vec::new();
    let mut quota: Option<crate::opensubs::QuotaState> = None;
    let (mut organized, mut art_placed, mut subs_placed) = (0u32, 0u32, 0u32);
    let mut scanned = 0u32;
    let mut truncated = false;

    let target = PathBuf::from(&sched.target);
    let key = crate::mediares::tmdb_key_get();
    let scan = crate::mediares::scan(
        &catalog,
        &target,
        &[sched.target.clone()],
        key.clone(),
    );
    match scan {
        Ok(scan) => {
            scanned = scan.rows.len() as u32;
            truncated = scan.truncated;
            // Final resting path per source (after organize).
            let mut final_path: HashMap<String, String> = HashMap::new();
            for r in &scan.rows {
                final_path.insert(r.path.clone(), r.path.clone());
            }
            if sched.steps.organize {
                let paths: Vec<String> = scan.rows.iter().map(|r| r.path.clone()).collect();
                match crate::mediares::plan_organize(&catalog, &target, &paths) {
                    Ok(plan) => {
                        for ex in &plan.excluded {
                            // "Already in place" is not a failure.
                            if !ex.reason.contains("already in its server place") {
                                failures.push(format!("{}: {}", ex.path, ex.reason));
                            }
                        }
                        for a in &plan.actions {
                            match move_file(Path::new(&a.src), Path::new(&a.dst)) {
                                Ok(()) => {
                                    organized += 1;
                                    actions.push(serde_json::json!({
                                        "kind": "move", "from": a.src, "to": a.dst
                                    }));
                                    final_path.insert(a.src.clone(), a.dst.clone());
                                }
                                Err(e) => failures.push(format!("{}: move failed: {e}", a.src)),
                            }
                        }
                    }
                    Err(e) => failures.push(format!("organize plan failed: {e}")),
                }
            }
            // Artwork + subtitles for eligible identities, at their
            // final paths. Probable/Unknown rows go to pending.
            let mut quota_hit = false;
            for r in &scan.rows {
                let Some(idn) = &r.identity else {
                    pending.push(pending_entry(r, None));
                    continue;
                };
                if !eligible(idn) {
                    pending.push(pending_entry(r, Some(idn)));
                    continue;
                }
                let here = final_path
                    .get(&r.path)
                    .cloned()
                    .unwrap_or_else(|| r.path.clone());
                let here_p = PathBuf::from(&here);
                if sched.steps.artwork {
                    if let Some(tid) = idn.tmdb_id {
                        match fetch_artwork(
                            &here_p,
                            &idn.media_type,
                            tid,
                            idn.season.map(|s| s as u32),
                            settings.fanart,
                            key.clone(),
                            &|| false,
                        ) {
                            FetchVerdict::Done(placed) => {
                                art_placed += placed.len() as u32;
                                for p in placed {
                                    actions.push(serde_json::json!({
                                        "kind": "placed", "path": p
                                    }));
                                }
                            }
                            FetchVerdict::Skipped(_) => {}
                            FetchVerdict::Failed(e) => {
                                failures.push(format!("{}: artwork: {e}", r.name))
                            }
                        }
                    }
                }
                if sched.steps.subtitles && !quota_hit {
                    match fetch_subtitle(
                        &here_p,
                        &settings.subtitle_language,
                        &idn.media_type,
                        idn.tmdb_id,
                        idn.season.map(|s| s as u32),
                        idn.episode.map(|e| e as u32),
                    ) {
                        SubVerdict::Done { path, quota: q } => {
                            subs_placed += 1;
                            quota = Some(q);
                            actions.push(serde_json::json!({
                                "kind": "placed", "path": path
                            }));
                        }
                        SubVerdict::Skipped(_) => {}
                        SubVerdict::QuotaReached(q) => {
                            quota = Some(q);
                            quota_hit = true;
                            failures.push(
                                "subtitle download allowance reached — remaining subtitles skipped until it resets".into(),
                            );
                        }
                        SubVerdict::Failed(e) => {
                            failures.push(format!("{}: subtitle: {e}", r.name))
                        }
                    }
                } else if sched.steps.subtitles && quota_hit {
                    // Not attempted; the quota failure above says why.
                }
            }
        }
        Err(e) => {
            row.status = "failed".into();
            row.summary_json = serde_json::json!({ "error": e }).to_string();
            row.finished = Some(rfc_local(now_local()));
            let _ = catalog.upsert_mediacomplete_run(&row);
            return Ok(row);
        }
    }

    let summary = serde_json::json!({
        "scanned": scanned,
        "truncated": truncated,
        "organized": organized,
        "artworkPlaced": art_placed,
        "subtitlesPlaced": subs_placed,
        "pending": pending.len(),
        "failures": failures,
    });
    row.summary_json = summary.to_string();
    row.actions_json = serde_json::Value::Array(actions).to_string();
    row.pending_json = serde_json::Value::Array(pending).to_string();
    row.quota_json = quota.map(|q| serde_json::to_string(&q).unwrap_or_default());
    row.status = if failures.is_empty() {
        "completed"
    } else {
        "completed_with_failures"
    }
    .into();
    row.finished = Some(rfc_local(now_local()));
    let _ = catalog.upsert_mediacomplete_run(&row);
    let _ = catalog.prune_mediacomplete_runs(50);
    Ok(row)
}

fn pending_entry(
    r: &crate::mediares::ScanRow,
    idn: Option<&crate::mediares::IdentityView>,
) -> serde_json::Value {
    serde_json::json!({
        "path": r.path,
        "name": r.name,
        "parsed": r.parsed,
        "identity": idn,
        "candidates": r.candidates,
        "note": r.note,
    })
}

/// Reverse a finished run from its journal: placed files are
/// removed; moves are moved back (newest first). Honest partial
/// results: every reversal that couldn't happen is named.
pub fn undo_run(run_id: &str) -> Result<String, String> {
    let catalog = Catalog::open(&catalog_path()).map_err(|e| e.to_string())?;
    let mut row = catalog
        .mediacomplete_run(run_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no such run".to_string())?;
    if row.undone_at.is_some() {
        return Err("this run was already undone".into());
    }
    let actions: Vec<serde_json::Value> =
        serde_json::from_str(&row.actions_json).unwrap_or_default();
    let (mut reversed, mut problems) = (0u32, Vec::new());
    for a in actions.iter().rev() {
        match a.get("kind").and_then(|k| k.as_str()) {
            Some("placed") => {
                if let Some(p) = a.get("path").and_then(|p| p.as_str()) {
                    match std::fs::remove_file(p) {
                        Ok(()) => reversed += 1,
                        Err(e) => problems.push(format!("{p}: {e}")),
                    }
                }
            }
            Some("move") => {
                let (Some(from), Some(to)) = (
                    a.get("from").and_then(|p| p.as_str()),
                    a.get("to").and_then(|p| p.as_str()),
                ) else {
                    continue;
                };
                if !Path::new(to).exists() {
                    problems.push(format!("{to}: no longer there"));
                    continue;
                }
                if Path::new(from).exists() {
                    problems.push(format!("{from}: the original place is occupied again"));
                    continue;
                }
                match move_file(Path::new(to), Path::new(from)) {
                    Ok(()) => reversed += 1,
                    Err(e) => problems.push(format!("{to} → {from}: {e}")),
                }
            }
            _ => {}
        }
    }
    row.undone_at = Some(rfc_local(now_local()));
    row.status = "undone".into();
    catalog
        .upsert_mediacomplete_run(&row)
        .map_err(|e| e.to_string())?;
    if problems.is_empty() {
        Ok(format!("Run undone — {reversed} action(s) reversed."))
    } else {
        Ok(format!(
            "Run undone with problems — {reversed} reversed, {} could not be: {}",
            problems.len(),
            problems.join("; ")
        ))
    }
}

// ---------------------------------------------------------------------------
// The background agent: a quiet loop that owns the schedule.
// ---------------------------------------------------------------------------

fn agent_lock_path(base: &Path) -> PathBuf {
    base.join("agent.lock")
}

fn agent_status_path(base: &Path) -> PathBuf {
    base.join("agent-status.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatus {
    pub running: bool,
    pub pid: Option<u32>,
    pub last_tick: Option<String>,
    /// (schedule name, next slot local stamp)
    pub next_runs: Vec<(String, String)>,
    pub note: Option<String>,
}

pub fn agent_status(base: &Path) -> AgentStatus {
    let mut st = AgentStatus {
        running: false,
        pid: None,
        last_tick: None,
        next_runs: Vec::new(),
        note: None,
    };
    if let Ok(text) = std::fs::read_to_string(agent_status_path(base)) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            st.pid = v.get("pid").and_then(|p| p.as_u64()).map(|p| p as u32);
            st.last_tick = v
                .get("at")
                .and_then(|a| a.as_str())
                .map(|s| s.to_string());
            if let Some(arr) = v.get("nextRuns").and_then(|n| n.as_array()) {
                for n in arr {
                    if let (Some(a), Some(b)) = (
                        n.get(0).and_then(|x| x.as_str()),
                        n.get(1).and_then(|x| x.as_str()),
                    ) {
                        st.next_runs.push((a.to_string(), b.to_string()));
                    }
                }
            }
        }
    }
    if let Some(pid) = st.pid {
        st.running = Path::new(&format!("/proc/{pid}")).exists();
    }
    st
}

fn write_agent_status(base: &Path, settings: &McSettings, now: chrono::NaiveDateTime) {
    let next: Vec<(String, String)> = settings
        .schedules
        .iter()
        .filter_map(|s| next_slot(s, now).map(|n| (s.name.clone(), stamp(n))))
        .collect();
    let v = serde_json::json!({
        "pid": std::process::id(),
        "at": stamp(now),
        "nextRuns": next,
    });
    let _ = std::fs::write(agent_status_path(base), v.to_string());
}

/// `fable-files --background`: the scheduler agent. Single-instance
/// (pid lockfile, stale-takeover), 30-second tick, no UI, ever.
/// Returns a process exit code.
pub fn run_background_agent() -> i32 {
    let base = data_base();
    let _ = std::fs::create_dir_all(&base);
    // Single instance: a live pid in the lockfile means an agent
    // already owns the schedule.
    let lock = agent_lock_path(&base);
    if let Ok(text) = std::fs::read_to_string(&lock) {
        if let Ok(pid) = text.trim().parse::<u32>() {
            if Path::new(&format!("/proc/{pid}")).exists() && pid != std::process::id() {
                return 0;
            }
        }
    }
    let _ = std::fs::write(&lock, std::process::id().to_string());
    eprintln!(
        "[fable-agent] started (pid {}), watching {}",
        std::process::id(),
        base.display()
    );
    loop {
        let mut settings = load_settings(&base);
        let now = now_local();
        let mut changed = false;
        for i in 0..settings.schedules.len() {
            let sched = settings.schedules[i].clone();
            if !sched.enabled {
                continue;
            }
            if let Decision::Due { late } = evaluate(&sched, now) {
                if let Some(reason) = defer_reason(&sched) {
                    eprintln!("[fable-agent] '{}' due but deferred: {reason}", sched.name);
                    continue;
                }
                eprintln!(
                    "[fable-agent] running '{}' (late: {late})",
                    sched.name
                );
                let started = stamp(now_local());
                match run_completion(&sched, late) {
                    Ok(row) => {
                        settings.schedules[i].last_started = Some(started);
                        settings.schedules[i].last_status = Some(row.status.clone());
                        changed = true;
                        eprintln!(
                            "[fable-agent] '{}' finished: {}",
                            sched.name, row.status
                        );
                    }
                    Err(e) => {
                        settings.schedules[i].last_started = Some(started);
                        settings.schedules[i].last_status =
                            Some(format!("failed: {e}"));
                        changed = true;
                        eprintln!("[fable-agent] '{}' failed: {e}", sched.name);
                    }
                }
            }
        }
        if changed {
            let _ = save_settings(&base, &settings);
        }
        write_agent_status(&base, &settings, now_local());
        std::thread::sleep(std::time::Duration::from_secs(30));
    }
}

// ---------------------------------------------------------------------------
// Autostart management: systemd user unit (primary), XDG autostart
// entry (fallback). Both are user-visible, user-removable files.
// ---------------------------------------------------------------------------

pub fn agent_unit_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".config/systemd/user/fable-files-agent.service")
}

pub fn agent_desktop_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".config/autostart/fable-files-agent.desktop")
}

fn autostart_kind() -> &'static str {
    if agent_unit_path().exists() {
        "systemd"
    } else if agent_desktop_path().exists() {
        "autostart"
    } else {
        "none"
    }
}

pub fn agent_install_kind() -> &'static str {
    autostart_kind()
}

fn systemctl_user(args: &[&str]) -> Result<(), String> {
    let out = std::process::Command::new("systemctl")
        .args(std::iter::once("--user").chain(args.iter().copied()))
        .output()
        .map_err(|e| format!("systemctl unavailable: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Turn the background agent on/off. Writes the unit (or fallback
/// entry), asks systemd to run it now, and reports honestly what
/// actually happened. `appimage` is the stable AppImage path.
pub fn set_agent_installed(appimage: &str, enabled: bool) -> Result<String, String> {
    let unit = agent_unit_path();
    let desktop = agent_desktop_path();
    if enabled {
        let unit_text = format!(
            "[Unit]\nDescription=Fable Files background scheduler\nAfter=network-online.target\n\n[Service]\nType=simple\nExecStart={appimage} --background\nRestart=on-failure\nRestartSec=10\n\n[Install]\nWantedBy=default.target\n"
        );
        if let Some(parent) = unit.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&unit, unit_text).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(&desktop);
        match systemctl_user(&["daemon-reload"]).and_then(|_| {
            systemctl_user(&["enable", "--now", "fable-files-agent.service"])
        }) {
            Ok(()) => Ok(
                "Background agent installed as a systemd user service and started — it runs at every login, window open or not."
                    .into(),
            ),
            Err(e) => {
                // Fallback: XDG autostart entry (still login-started,
                // just not systemd-supervised). Say so, honestly.
                let desk_text = format!(
                    "[Desktop Entry]\nType=Application\nName=Fable Files background scheduler\nExec={appimage} --background\nX-GNOME-Autostart-enabled=true\nNoDisplay=true\n"
                );
                if let Some(parent) = desktop.parent() {
                    std::fs::create_dir_all(parent).map_err(|ee| ee.to_string())?;
                }
                std::fs::write(&desktop, desk_text).map_err(|ee| ee.to_string())?;
                let _ = std::fs::remove_file(&unit);
                Ok(format!(
                    "systemd user services weren't available ({e}), so the agent was installed as a login autostart entry instead — it starts at login and runs with the window closed."
                ))
            }
        }
    } else {
        let _ = systemctl_user(&["disable", "--now", "fable-files-agent.service"]);
        let _ = std::fs::remove_file(&unit);
        let _ = std::fs::remove_file(&desktop);
        let _ = systemctl_user(&["daemon-reload"]);
        Ok("Background agent stopped and removed — scheduled runs will not fire until it is turned on again.".into())
    }
}

#[cfg(test)]
mod tests;
