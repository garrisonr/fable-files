//! mediares: Media Resolution (Dickles, 2026-10-15) — resolve film +
//! TV files inside Fable so libraries are ready for Plex / Emby /
//! Jellyfin. The video twin of ROM identification, and it obeys the
//! same laws:
//!
//! - The parser and scorer are pure (ufm_media::videoid); this module
//!   is the TMDB client, the keyring, and the scan/confirm/organize
//!   logic over the catalog.
//! - TMDB matching is HONEST: three confidence tiers, the evidence
//!   strings ride along to the UI, Probable + Unknown require a human
//!   confirm, and nothing is ever silently renamed. Organize runs
//!   only on confirmed or Confident identities, only after the UI
//!   showed the full from→to preview, as a journaled ufm-jobs job.
//! - The TMDB key is the USER's own free key, pasted in Preferences
//!   and stored ONLY in the OS keyring (entry `mediares/tmdb`). It is
//!   never written to a settings file, a job snapshot, or a log. No
//!   key is ever bundled with the app.
//! - Keyless is first-class: parsing, compliance judging and
//!   organizing from names the user confirms by hand all work with
//!   no key at all; the key only unlocks TMDB verification.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use ufm_catalog::Catalog;
use ufm_media::videoid::{
    self, CandidateScore, ParsedKind, ParsedVideo, Tier, TmdbCandidate, TmdbEpisode,
};

pub const VIDEO_EXTS: &[&str] = &[
    "mkv", "mp4", "avi", "mov", "m4v", "ts", "m2ts", "wmv", "flv", "webm", "mpg", "mpeg",
    "ogv", "3gp", "divx",
];

const MAX_SCAN_FILES: usize = 500;
/// Organize (and batch-confirm) eligibility: a stored identity must
/// be user-confirmed or Confident-tier. Mirrors videoid::tier_for.
const CONFIDENT_MIN: f64 = 0.85;

// ---------------------------------------------------------------------------
// TMDB key — OS keyring only (rommedia pattern).
// ---------------------------------------------------------------------------

fn keyring_entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(crate::rommedia::KEYRING_SERVICE, "mediares/tmdb")
        .map_err(|e| format!("system keyring unavailable: {e}"))
}

pub fn tmdb_key_get() -> Option<String> {
    let entry = keyring_entry().ok()?;
    match entry.get_password() {
        Ok(v) if !v.is_empty() => Some(v),
        _ => None,
    }
}

pub fn tmdb_key_set(value: Option<&str>) -> Result<(), String> {
    let entry = keyring_entry()?;
    match value {
        Some(v) if !v.trim().is_empty() => {
            entry
                .set_password(v.trim())
                .map_err(|e| format!("could not save to the system keyring: {e}"))?;
            // Never report "Saved" on faith: read it back through a
            // fresh Entry first (the 2026-10-15 mock-store incident —
            // saves returned Ok and evaporated).
            crate::rommedia::verify_readback("mediares/tmdb", v.trim())
        }
        _ => match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(format!("could not clear the saved key: {e}")),
        },
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    pub has_key: bool,
    pub keyring_ok: bool,
}

pub fn settings_view() -> SettingsView {
    SettingsView {
        has_key: tmdb_key_get().is_some(),
        keyring_ok: crate::rommedia::keyring_available(),
    }
}

// ---------------------------------------------------------------------------
// TMDB client (v3 key auth). JSON → videoid structs mapping is pure
// and unit-tested against real captured responses.
// ---------------------------------------------------------------------------

pub struct TmdbClient {
    key: String,
    agent: ureq::Agent,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateResult {
    pub ok: bool,
    pub message: String,
}

impl TmdbClient {
    pub fn new(key: String) -> Self {
        Self {
            key,
            agent: ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(20))
                .build(),
        }
    }

    fn get(&self, path: &str, query: &[(&str, String)]) -> Result<serde_json::Value, String> {
        // Politeness throttle: TMDB's published budget is generous,
        // and a season pack should still look like a person, not a
        // scraper.
        std::thread::sleep(std::time::Duration::from_millis(34));
        let mut req = self
            .agent
            .get(&format!("https://api.themoviedb.org/3{path}"))
            .query("api_key", &self.key);
        for (k, v) in query {
            req = req.query(k, v);
        }
        match req.call() {
            Ok(resp) => {
                let text = resp
                    .into_string()
                    .map_err(|e| format!("TMDB sent back something unreadable: {e}"))?;
                serde_json::from_str(&text)
                    .map_err(|e| format!("TMDB sent back something unreadable: {e}"))
            }
            Err(ureq::Error::Status(401, _)) => {
                Err("TMDB rejected the API key (401). Check the key in Preferences.".into())
            }
            Err(ureq::Error::Status(code, _)) => {
                Err(format!("TMDB answered with an error (HTTP {code})."))
            }
            Err(e) => Err(format!("couldn't reach TMDB: {e}")),
        }
    }

    pub fn validate(&self) -> ValidateResult {
        match self.get("/configuration", &[]) {
            Ok(_) => ValidateResult {
                ok: true,
                message: "TMDB accepted the key — lookups are on.".into(),
            },
            Err(e) => ValidateResult {
                ok: false,
                message: e,
            },
        }
    }

    pub fn search_movies(
        &self,
        title: &str,
        year: Option<u32>,
    ) -> Result<Vec<TmdbCandidate>, String> {
        let mut q = vec![("query", title.to_string())];
        if let Some(y) = year {
            q.push(("year", y.to_string()));
        }
        let v = self.get("/search/movie", &q)?;
        Ok(candidates_from_search(&v, "movie"))
    }

    pub fn search_tv(
        &self,
        title: &str,
        year: Option<u32>,
    ) -> Result<Vec<TmdbCandidate>, String> {
        let mut q = vec![("query", title.to_string())];
        if let Some(y) = year {
            q.push(("first_air_date_year", y.to_string()));
        }
        let v = self.get("/search/tv", &q)?;
        Ok(candidates_from_search(&v, "tv"))
    }

    pub fn season_episodes(
        &self,
        tv_id: i64,
        season: u32,
    ) -> Result<Vec<TmdbEpisode>, String> {
        let v = self.get(&format!("/tv/{tv_id}/season/{season}"), &[])?;
        Ok(episodes_from_season(&v))
    }

    /// Artwork paths for a movie: (poster_path, backdrop_path) from
    /// the details call. Media Completion files these as poster.jpg
    /// / fanart.jpg sidecars; the image CDN itself needs no key.
    pub fn movie_art(&self, id: i64) -> Result<(Option<String>, Option<String>), String> {
        let v = self.get(&format!("/movie/{id}"), &[])?;
        Ok(art_from_details(&v))
    }

    /// Artwork paths for a show: (poster_path, backdrop_path).
    pub fn tv_art(&self, id: i64) -> Result<(Option<String>, Option<String>), String> {
        let v = self.get(&format!("/tv/{id}"), &[])?;
        Ok(art_from_details(&v))
    }

    /// A season's own poster path (season posters live in the
    /// Season folder as poster.jpg).
    pub fn season_poster(&self, tv_id: i64, season: u32) -> Result<Option<String>, String> {
        let v = self.get(&format!("/tv/{tv_id}/season/{season}"), &[])?;
        Ok(poster_from_season(&v))
    }
}

/// TMDB image CDN URL for an image path at a size segment
/// ("w500" posters, "w1280" fanart are the house picks — sensible
/// resolution: not the multi-MB original, not a thumbnail).
pub fn tmdb_image_url(size: &str, path: &str) -> String {
    format!("https://image.tmdb.org/t/p/{size}{path}")
}

/// Map a TMDB search response to candidates. Pure; tested against a
/// real captured response (research/tmdb-search-*.json, 2026-10-15).
pub fn candidates_from_search(v: &serde_json::Value, media_type: &str) -> Vec<TmdbCandidate> {
    let mut out = Vec::new();
    let Some(results) = v.get("results").and_then(|r| r.as_array()) else {
        return out;
    };
    for r in results {
        let Some(id) = r.get("id").and_then(|i| i.as_i64()) else {
            continue;
        };
        let title = r
            .get("title")
            .or_else(|| r.get("name"))
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string();
        let original = r
            .get("original_title")
            .or_else(|| r.get("original_name"))
            .and_then(|t| t.as_str())
            .unwrap_or(&title)
            .to_string();
        let date = r
            .get("release_date")
            .or_else(|| r.get("first_air_date"))
            .and_then(|d| d.as_str())
            .unwrap_or_default();
        let year = date
            .get(..4)
            .and_then(|y| y.parse::<u32>().ok())
            .filter(|y| *y > 0);
        out.push(TmdbCandidate {
            id,
            media_type: media_type.to_string(),
            title,
            original_title: original,
            year,
            popularity: r.get("popularity").and_then(|p| p.as_f64()).unwrap_or(0.0),
        });
    }
    out
}

/// (poster_path, backdrop_path) from a movie/TV details response.
/// Pure; tested against the real captured field values.
pub fn art_from_details(v: &serde_json::Value) -> (Option<String>, Option<String>) {
    let s = |k: &str| {
        v.get(k)
            .and_then(|p| p.as_str())
            .filter(|p| !p.is_empty())
            .map(|p| p.to_string())
    };
    (s("poster_path"), s("backdrop_path"))
}

/// The season's poster_path from a season response. Pure; tested.
pub fn poster_from_season(v: &serde_json::Value) -> Option<String> {
    v.get("poster_path")
        .and_then(|p| p.as_str())
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string())
}

/// Map a TMDB season response to its episode list. Pure; tested.
pub fn episodes_from_season(v: &serde_json::Value) -> Vec<TmdbEpisode> {
    let mut out = Vec::new();
    let Some(eps) = v.get("episodes").and_then(|e| e.as_array()) else {
        return out;
    };
    for e in eps {
        let Some(num) = e
            .get("episode_number")
            .and_then(|n| n.as_u64())
            .map(|n| n as u32)
        else {
            continue;
        };
        out.push(TmdbEpisode {
            episode_number: num,
            name: e
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or_default()
                .to_string(),
            air_date: e
                .get("air_date")
                .and_then(|d| d.as_str())
                .map(|s| s.to_string()),
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Identities: stored rows <-> the view the UI works with
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityView {
    /// "movie" | "episode" (catalog media_kind vocabulary)
    pub media_type: String,
    pub tmdb_id: Option<i64>,
    pub title: String,
    pub year: Option<i64>,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    pub episode_end: Option<i64>,
    pub confidence: Option<f64>,
    pub confirmed: bool,
    /// stored | scan | name — where this identity came from.
    pub source: String,
    /// TMDB episode titles for the target name ("" when unknown).
    #[serde(default)]
    pub episode_titles: Vec<String>,
}

fn identity_from_row(row: &ufm_catalog::MediaIdentityRow, source: &str) -> Option<IdentityView> {
    let title = row.title.clone()?;
    let mut ep_titles = Vec::new();
    if let Ok(ev) = serde_json::from_str::<serde_json::Value>(&row.evidence_json) {
        if let Some(arr) = ev.get("episode_titles").and_then(|t| t.as_array()) {
            ep_titles = arr
                .iter()
                .filter_map(|t| t.as_str().map(|s| s.to_string()))
                .collect();
        }
    }
    Some(IdentityView {
        media_type: row.media_kind.clone(),
        tmdb_id: row.tmdb_id,
        title,
        year: row.year,
        season: row.season,
        episode: row.episode,
        episode_end: row.episode_end,
        confidence: row.confidence,
        confirmed: row.confirmed,
        source: source.to_string(),
        episode_titles: ep_titles,
    })
}

/// File freshness evidence, ROM-identify style: (size, modified).
fn fs_freshness(path: &Path) -> (u64, Option<String>) {
    match std::fs::metadata(path) {
        Ok(m) => {
            let modified = m
                .modified()
                .ok()
                .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339());
            (m.len(), modified)
        }
        Err(_) => (0, None),
    }
}

fn stored_is_fresh(row: &ufm_catalog::MediaIdentityRow, path: &Path) -> bool {
    let (size, modified) = fs_freshness(path);
    let Ok(ev) = serde_json::from_str::<serde_json::Value>(&row.evidence_json) else {
        return false;
    };
    ev.get("size").and_then(|v| v.as_u64()) == Some(size)
        && ev
            .get("modified")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            == modified
}

/// The canonical target (relative to the library root) for an
/// identity + the facts only the file itself knows (edition, part).
fn target_rel_for(
    parsed: &ParsedVideo,
    idn: &IdentityView,
    ext: &str,
) -> Option<PathBuf> {
    let year = idn.year.map(|y| y as u32);
    match idn.media_type.as_str() {
        "movie" => Some(videoid::movie_target_rel(
            &idn.title,
            year,
            parsed.edition.as_deref(),
            parsed.part.as_deref(),
            ext,
        )),
        "episode" => {
            let season = idn.season.or(parsed.season.map(|s| s as i64))? as u32;
            let first = idn.episode.or(parsed.episode_first.map(|e| e as i64))? as u32;
            let last = idn
                .episode_end
                .or(parsed.episode_last.map(|e| e as i64))
                .unwrap_or(first as i64) as u32;
            let eps: Vec<u32> = (first..=last).collect();
            Some(videoid::episode_target_rel(
                &idn.title,
                year,
                season,
                &eps,
                &idn.episode_titles,
                ext,
            ))
        }
        _ => None,
    }
}

fn ext_of(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn is_video(path: &Path) -> bool {
    VIDEO_EXTS.contains(&ext_of(path).as_str())
}

// ---------------------------------------------------------------------------
// Scan: parse + match + judge one folder's worth of files
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanRow {
    pub path: String,
    pub name: String,
    pub item_id: Option<String>,
    pub parsed: ParsedVideo,
    /// ready | fixable | attention
    pub state: String,
    pub identity: Option<IdentityView>,
    pub candidates: Vec<CandidateScore>,
    pub target_rel: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanResult {
    pub root: String,
    pub has_key: bool,
    pub rows: Vec<ScanRow>,
    pub ready: u32,
    pub fixable: u32,
    pub attention: u32,
    pub truncated: bool,
}

/// Expand scan inputs: directories walk recursively (video files
/// only), files pass through. Deterministic order (sorted).
pub fn expand_inputs(paths: &[String]) -> (Vec<PathBuf>, bool) {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut truncated = false;
    let mut queue: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    queue.sort();
    while let Some(p) = queue.pop() {
        if p.is_dir() {
            let mut entries: Vec<PathBuf> = std::fs::read_dir(&p)
                .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
                .unwrap_or_default();
            entries.sort();
            queue.extend(entries);
        } else if p.is_file() && is_video(&p) {
            if out.len() >= MAX_SCAN_FILES {
                truncated = true;
            } else {
                out.push(p);
            }
        }
    }
    out.sort();
    out.dedup();
    (out, truncated)
}

struct Matcher<'a> {
    client: Option<&'a TmdbClient>,
    search_cache: HashMap<String, Result<Vec<TmdbCandidate>, String>>,
    season_cache: HashMap<(i64, u32), Result<Vec<TmdbEpisode>, String>>,
}

impl<'a> Matcher<'a> {
    fn search(
        &mut self,
        media_type: &str,
        title: &str,
        year: Option<u32>,
    ) -> Result<Vec<TmdbCandidate>, String> {
        let key = format!("{media_type}|{}|{}", title.to_lowercase(), year.unwrap_or(0));
        if let Some(hit) = self.search_cache.get(&key) {
            return hit.clone();
        }
        let client = self
            .client
            .ok_or_else(|| "no TMDB key".to_string())?;
        let res = match media_type {
            "tv" => client.search_tv(title, year),
            _ => client.search_movies(title, year),
        };
        self.search_cache.insert(key, res.clone());
        res
    }

    fn season(
        &mut self,
        tv_id: i64,
        season: u32,
    ) -> Result<Vec<TmdbEpisode>, String> {
        if let Some(hit) = self.season_cache.get(&(tv_id, season)) {
            return hit.clone();
        }
        let client = self
            .client
            .ok_or_else(|| "no TMDB key".to_string())?;
        let res = client.season_episodes(tv_id, season);
        self.season_cache.insert((tv_id, season), res.clone());
        res
    }
}

/// The scan itself. Runs on a blocking thread (network + fs); the
/// catalog passed in is a short-lived connection, per house pattern.
pub fn scan(
    catalog: &Catalog,
    root: &Path,
    paths: &[String],
    key: Option<String>,
) -> Result<ScanResult, String> {
    let client = key.as_ref().map(|k| TmdbClient::new(k.clone()));
    let mut matcher = Matcher {
        client: client.as_ref(),
        search_cache: HashMap::new(),
        season_cache: HashMap::new(),
    };
    let (files, truncated) = expand_inputs(paths);
    let mut rows: Vec<ScanRow> = Vec::new();

    for path in &files {
        rows.push(scan_one(catalog, root, path, &mut matcher));
    }
    // Duplicate targets inside one scan: flag every row that shares.
    let mut by_target: HashMap<String, usize> = HashMap::new();
    for r in &rows {
        if let Some(t) = &r.target_rel {
            *by_target.entry(t.clone()).or_default() += 1;
        }
    }
    for r in &mut rows {
        if let Some(t) = &r.target_rel {
            if by_target.get(t).copied().unwrap_or(0) > 1 && r.state != "attention" {
                r.state = "attention".into();
                r.note = Some(
                    "another file in this folder wants the same server name — resolve the clash before organizing"
                        .into(),
                );
            }
        }
    }
    let (mut ready, mut fixable, mut attention) = (0u32, 0u32, 0u32);
    for r in &rows {
        match r.state.as_str() {
            "ready" => ready += 1,
            "fixable" => fixable += 1,
            _ => attention += 1,
        }
    }
    Ok(ScanResult {
        root: root.to_string_lossy().into_owned(),
        has_key: key.is_some(),
        rows,
        ready,
        fixable,
        attention,
        truncated,
    })
}

fn scan_one(
    catalog: &Catalog,
    root: &Path,
    path: &Path,
    matcher: &mut Matcher,
) -> ScanRow {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut parsed = videoid::parse_video_filename(&name);
    // Folder context: parent folder, then grandparent (a file inside
    // "Season 02" inside "Show (2019)" gets both).
    if let Some(parent) = path.parent() {
        if let Some(pn) = parent.file_name() {
            videoid::apply_folder_context(&mut parsed, &pn.to_string_lossy());
        }
        if let Some(gp) = parent.parent().and_then(|g| g.file_name()) {
            videoid::apply_folder_context(&mut parsed, &gp.to_string_lossy());
        }
    }

    let item = catalog
        .item_by_provider_key(&path.to_string_lossy())
        .ok()
        .flatten();
    let item_id = item.as_ref().map(|i| i.id.to_string());
    let stored = item
        .as_ref()
        .and_then(|i| catalog.media_identity_for(&i.id).ok().flatten())
        .filter(|r| r.media_kind == "movie" || r.media_kind == "episode");

    let mut note: Option<String> = None;
    let mut candidates: Vec<CandidateScore> = Vec::new();
    let mut identity: Option<IdentityView> = None;

    // 1. A confirmed identity is the user's truth — it wins.
    if let Some(row) = &stored {
        if row.confirmed {
            identity = identity_from_row(row, "stored");
        }
    }
    // 2. A fresh unconfirmed TMDB identity is reused without network.
    if identity.is_none() {
        if let Some(row) = &stored {
            if row.tmdb_id.is_some() && stored_is_fresh(row, path) {
                identity = identity_from_row(row, "stored");
            }
        }
    }
    // 3. TMDB matching (needs the key).
    if identity.is_none()
        && !parsed.flags.contains(&"date_based".to_string())
        && parsed.absolute_episode.is_none()
        && !parsed.title.is_empty()
    {
        if matcher.client.is_some() {
            match match_parsed(catalog, item.as_ref(), path, &parsed, matcher) {
                Ok((idn, cands)) => {
                    identity = idn;
                    candidates = cands;
                }
                Err(e) => {
                    note = Some(e);
                }
            }
        }
    }
    // 4. Keyless fallback: a complete parse is a name-derived
    //    identity the user can confirm by eye (source "name").
    if identity.is_none() {
        if let Some(row) = &stored {
            identity = identity_from_row(row, "stored");
        }
    }
    if identity.is_none() {
        let complete = match parsed.kind {
            ParsedKind::Movie => parsed.year.is_some(),
            ParsedKind::Episode => parsed.season.is_some() && parsed.episode_first.is_some(),
            ParsedKind::Unknown => false,
        };
        if complete {
            identity = Some(IdentityView {
                media_type: match parsed.kind {
                    ParsedKind::Episode => "episode".into(),
                    _ => "movie".into(),
                },
                tmdb_id: None,
                title: parsed.title.clone(),
                year: parsed.year.map(|y| y as i64),
                season: parsed.season.map(|s| s as i64),
                episode: parsed.episode_first.map(|e| e as i64),
                episode_end: parsed.episode_last.map(|e| e as i64),
                confidence: None,
                confirmed: false,
                source: "name".into(),
                episode_titles: Vec::new(),
            });
            if matcher.client.is_none() {
                note = Some(
                    "judged from the file name alone — add a free TMDB key in Preferences to verify titles"
                        .into(),
                );
            }
        }
    }
    if identity.is_none() && note.is_none() {
        note = Some(unmatched_note(&parsed, matcher.client.is_some()));
    }

    // Judge compliance against the canonical target.
    let ext = ext_of(path);
    let target_rel = identity
        .as_ref()
        .and_then(|idn| target_rel_for(&parsed, idn, &ext));
    let state = match (&identity, &target_rel) {
        (Some(_), Some(trel)) => {
            let current_rel = path.strip_prefix(root).unwrap_or(path);
            if current_rel == trel.as_path() {
                "ready"
            } else {
                "fixable"
            }
        }
        _ => "attention",
    };
    ScanRow {
        path: path.to_string_lossy().into_owned(),
        name,
        item_id,
        parsed,
        state: state.into(),
        identity,
        candidates,
        target_rel: target_rel.map(|t| t.to_string_lossy().into_owned()),
        note,
    }
}

fn unmatched_note(parsed: &ParsedVideo, has_key: bool) -> String {
    if parsed.flags.contains(&"date_based".to_string()) {
        return "date-based episode — servers number episodes by season, not air date. \
                Confirm the show, then use Correct to set its season and episode by hand."
            .into();
    }
    if parsed.absolute_episode.is_some() {
        return format!(
            "absolute episode number {} with no season — servers want S/E numbers. \
             Confirm the show, then use Correct if you know which season this is.",
            parsed.absolute_episode.unwrap_or(0)
        );
    }
    if parsed.title.is_empty() {
        return "nothing usable could be read from this file name — use Correct to say what it is"
            .into();
    }
    if !has_key {
        return "not enough in the file name to judge it, and no TMDB key is set — \
                add a free key in Preferences, or use Correct to identify it by hand"
            .into();
    }
    "TMDB has no confident match for this name — check the candidates, or use Correct".into()
}

/// Run TMDB matching for one parsed file; on a Probable-or-better
/// best candidate, persist the (unconfirmed) identity so rescans
/// are fresh-cheap and the confirm flow has something to write over.
fn match_parsed(
    catalog: &Catalog,
    item: Option<&ufm_vfs::Item>,
    path: &Path,
    parsed: &ParsedVideo,
    matcher: &mut Matcher,
) -> Result<(Option<IdentityView>, Vec<CandidateScore>), String> {
    let (media_type, search_kind) = match parsed.kind {
        ParsedKind::Episode => ("episode", "tv"),
        _ => ("movie", "movie"),
    };
    let found = matcher.search(search_kind, &parsed.title, parsed.year)?;
    if found.is_empty() {
        return Ok((None, Vec::new()));
    }
    let mut ranked = videoid::rank_candidates(parsed, &found);
    let mut ep_titles: Vec<String> = Vec::new();
    let mut verify_note: Option<String> = None;
    // Episode verification against the season's real episode list.
    if media_type == "episode" {
        if let (Some(best), Some(season)) = (ranked.first().cloned(), parsed.season) {
            if best.tier != Tier::Unknown {
                if let Ok(eps) = matcher.season(best.candidate.id, season) {
                    let (adj, note) = videoid::verify_episode(parsed, &eps);
                    verify_note = note;
                    if let Some(first) = ranked.first_mut() {
                        first.score = (first.score + adj).clamp(0.0, 1.0);
                        first.tier = videoid::tier_for(first.score);
                        if let Some(n) = &verify_note {
                            first.reasons.push(n.clone());
                        }
                    }
                    if let (Some(f), Some(l)) = (parsed.episode_first, parsed.episode_last) {
                        for n in f..=l {
                            if let Some(ep) = eps.iter().find(|e| e.episode_number == n) {
                                ep_titles.push(ep.name.clone());
                            }
                        }
                    }
                }
            }
        }
    }
    let top: Vec<CandidateScore> = ranked.iter().take(3).cloned().collect();
    let best = ranked.into_iter().next();
    let Some(best) = best else {
        return Ok((None, top));
    };
    if best.tier == Tier::Unknown {
        return Ok((None, top));
    }
    let cand = &best.candidate;
    let idn = IdentityView {
        media_type: media_type.into(),
        tmdb_id: Some(cand.id),
        title: cand.title.clone(),
        year: cand.year.map(|y| y as i64),
        season: parsed.season.map(|s| s as i64),
        episode: parsed.episode_first.map(|e| e as i64),
        episode_end: parsed.episode_last.map(|e| e as i64),
        confidence: Some(best.score as f64),
        confirmed: false,
        source: "scan".into(),
        episode_titles: ep_titles,
    };
    // Persist (unconfirmed) so a rescan is a freshness check, not a
    // new round of network calls. Only for catalogued files.
    if let Some(item) = item {
        let (size, modified) = fs_freshness(path);
        let evidence = serde_json::json!({
            "source": "tmdb-scan",
            "size": size,
            "modified": modified,
            "episode_titles": idn.episode_titles,
            "reasons": best.reasons,
        });
        let _ = catalog.upsert_video_identity(
            &item.id,
            &idn.media_type,
            Some(&idn.title),
            idn.year,
            idn.tmdb_id,
            idn.season,
            idn.episode,
            idn.episode_end,
            idn.confidence,
            false,
            &evidence.to_string(),
        );
    }
    Ok((Some(idn), top))
}

// ---------------------------------------------------------------------------
// Confirm / correct: the human writes the truth
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmChoice {
    pub media_type: String,
    pub tmdb_id: Option<i64>,
    pub title: String,
    pub year: Option<i64>,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    pub episode_end: Option<i64>,
    #[serde(default)]
    pub episode_titles: Vec<String>,
}

pub fn confirm(
    catalog: &Catalog,
    path: &Path,
    choice: &ConfirmChoice,
) -> Result<IdentityView, String> {
    let item = catalog
        .item_by_provider_key(&path.to_string_lossy())
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            "this file isn't in Fable's catalog yet — let the indexer see its folder first"
                .to_string()
        })?;
    if choice.title.trim().is_empty() {
        return Err("a title is required".into());
    }
    let (size, modified) = fs_freshness(path);
    let evidence = serde_json::json!({
        "source": if choice.tmdb_id.is_some() { "user-confirm" } else { "user-correct" },
        "size": size,
        "modified": modified,
        "episode_titles": choice.episode_titles,
    });
    catalog
        .upsert_video_identity(
            &item.id,
            &choice.media_type,
            Some(choice.title.trim()),
            choice.year,
            choice.tmdb_id,
            choice.season,
            choice.episode,
            choice.episode_end,
            Some(1.0),
            true,
            &evidence.to_string(),
        )
        .map_err(|e| e.to_string())?;
    Ok(IdentityView {
        media_type: choice.media_type.clone(),
        tmdb_id: choice.tmdb_id,
        title: choice.title.trim().to_string(),
        year: choice.year,
        season: choice.season,
        episode: choice.episode,
        episode_end: choice.episode_end,
        confidence: Some(1.0),
        confirmed: true,
        source: "stored".into(),
        episode_titles: choice.episode_titles.clone(),
    })
}

// ---------------------------------------------------------------------------
// Organize: plan (the preview IS this) + the eligibility law
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrganizeAction {
    pub src: String,
    pub dst: String,
    pub rel: String,
    /// The confirmed/Confident identity this move files under —
    /// the Organize job builder hangs FetchArtwork / FetchSubtitle
    /// steps off it (Media Completion).
    pub identity: IdentityView,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrganizeExclusion {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrganizePlan {
    pub actions: Vec<OrganizeAction>,
    pub excluded: Vec<OrganizeExclusion>,
}

/// Compute the organize plan for a set of files under `root`, from
/// STORED identities only (deterministic: the preview the user saw
/// is exactly what the job will do). Eligibility: confirmed, or a
/// Confident-tier TMDB match (≥ 0.85) — the Organize click after the
/// full preview is the batch-confirm for those, per the law.
pub fn plan_organize(
    catalog: &Catalog,
    root: &Path,
    paths: &[String],
) -> Result<OrganizePlan, String> {
    let mut actions: Vec<OrganizeAction> = Vec::new();
    let mut excluded: Vec<OrganizeExclusion> = Vec::new();
    let mut seen_targets: HashMap<String, String> = HashMap::new();
    let mut dup_targets: Vec<String> = Vec::new();

    for pstr in paths {
        let path = PathBuf::from(pstr);
        let exclude = |reason: &str| OrganizeExclusion {
            path: pstr.clone(),
            reason: reason.into(),
        };
        let item = catalog
            .item_by_provider_key(pstr)
            .map_err(|e| e.to_string())?;
        let Some(item) = item else {
            excluded.push(exclude("not in the catalog yet"));
            continue;
        };
        let row = catalog
            .media_identity_for(&item.id)
            .map_err(|e| e.to_string())?
            .filter(|r| r.media_kind == "movie" || r.media_kind == "episode");
        let Some(row) = row else {
            excluded.push(exclude("no match yet — resolve it first"));
            continue;
        };
        let eligible = row.confirmed
            || row
                .confidence
                .map(|c| c >= CONFIDENT_MIN)
                .unwrap_or(false);
        if !eligible {
            excluded.push(exclude(
                "this match needs your confirmation first (it isn't confident enough to move on its own)",
            ));
            continue;
        }
        let Some(idn) = identity_from_row(&row, "stored") else {
            excluded.push(exclude("the stored match has no title"));
            continue;
        };
        let parsed = videoid::parse_video_filename(
            &path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
        let Some(rel) = target_rel_for(&parsed, &idn, &ext_of(&path)) else {
            excluded.push(exclude("the stored match is incomplete"));
            continue;
        };
        let dst = root.join(&rel);
        if dst == path {
            excluded.push(exclude("already in its server place"));
            continue;
        }
        if std::fs::symlink_metadata(&dst).is_ok() {
            excluded.push(exclude(
                "a file already exists at the destination — skipped; Fable never overwrites",
            ));
            continue;
        }
        let key = dst.to_string_lossy().into_owned();
        if seen_targets.contains_key(&key) {
            dup_targets.push(key.clone());
        }
        seen_targets.insert(key, pstr.clone());
        actions.push(OrganizeAction {
            src: pstr.clone(),
            dst: dst.to_string_lossy().into_owned(),
            rel: rel.to_string_lossy().into_owned(),
            identity: idn,
        });
    }
    // Two sources wanting one destination: exclude BOTH (never pick
    // a winner silently).
    if !dup_targets.is_empty() {
        let dup: std::collections::HashSet<&String> = dup_targets.iter().collect();
        let mut kept = Vec::new();
        for a in actions {
            if dup.contains(&a.dst) {
                excluded.push(OrganizeExclusion {
                    path: a.src.clone(),
                    reason: "another file in this set wants the same destination".into(),
                });
            } else {
                kept.push(a);
            }
        }
        actions = kept;
    }
    Ok(OrganizePlan { actions, excluded })
}

#[cfg(test)]
mod tests;
