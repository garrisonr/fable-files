//! musiccomplete: Music Completion (Dickles, 2026-10-15) — the
//! music sibling of Media Completion. Media Completion gave films
//! their name + poster + subtitles; this gives a music library its
//! tags + cover art + lyrics, from free sources, under the same
//! laws:
//!
//! - PROPOSE, NEVER SILENTLY OVERWRITE. Matching (MusicBrainz)
//!   produces a per-track plan the user reviews field by field.
//!   The plan builder (ufm_media::musicid) fills ONLY empty tags;
//!   anything that would change a non-empty tag is an overwrite
//!   proposal that batch apply skips unless a human confirmed
//!   that exact field.
//! - JOURNALED WRITES. Every apply — interactive or scheduled —
//!   is a run row in the catalog whose actions journal stores the
//!   PRIOR tag values / lyric text for every file touched. Undo
//!   restores exactly what was there. Sidecars are never
//!   overwritten (an existing .lrc wins), embedded art is only
//!   ever added when a file has none.
//! - UNATTENDED AUTONOMY (the mediacomplete rule): a scheduled
//!   run completes Confident matches end-to-end and queues
//!   Probable/Unknown into the run's pending list — the morning
//!   checklist. It never guesses at 3 AM.
//! - FREE PATH FIRST-CLASS. MusicBrainz + Cover Art Archive +
//!   LRCLIB need no key and no account. Fingerprinting (AcoustID)
//!   is the only keyed extra, and only when the `fpcalc` tool is
//!   installed AND the user saved their own free AcoustID key;
//!   otherwise the status line says fingerprinting is off and
//!   tag/filename matching carries on — reduced mode is a note,
//!   never an error.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use ufm_catalog::Catalog;
use ufm_media::musicid::{
    self, AudioTags, LocalInfo, MbCandidate, TagChange, Tier,
};
use ufm_media::lrc;

pub use crate::mediacomplete::{catalog_path, data_base};

pub const AUDIO_EXTS: &[&str] = &["mp3", "flac", "ogg", "opus", "m4a", "m4b", "aac", "wav"];
const MAX_SCAN_FILES: usize = 500;
const USER_AGENT: &str = "FableFiles/1.0 ( https://github.com/garrisonr/fable-files )";

// ---------------------------------------------------------------------------
// Settings + schedules (JSON sidecar, atomic writes — house pattern)
// ---------------------------------------------------------------------------

fn settings_path(base: &Path) -> PathBuf {
    base.join("musiccomplete-settings.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MuSteps {
    pub tags: bool,
    pub lyrics: bool,
    pub artwork: bool,
}

impl Default for MuSteps {
    fn default() -> Self {
        Self {
            tags: true,
            lyrics: true,
            artwork: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MuSchedule {
    pub id: String,
    pub name: String,
    /// Drive or folder tree to complete.
    pub target: String,
    pub steps: MuSteps,
    /// Local time of day, "HH:MM".
    pub time: String,
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

impl MuSchedule {
    /// The mediacomplete scheduler state machine is the tested
    /// one; auditor converts its schedules the same way. The step
    /// flags ride the mc slots (tags→organize, artwork→artwork,
    /// lyrics→subtitles) purely so evaluate()/defer_reason() can
    /// be reused — run_scheduled() reads the MuSteps themselves.
    pub fn as_mc(&self) -> crate::mediacomplete::Schedule {
        crate::mediacomplete::Schedule {
            id: self.id.clone(),
            name: self.name.clone(),
            target: self.target.clone(),
            steps: crate::mediacomplete::ScheduleSteps {
                organize: self.steps.tags,
                artwork: self.steps.artwork,
                subtitles: self.steps.lyrics,
            },
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
pub struct MuSettings {
    /// Where fetched lyrics are saved: "embedded" | "sidecar" |
    /// "both". Embedded travels with the file; a .lrc sidecar is
    /// what most players read for synced lyrics.
    pub lyrics_target: String,
    /// Enrichment also fetches lyrics (LRCLIB) — default on.
    pub fetch_lyrics: bool,
    /// Enrichment also completes missing cover art — default on.
    pub fetch_artwork: bool,
    #[serde(default)]
    pub schedules: Vec<MuSchedule>,
}

impl Default for MuSettings {
    fn default() -> Self {
        Self {
            lyrics_target: "both".into(),
            fetch_lyrics: true,
            fetch_artwork: true,
            schedules: Vec::new(),
        }
    }
}

pub fn load_settings(base: &Path) -> MuSettings {
    std::fs::read_to_string(settings_path(base))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_settings(base: &Path, settings: &MuSettings) -> Result<(), String> {
    std::fs::create_dir_all(base).map_err(|e| e.to_string())?;
    let tmp = base.join("musiccomplete-settings.json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, settings_path(base)).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// AcoustID key — OS keyring only (optional, user-supplied, free).
// ---------------------------------------------------------------------------

fn acoustid_entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(crate::rommedia::KEYRING_SERVICE, "musiccomplete/acoustid")
        .map_err(|e| format!("system keyring unavailable: {e}"))
}

pub fn acoustid_key_get() -> Option<String> {
    let entry = acoustid_entry().ok()?;
    match entry.get_password() {
        Ok(v) if !v.is_empty() => Some(v),
        _ => None,
    }
}

pub fn acoustid_key_set(value: Option<&str>) -> Result<(), String> {
    let entry = acoustid_entry()?;
    match value {
        Some(v) if !v.trim().is_empty() => {
            entry
                .set_password(v.trim())
                .map_err(|e| format!("could not save to the system keyring: {e}"))?;
            crate::rommedia::verify_readback("musiccomplete/acoustid", v.trim())
        }
        _ => match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(format!("could not clear the saved key: {e}")),
        },
    }
}

pub fn fpcalc_path() -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|dir| dir.join("fpcalc"))
        .find(|p| p.is_file())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MuStatus {
    pub keyring_ok: bool,
    pub acoustid_key_saved: bool,
    /// Fingerprinting needs the chromaprint `fpcalc` tool.
    pub fpcalc_available: bool,
    /// True only when BOTH the tool and a key exist — the honest
    /// reduced-mode line in Preferences reads these fields.
    pub fingerprinting_ready: bool,
}

pub fn status() -> MuStatus {
    let fpcalc = fpcalc_path().is_some();
    let key = acoustid_key_get().is_some();
    MuStatus {
        keyring_ok: crate::rommedia::keyring_available(),
        acoustid_key_saved: key,
        fpcalc_available: fpcalc,
        fingerprinting_ready: fpcalc && key,
    }
}

// ---------------------------------------------------------------------------
// Tag I/O (lofty). Reads tolerate anything; writes go through one
// door so the journal's prior-value capture can't be bypassed.
// ---------------------------------------------------------------------------

fn field_key(field: &str) -> Option<lofty::tag::ItemKey> {
    use lofty::tag::ItemKey;
    Some(match field {
        "title" => ItemKey::TrackTitle,
        "artist" => ItemKey::TrackArtist,
        "album" => ItemKey::AlbumTitle,
        "albumArtist" => ItemKey::AlbumArtist,
        "year" => ItemKey::Year,
        "track" => ItemKey::TrackNumber,
        "trackTotal" => ItemKey::TrackTotal,
        "disc" => ItemKey::DiscNumber,
        "discTotal" => ItemKey::DiscTotal,
        "genre" => ItemKey::Genre,
        "composer" => ItemKey::Composer,
        _ => return None,
    })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTagsView {
    pub tags: AudioTags,
    pub has_art: bool,
    pub duration_secs: Option<f64>,
    /// Lyrics already in the file (plain text).
    pub lyrics_plain: Option<String>,
    /// Lyrics already in the file, LRC-formatted.
    pub lyrics_synced: Option<String>,
}

/// Read everything completion needs from one audio file.
pub fn read_file_tags(path: &Path) -> Result<FileTagsView, String> {
    use lofty::prelude::*;
    let file = lofty::read_from_path(path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let duration = file.properties().duration().as_secs_f64();
    let mut view = FileTagsView {
        tags: AudioTags::default(),
        has_art: false,
        duration_secs: if duration > 0.0 { Some(duration) } else { None },
        lyrics_plain: None,
        lyrics_synced: None,
    };
    if let Some(tag) = file.primary_tag() {
        for field in musicid::FIELDS {
            if let Some(key) = field_key(field) {
                if let Some(v) = tag.get_string(&key) {
                    view.tags.set(field, Some(v.to_string()));
                }
            }
        }
        // Combined "3/12" values (some formats store track+total in
        // one field): split into the pair when the total is unset.
        for (num, total) in [("track", "trackTotal"), ("disc", "discTotal")] {
            if let Some(v) = view.tags.get(num).map(str::to_string) {
                if v.contains('/') {
                    let (a, b) = v.split_once('/').unwrap();
                    view.tags.set(num, Some(a.trim().to_string()));
                    if view.tags.get(total).is_none() {
                        view.tags.set(total, Some(b.trim().to_string()));
                    }
                }
            }
        }
        view.has_art = !tag.pictures().is_empty();
        if let Some(lyr) = tag.get_string(&lofty::tag::ItemKey::Lyrics) {
            if lrc::looks_synced(lyr) {
                view.lyrics_synced = Some(lyr.to_string());
            } else if !lyr.trim().is_empty() {
                view.lyrics_plain = Some(lyr.to_string());
            }
        }
    }
    Ok(view)
}

fn open_for_write(
    path: &Path,
) -> Result<lofty::file::TaggedFile, String> {
    use lofty::prelude::*;
    let mut file = lofty::read_from_path(path)
        .map_err(|e| format!("could not open {}: {e}", path.display()))?;
    if file.primary_tag().is_none() {
        let tag_type = file.file_type().primary_tag_type();
        file.insert_tag(lofty::tag::Tag::new(tag_type));
    }
    Ok(file)
}

/// Write tag fields into a file. `changes` values: Some = set,
/// None = clear the field. Returns the values that were in the
/// file before the write (the undo journal's payload).
pub fn write_tag_fields(
    path: &Path,
    changes: &[(String, Option<String>)],
) -> Result<Vec<(String, Option<String>)>, String> {
    use lofty::prelude::*;
    let mut file = open_for_write(path)?;
    let tag = file
        .primary_tag_mut()
        .ok_or_else(|| "the file has no writable tag".to_string())?;
    let mut priors: Vec<(String, Option<String>)> = Vec::new();
    for (field, value) in changes {
        let Some(key) = field_key(field) else {
            return Err(format!("unknown tag field: {field}"));
        };
        priors.push((
            field.clone(),
            tag.get_string(&key).map(str::to_string),
        ));
        match value {
            Some(v) => {
                tag.insert_text(key, v.clone());
            }
            None => tag.remove_key(&key),
        }
    }
    file.save_to_path(path, lofty::config::WriteOptions::default())
        .map_err(|e| format!("could not save tags to {}: {e}", path.display()))?;
    Ok(priors)
}

/// Replace the embedded lyrics text (None = remove). Returns the
/// prior embedded lyrics text.
pub fn write_embedded_lyrics(
    path: &Path,
    text: Option<&str>,
) -> Result<Option<String>, String> {
    use lofty::prelude::*;
    let mut file = open_for_write(path)?;
    let tag = file
        .primary_tag_mut()
        .ok_or_else(|| "the file has no writable tag".to_string())?;
    let key = lofty::tag::ItemKey::Lyrics;
    let prior = tag.get_string(&key).map(str::to_string);
    match text {
        Some(t) => {
            tag.insert_text(key, t.to_string());
        }
        None => tag.remove_key(&key),
    }
    file.save_to_path(path, lofty::config::WriteOptions::default())
        .map_err(|e| format!("could not save lyrics to {}: {e}", path.display()))?;
    Ok(prior)
}

/// Embed cover art — ONLY when the file has none (existing art
/// always wins). Returns true when art was embedded.
pub fn embed_art(path: &Path, bytes: &[u8]) -> Result<bool, String> {
    use lofty::prelude::*;
    let mut file = open_for_write(path)?;
    let tag = file
        .primary_tag_mut()
        .ok_or_else(|| "the file has no writable tag".to_string())?;
    if !tag.pictures().is_empty() {
        return Ok(false);
    }
    let mime = if bytes.starts_with(&[0xFF, 0xD8]) {
        Some(lofty::picture::MimeType::Jpeg)
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some(lofty::picture::MimeType::Png)
    } else {
        None
    };
    tag.push_picture(lofty::picture::Picture::new_unchecked(
        lofty::picture::PictureType::CoverFront,
        mime,
        None,
        bytes.to_vec(),
    ));
    file.save_to_path(path, lofty::config::WriteOptions::default())
        .map_err(|e| format!("could not embed artwork in {}: {e}", path.display()))?;
    Ok(true)
}

/// Remove ALL embedded pictures (the undo of embed_art — art is
/// only ever embedded into files that had none).
pub fn remove_embedded_art(path: &Path) -> Result<(), String> {
    use lofty::prelude::*;
    let mut file = open_for_write(path)?;
    let tag = file
        .primary_tag_mut()
        .ok_or_else(|| "the file has no writable tag".to_string())?;
    while !tag.pictures().is_empty() {
        tag.remove_picture(0);
    }
    file.save_to_path(path, lofty::config::WriteOptions::default())
        .map_err(|e| format!("could not update {}: {e}", path.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// MusicBrainz client (ureq; 1 request/second politeness — the
// published etiquette; a shared throttle so a batch scan can
// never outrun it).
// ---------------------------------------------------------------------------

pub struct MbClient {
    agent: ureq::Agent,
    last_call: Mutex<std::time::Instant>,
}

impl MbClient {
    pub fn new() -> Self {
        Self {
            agent: ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(25))
                .build(),
            last_call: Mutex::new(
                std::time::Instant::now() - std::time::Duration::from_secs(2),
            ),
        }
    }

    fn throttle(&self) {
        let mut last = self.last_call.lock().unwrap();
        let wait = std::time::Duration::from_millis(1050).saturating_sub(last.elapsed());
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
        *last = std::time::Instant::now();
    }

    fn get_json(&self, url: &str) -> Result<serde_json::Value, String> {
        self.throttle();
        let resp = self.agent.get(url).set("User-Agent", USER_AGENT).call();
        match resp {
            Ok(r) => {
                let text = r
                    .into_string()
                    .map_err(|e| format!("MusicBrainz sent an unreadable reply: {e}"))?;
                serde_json::from_str(&text)
                    .map_err(|e| format!("MusicBrainz sent an unreadable reply: {e}"))
            }
            Err(ureq::Error::Status(503, _)) => Err(
                "MusicBrainz is busy right now — give it a minute and try again.".into(),
            ),
            Err(ureq::Error::Status(404, _)) => Err("not found on MusicBrainz".into()),
            Err(ureq::Error::Status(code, _)) => {
                Err(format!("MusicBrainz answered with an error (HTTP {code})."))
            }
            Err(e) => Err(format!(
                "couldn't reach MusicBrainz ({e}) — check the network connection."
            )),
        }
    }

    /// Search recordings by the strongest local identity we have.
    pub fn search(&self, local: &LocalInfo) -> Result<Vec<MbCandidate>, String> {
        let mut clauses: Vec<String> = Vec::new();
        if let Some(t) = &local.title {
            clauses.push(format!("recording:\"{}\"", lucene_escape(t)));
        }
        if let Some(a) = &local.artist {
            clauses.push(format!("artist:\"{}\"", lucene_escape(a)));
        }
        if let Some(al) = &local.album {
            clauses.push(format!("release:\"{}\"", lucene_escape(al)));
        }
        if clauses.is_empty() {
            return Ok(Vec::new());
        }
        let query = clauses.join(" AND ");
        let url = format!(
            "https://musicbrainz.org/ws/2/recording/?query={}&fmt=json&limit=8",
            url_encode(&query)
        );
        let json = self.get_json(&url)?;
        if let Some(err) = json.get("error").and_then(|e| e.as_str()) {
            return Err(format!("MusicBrainz said: {err}"));
        }
        Ok(musicid::map_search_response(&json))
    }

    /// Enrich one chosen candidate from its recording lookup
    /// (track/disc numbers, genres, release media).
    pub fn lookup(&self, cand: &mut MbCandidate) -> Result<(), String> {
        let url = format!(
            "https://musicbrainz.org/ws/2/recording/{}?inc=releases+genres+artist-credits&fmt=json",
            cand.id
        );
        let json = self.get_json(&url)?;
        musicid::merge_lookup(cand, &json);
        Ok(())
    }
}

fn lucene_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '+' | '-' | '&' | '|' | '!' | '(' | ')' | '{' | '}' | '[' | ']' | '^' | '"' | '~'
                | '*' | '?' | ':' | '\\' | '/'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// LRCLIB client (free, no key): /api/get first (exact match), then
// /api/search (best duration + synced availability wins).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricsHit {
    pub plain: Option<String>,
    pub synced: Option<String>,
    pub instrumental: bool,
}

pub struct LrclibClient {
    agent: ureq::Agent,
}

impl LrclibClient {
    pub fn new() -> Self {
        Self {
            agent: ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(20))
                .build(),
        }
    }

    fn get_json(&self, url: &str) -> Result<Option<serde_json::Value>, String> {
        match self.agent.get(url).set("User-Agent", USER_AGENT).call() {
            Ok(r) => {
                let text = r
                    .into_string()
                    .map_err(|e| format!("LRCLIB sent an unreadable reply: {e}"))?;
                serde_json::from_str(&text)
                    .map(Some)
                    .map_err(|e| format!("LRCLIB sent an unreadable reply: {e}"))
            }
            Err(ureq::Error::Status(404, _)) => Ok(None),
            Err(ureq::Error::Status(code, _)) => {
                Err(format!("LRCLIB answered with an error (HTTP {code})."))
            }
            Err(e) => Err(format!(
                "couldn't reach lrclib.net ({e}) — lyrics lookup needs the network."
            )),
        }
    }

    fn hit_of(json: &serde_json::Value) -> Option<LyricsHit> {
        let plain = json
            .get("plainLyrics")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string);
        let synced = json
            .get("syncedLyrics")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string);
        let instrumental = json
            .get("instrumental")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if plain.is_none() && synced.is_none() && !instrumental {
            return None;
        }
        Some(LyricsHit {
            plain,
            synced,
            instrumental,
        })
    }

    /// Fetch lyrics for one track. Ok(None) = LRCLIB has none.
    pub fn fetch(
        &self,
        artist: &str,
        title: &str,
        album: Option<&str>,
        duration_secs: Option<f64>,
    ) -> Result<Option<LyricsHit>, String> {
        let mut url = format!(
            "https://lrclib.net/api/get?artist_name={}&track_name={}",
            url_encode(artist),
            url_encode(title)
        );
        if let Some(a) = album {
            url.push_str(&format!("&album_name={}", url_encode(a)));
        }
        if let Some(d) = duration_secs {
            url.push_str(&format!("&duration={}", d.round() as u64));
        }
        if let Some(json) = self.get_json(&url)? {
            if let Some(hit) = Self::hit_of(&json) {
                return Ok(Some(hit));
            }
        }
        // Search fallback: pick the closest duration that has
        // lyrics, preferring synced.
        let surl = format!(
            "https://lrclib.net/api/search?artist_name={}&track_name={}",
            url_encode(artist),
            url_encode(title)
        );
        let Some(json) = self.get_json(&surl)? else {
            return Ok(None);
        };
        let Some(results) = json.as_array() else {
            return Ok(None);
        };
        let mut best: Option<(f64, LyricsHit)> = None;
        for r in results {
            let Some(hit) = Self::hit_of(r) else { continue };
            let dur = r.get("duration").and_then(|d| d.as_f64());
            let delta = match (duration_secs, dur) {
                (Some(a), Some(b)) => (a - b).abs(),
                _ => 30.0,
            };
            let mut rank = 100.0 - delta.min(100.0);
            if hit.synced.is_some() {
                rank += 25.0;
            }
            if best.as_ref().map(|(b, _)| rank > *b).unwrap_or(true) {
                best = Some((rank, hit));
            }
        }
        Ok(best.map(|(_, h)| h))
    }
}

// ---------------------------------------------------------------------------
// Cover Art Archive: front cover for a release, release-group
// fallback. Bytes validated by magic number (never trust the
// redirect chain's content type alone).
// ---------------------------------------------------------------------------

pub fn fetch_cover_art(release_id: Option<&str>, release_group_id: Option<&str>) -> Option<Vec<u8>> {
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(25))
        .build();
    let mut urls: Vec<String> = Vec::new();
    if let Some(id) = release_id {
        urls.push(format!(
            "https://coverartarchive.org/release/{id}/front-500"
        ));
    }
    if let Some(id) = release_group_id {
        urls.push(format!(
            "https://coverartarchive.org/release-group/{id}/front-500"
        ));
    }
    for url in urls {
        let Ok(resp) = agent.get(&url).set("User-Agent", USER_AGENT).call() else {
            continue;
        };
        use std::io::Read;
        let mut buf = Vec::new();
        if resp
            .into_reader()
            .take(8 * 1024 * 1024)
            .read_to_end(&mut buf)
            .is_err()
        {
            continue;
        }
        let is_image = buf.starts_with(&[0xFF, 0xD8])
            || buf.starts_with(&[0x89, b'P', b'N', b'G'])
            || (buf.len() > 12 && &buf[0..4] == b"RIFF" && &buf[8..12] == b"WEBP");
        if is_image && buf.len() > 1024 {
            return Some(buf);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// AcoustID fingerprinting — only when fpcalc exists AND a key is
// saved. Returns the MusicBrainz recording id of the best match.
// ---------------------------------------------------------------------------

pub fn acoustid_lookup(path: &Path, key: &str) -> Result<Option<(String, f64)>, String> {
    let Some(fpcalc) = fpcalc_path() else {
        return Err("the fpcalc fingerprint tool is not installed".into());
    };
    let out = std::process::Command::new(&fpcalc)
        .arg("-json")
        .arg(path)
        .output()
        .map_err(|e| format!("couldn't run fpcalc: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "fpcalc couldn't fingerprint {}",
            path.display()
        ));
    }
    let parsed: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("fpcalc said: {e}"))?;
    let duration = parsed.get("duration").and_then(|d| d.as_f64()).unwrap_or(0.0);
    let Some(fingerprint) = parsed.get("fingerprint").and_then(|f| f.as_str()) else {
        return Err("fpcalc returned no fingerprint".into());
    };
    let url = format!(
        "https://api.acoustid.org/v2/lookup?client={}&meta=recordings&duration={}&fingerprint={}",
        url_encode(key),
        duration.round() as u64,
        url_encode(fingerprint)
    );
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(20))
        .build();
    let resp = agent
        .get(&url)
        .set("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| format!("couldn't reach AcoustID: {e}"))?;
    let text = resp
        .into_string()
        .map_err(|e| format!("AcoustID sent an unreadable reply: {e}"))?;
    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("AcoustID sent an unreadable reply: {e}"))?;
    if json.get("status").and_then(|s| s.as_str()) != Some("ok") {
        let msg = json
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error");
        return Err(format!("AcoustID said: {msg}"));
    }
    let mut best: Option<(String, f64)> = None;
    if let Some(results) = json.get("results").and_then(|r| r.as_array()) {
        for r in results {
            let score = r.get("score").and_then(|s| s.as_f64()).unwrap_or(0.0);
            if let Some(recs) = r.get("recordings").and_then(|x| x.as_array()) {
                if let Some(id) = recs.first().and_then(|x| x.get("id")).and_then(|i| i.as_str()) {
                    if best.as_ref().map(|(_, s)| score > *s).unwrap_or(true) {
                        best = Some((id.to_string(), score));
                    }
                }
            }
        }
    }
    Ok(best)
}

// ---------------------------------------------------------------------------
// Scanning + proposals. One pass per file: read tags, match on
// MusicBrainz (tags first, filename second, fingerprint last),
// plan the fills, probe LRCLIB. Everything the review surface
// shows comes from here; apply() consumes the same shapes.
// ---------------------------------------------------------------------------

fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Expand files/folders into a sorted, deduped audio file list.
fn expand_inputs(paths: &[String]) -> (Vec<PathBuf>, bool) {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut truncated = false;
    fn walk(dir: &Path, out: &mut Vec<PathBuf>, truncated: &mut bool) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if out.len() >= MAX_SCAN_FILES {
                *truncated = true;
                return;
            }
            if p.is_dir() {
                walk(&p, out, truncated);
            } else if is_audio(&p) {
                out.push(p);
            }
        }
    }
    for raw in paths {
        let p = PathBuf::from(raw);
        if p.is_dir() {
            walk(&p, &mut out, &mut truncated);
        } else if is_audio(&p) {
            out.push(p);
        }
    }
    out.sort();
    out.dedup();
    (out, truncated)
}

pub fn sidecar_path(audio: &Path) -> PathBuf {
    audio.with_extension("lrc")
}

fn sidecar_text(audio: &Path) -> Option<String> {
    std::fs::read_to_string(sidecar_path(audio))
        .ok()
        .filter(|s| !s.trim().is_empty())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateView {
    pub candidate: MbCandidate,
    pub score: f64,
    /// "confident" | "probable" | "unknown"
    pub tier: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackProposal {
    pub path: String,
    pub name: String,
    pub current: AudioTags,
    pub duration_secs: Option<f64>,
    /// "confident" | "probable" | "unknown" | "unmatched"
    pub state: String,
    pub candidate: Option<CandidateView>,
    pub changes: Vec<TagChange>,
    pub has_art: bool,
    /// A cover exists upstream and the file has none.
    pub art_available: bool,
    /// Lyrics already saved (embedded or sidecar).
    pub has_lyrics: bool,
    /// LRCLIB has lyrics this file doesn't.
    pub lyrics_available: bool,
    pub fetched_plain: Option<String>,
    pub fetched_synced: Option<String>,
    /// True when there is at least one thing completion would do.
    pub needs_work: bool,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanView {
    pub root: String,
    pub rows: Vec<TrackProposal>,
    pub truncated: bool,
    pub fingerprinting_ready: bool,
}

struct Proposers {
    mb: MbClient,
    lrclib: LrclibClient,
    acoustid_key: Option<String>,
    fingerprinting_ready: bool,
}

impl Proposers {
    fn new() -> Self {
        let key = acoustid_key_get();
        let ready = fpcalc_path().is_some() && key.is_some();
        Self {
            mb: MbClient::new(),
            lrclib: LrclibClient::new(),
            acoustid_key: key,
            fingerprinting_ready: ready,
        }
    }

    fn best_match(
        &self,
        path: &Path,
        local: &LocalInfo,
        parsed: &musicid::ParsedMusicName,
    ) -> Option<CandidateView> {
        let mut best: Option<CandidateView> = None;
        fn consider(
            best: &mut Option<CandidateView>,
            cands: Vec<MbCandidate>,
            local: &LocalInfo,
        ) {
            for cand in cands {
                let (score, evidence) = musicid::score_candidate(local, &cand);
                let view = CandidateView {
                    candidate: cand,
                    score,
                    tier: tier_name(musicid::tier_for(score)),
                    evidence,
                };
                if best.as_ref().map(|b| view.score > b.score).unwrap_or(true) {
                    *best = Some(view);
                }
            }
        }
        if local.title.is_some() {
            if let Ok(cands) = self.mb.search(local) {
                consider(&mut best, cands, local);
            }
        }
        // Second door: the filename's own identity, when the tags
        // produced nothing Probable or better.
        let weak = best
            .as_ref()
            .map(|b| b.score < musicid::PROBABLE_MIN)
            .unwrap_or(true);
        if weak && (parsed.title.is_some() || parsed.artist.is_some()) {
            let plocal = LocalInfo {
                title: parsed.title.clone().or_else(|| local.title.clone()),
                artist: parsed.artist.clone().or_else(|| local.artist.clone()),
                album: parsed.album.clone().or_else(|| local.album.clone()),
                duration_secs: local.duration_secs,
            };
            if plocal.title.is_some() {
                if let Ok(cands) = self.mb.search(&plocal) {
                    consider(&mut best, cands, &plocal);
                }
            }
        }
        // Last door: fingerprint. Only when it's actually ready —
        // absence is a reduced mode, not a failure.
        let weak = best
            .as_ref()
            .map(|b| b.score < musicid::PROBABLE_MIN)
            .unwrap_or(true);
        if weak && self.fingerprinting_ready {
            if let Some(key) = &self.acoustid_key {
                if let Ok(Some((rec_id, a_score))) = acoustid_lookup(path, key) {
                    if let Some(mut cand) = self.candidate_by_id(&rec_id) {
                        cand.mb_score = (a_score * 100.0).round() as u8;
                        let (score, mut evidence) =
                            musicid::score_candidate(local, &cand);
                        evidence.insert(
                            0,
                            format!(
                                "identified by audio fingerprint (AcoustID, {:.0}% match)",
                                a_score * 100.0
                            ),
                        );
                        let view = CandidateView {
                            candidate: cand,
                            score,
                            tier: tier_name(musicid::tier_for(score)),
                            evidence,
                        };
                        if best.as_ref().map(|b| view.score > b.score).unwrap_or(true) {
                            best = Some(view);
                        }
                    }
                }
            }
        }
        best
    }

    fn candidate_by_id(&self, rec_id: &str) -> Option<MbCandidate> {
        let url = format!(
            "https://musicbrainz.org/ws/2/recording/{rec_id}?inc=releases+genres+artist-credits&fmt=json"
        );
        let json = self.mb.get_json(&url).ok()?;
        let wrapped = serde_json::json!({ "recordings": [json] });
        let mut cand = musicid::map_search_response(&wrapped).into_iter().next()?;
        musicid::merge_lookup(&mut cand, &json);
        Some(cand)
    }

    fn propose_one(&self, path: &Path, settings: &MuSettings) -> TrackProposal {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut row = TrackProposal {
            path: path.to_string_lossy().into_owned(),
            name: name.clone(),
            current: AudioTags::default(),
            duration_secs: None,
            state: "unmatched".into(),
            candidate: None,
            changes: Vec::new(),
            has_art: false,
            art_available: false,
            has_lyrics: false,
            lyrics_available: false,
            fetched_plain: None,
            fetched_synced: None,
            needs_work: false,
            note: None,
        };
        let view = match read_file_tags(path) {
            Ok(v) => v,
            Err(e) => {
                row.note = Some(e);
                return row;
            }
        };
        row.current = view.tags.clone();
        row.duration_secs = view.duration_secs;
        row.has_art = view.has_art;
        let sidecar = sidecar_text(path);
        row.has_lyrics =
            view.lyrics_plain.is_some() || view.lyrics_synced.is_some() || sidecar.is_some();

        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let parsed = musicid::parse_music_filename(&stem);
        let local = LocalInfo {
            title: view.tags.get("title").map(str::to_string).or(parsed.title.clone()),
            artist: view
                .tags
                .get("artist")
                .map(str::to_string)
                .or(parsed.artist.clone()),
            album: view.tags.get("album").map(str::to_string).or(parsed.album.clone()),
            duration_secs: view.duration_secs,
        };

        let mut best = self.best_match(path, &local, &parsed);
        if let Some(b) = &mut best {
            if b.score >= musicid::PROBABLE_MIN {
                // Enrich the winner (track/disc numbers live in the
                // lookup, not the search results). A failed lookup
                // is not fatal — the search data still plans.
                let _ = self.mb.lookup(&mut b.candidate);
                row.changes = musicid::plan_changes(&view.tags, &b.candidate);
                row.art_available = settings.fetch_artwork
                    && !view.has_art
                    && (b.candidate.release_id.is_some()
                        || b.candidate.release_group_id.is_some());
            }
            row.state = b.tier.clone();
            row.candidate = Some(b.clone());
        }
        if row.candidate.is_none() {
            row.note = Some(if local.title.is_none() {
                "no title in the tags or the filename to search by — add one by hand, or drop the file in a folder named for the album".into()
            } else {
                "no match on MusicBrainz for this one".into()
            });
        }

        // Lyrics probe: prefer the matched identity (it is the
        // track we're completing toward), else the local one.
        if settings.fetch_lyrics && !row.has_lyrics {
            let (artist, title, album) = match &row.candidate {
                Some(c) if c.score >= musicid::PROBABLE_MIN => (
                    c.candidate.artist.clone(),
                    c.candidate.title.clone(),
                    c.candidate.album.clone(),
                ),
                _ => match (local.artist.clone(), local.title.clone()) {
                    (Some(a), Some(t)) => (a, t, local.album.clone()),
                    _ => (String::new(), String::new(), None),
                },
            };
            if !artist.is_empty() && !title.is_empty() {
                match self.lrclib.fetch(&artist, &title, album.as_deref(), view.duration_secs) {
                    Ok(Some(hit)) => {
                        row.lyrics_available =
                            hit.plain.is_some() || hit.synced.is_some();
                        row.fetched_plain = hit.plain;
                        row.fetched_synced = hit.synced;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        if row.note.is_none() {
                            row.note = Some(e);
                        }
                    }
                }
            }
        }
        row.needs_work = !row.changes.is_empty() || row.art_available || row.lyrics_available;
        row
    }
}

fn tier_name(t: Tier) -> String {
    match t {
        Tier::Confident => "confident",
        Tier::Probable => "probable",
        Tier::Unknown => "unknown",
    }
    .to_string()
}

pub fn propose_paths(
    root: &Path,
    paths: &[String],
    settings: &MuSettings,
) -> Result<ScanView, String> {
    let (files, truncated) = expand_inputs(paths);
    let proposers = Proposers::new();
    let mut rows = Vec::new();
    for f in &files {
        rows.push(proposers.propose_one(f, settings));
    }
    Ok(ScanView {
        root: root.to_string_lossy().into_owned(),
        rows,
        truncated,
        fingerprinting_ready: proposers.fingerprinting_ready,
    })
}

// ---------------------------------------------------------------------------
// Apply: the journaled write path. Both doors (review surface,
// scheduled run) funnel through apply_core; the run row exists
// BEFORE the first byte changes, and every file's prior values
// land in its actions journal as the write happens.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyField {
    pub field: String,
    /// None = clear the field (only ever from an explicit edit).
    pub value: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyItem {
    pub path: String,
    #[serde(default)]
    pub fields: Vec<ApplyField>,
    pub art_release_id: Option<String>,
    pub art_release_group_id: Option<String>,
    /// Lyrics text approved for saving (per the settings target).
    pub lyrics_plain: Option<String>,
    pub lyrics_synced: Option<String>,
}

fn now_local() -> chrono::NaiveDateTime {
    chrono::Local::now().naive_local()
}

fn stamp(t: chrono::NaiveDateTime) -> String {
    t.format("%Y-%m-%dT%H:%M:%S").to_string()
}

/// The lyrics text that gets embedded / sidecar'd: synced when we
/// have it (players that can't sync just read the stamps as text
/// in the worst case — and the .lrc IS the sync format), else
/// plain.
fn lyrics_payload(item: &ApplyItem) -> Option<String> {
    item.lyrics_synced
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| item.lyrics_plain.clone().filter(|s| !s.trim().is_empty()))
}

pub fn apply_core(
    catalog: &Catalog,
    run_name: &str,
    schedule_id: Option<String>,
    target: &str,
    items: &[ApplyItem],
    pending: Vec<serde_json::Value>,
    settings: &MuSettings,
    was_late: bool,
) -> Result<ufm_catalog::MusicCompleteRunRow, String> {
    let started = now_local();
    let mut row = ufm_catalog::MusicCompleteRunRow {
        id: uuid::Uuid::new_v4().to_string(),
        schedule_id,
        schedule_name: run_name.to_string(),
        target: target.to_string(),
        started: stamp(started),
        finished: None,
        was_late,
        status: "running".into(),
        summary_json: "{}".into(),
        actions_json: "[]".into(),
        pending_json: serde_json::Value::Array(pending.clone()).to_string(),
        undone_at: None,
    };
    catalog
        .upsert_musiccomplete_run(&row)
        .map_err(|e| e.to_string())?;

    let mut actions: Vec<serde_json::Value> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    let (mut files_done, mut fields_written, mut art_done, mut lyrics_done) =
        (0u32, 0u32, 0u32, 0u32);

    for item in items {
        let path = PathBuf::from(&item.path);
        let mut touched = false;
        // Tags.
        if !item.fields.is_empty() {
            let changes: Vec<(String, Option<String>)> = item
                .fields
                .iter()
                .map(|f| (f.field.clone(), f.value.clone()))
                .collect();
            match write_tag_fields(&path, &changes) {
                Ok(priors) => {
                    let mut prior_obj = serde_json::Map::new();
                    for (f, v) in priors {
                        prior_obj.insert(f, v.into());
                    }
                    actions.push(serde_json::json!({
                        "kind": "tags", "path": item.path,
                        "prior": serde_json::Value::Object(prior_obj),
                    }));
                    fields_written += changes.len() as u32;
                    touched = true;
                }
                Err(e) => failures.push(format!("{}: {e}", item.path)),
            }
        }
        // Artwork (only into a file that has none).
        if item.art_release_id.is_some() || item.art_release_group_id.is_some() {
            match read_file_tags(&path) {
                Ok(v) if v.has_art => {}
                Ok(_) => match fetch_cover_art(
                    item.art_release_id.as_deref(),
                    item.art_release_group_id.as_deref(),
                ) {
                    Some(bytes) => match embed_art(&path, &bytes) {
                        Ok(true) => {
                            actions.push(serde_json::json!({
                                "kind": "artwork", "path": item.path,
                            }));
                            art_done += 1;
                            touched = true;
                        }
                        Ok(false) => {}
                        Err(e) => failures.push(format!("{}: artwork: {e}", item.path)),
                    },
                    None => failures.push(format!(
                        "{}: the Cover Art Archive has no cover for this release",
                        item.path
                    )),
                },
                Err(e) => failures.push(format!("{}: {e}", item.path)),
            }
        }
        // Lyrics.
        if let Some(text) = lyrics_payload(item) {
            if settings.lyrics_target == "embedded" || settings.lyrics_target == "both" {
                match write_embedded_lyrics(&path, Some(&text)) {
                    Ok(prior) => {
                        actions.push(serde_json::json!({
                            "kind": "lyrics-embedded", "path": item.path, "prior": prior,
                        }));
                        lyrics_done += 1;
                        touched = true;
                    }
                    Err(e) => failures.push(format!("{}: lyrics: {e}", item.path)),
                }
            }
            if settings.lyrics_target == "sidecar" || settings.lyrics_target == "both" {
                let sc = sidecar_path(&path);
                if sc.exists() {
                    // An existing sidecar is the user's — it wins.
                } else if std::fs::write(&sc, &text).is_ok() {
                    actions.push(serde_json::json!({
                        "kind": "lyrics-sidecar", "path": sc.to_string_lossy(),
                    }));
                    lyrics_done += 1;
                    touched = true;
                } else {
                    failures.push(format!(
                        "{}: could not write the .lrc sidecar",
                        item.path
                    ));
                }
            }
        }
        if touched {
            files_done += 1;
        }
    }

    let summary = serde_json::json!({
        "filesCompleted": files_done,
        "fieldsWritten": fields_written,
        "artworkEmbedded": art_done,
        "lyricsSaved": lyrics_done,
        "pending": pending.len(),
        "failures": failures,
    });
    row.summary_json = summary.to_string();
    row.actions_json = serde_json::Value::Array(actions).to_string();
    row.status = if failures.is_empty() {
        "completed"
    } else {
        "completed_with_failures"
    }
    .into();
    row.finished = Some(stamp(now_local()));
    catalog
        .upsert_musiccomplete_run(&row)
        .map_err(|e| e.to_string())?;
    let _ = catalog.prune_musiccomplete_runs(50);
    Ok(row)
}

/// Interactive door: the review surface's approved items.
pub fn apply_review(
    target: &str,
    items: &[ApplyItem],
    settings: &MuSettings,
) -> Result<ufm_catalog::MusicCompleteRunRow, String> {
    let catalog = Catalog::open(&catalog_path()).map_err(|e| e.to_string())?;
    apply_core(
        &catalog,
        "Manual completion",
        None,
        target,
        items,
        Vec::new(),
        settings,
        false,
    )
}

// ---------------------------------------------------------------------------
// Undo: walk the run's journal backwards and restore priors.
// ---------------------------------------------------------------------------

pub fn undo_run(run_id: &str) -> Result<String, String> {
    let catalog = Catalog::open(&catalog_path()).map_err(|e| e.to_string())?;
    undo_run_with(&catalog, run_id)
}

pub fn undo_run_with(catalog: &Catalog, run_id: &str) -> Result<String, String> {
    let mut row = catalog
        .musiccomplete_run(run_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no such run".to_string())?;
    if row.undone_at.is_some() {
        return Err("this run was already undone".into());
    }
    let actions: Vec<serde_json::Value> =
        serde_json::from_str(&row.actions_json).unwrap_or_default();
    let (mut reversed, mut problems) = (0u32, Vec::new());
    for a in actions.iter().rev() {
        let Some(path) = a.get("path").and_then(|p| p.as_str()) else {
            continue;
        };
        let p = PathBuf::from(path);
        match a.get("kind").and_then(|k| k.as_str()) {
            Some("tags") => {
                let mut changes: Vec<(String, Option<String>)> = Vec::new();
                if let Some(prior) = a.get("prior").and_then(|p| p.as_object()) {
                    for field in musicid::FIELDS {
                        if let Some(v) = prior.get(field) {
                            changes.push((
                                field.to_string(),
                                v.as_str().map(str::to_string),
                            ));
                        }
                    }
                }
                match write_tag_fields(&p, &changes) {
                    Ok(_) => reversed += 1,
                    Err(e) => problems.push(format!("{path}: {e}")),
                }
            }
            Some("lyrics-embedded") => {
                let prior = a.get("prior").and_then(|v| v.as_str());
                match write_embedded_lyrics(&p, prior) {
                    Ok(_) => reversed += 1,
                    Err(e) => problems.push(format!("{path}: {e}")),
                }
            }
            Some("artwork") => match remove_embedded_art(&p) {
                Ok(()) => reversed += 1,
                Err(e) => problems.push(format!("{path}: {e}")),
            },
            Some("lyrics-sidecar") => match std::fs::remove_file(&p) {
                Ok(()) => reversed += 1,
                Err(e) => problems.push(format!("{path}: {e}")),
            },
            _ => {}
        }
    }
    row.undone_at = Some(stamp(now_local()));
    row.status = "undone".into();
    catalog
        .upsert_musiccomplete_run(&row)
        .map_err(|e| e.to_string())?;
    if problems.is_empty() {
        Ok(format!("Run undone — {reversed} change(s) reversed."))
    } else {
        Ok(format!(
            "Run undone with problems — {reversed} reversed, {} could not be: {}",
            problems.len(),
            problems.join("; ")
        ))
    }
}

// ---------------------------------------------------------------------------
// Lyrics Manager: per-file view + journaled save.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricsView {
    pub path: String,
    pub embedded_plain: Option<String>,
    pub embedded_synced: Option<String>,
    pub sidecar_text: Option<String>,
    pub sidecar_path: String,
    pub fetched_plain: Option<String>,
    pub fetched_synced: Option<String>,
    pub fetched_instrumental: bool,
    pub note: Option<String>,
}

pub fn lyrics_view(path: &Path, fetch: bool) -> Result<LyricsView, String> {
    let view = read_file_tags(path)?;
    let mut out = LyricsView {
        path: path.to_string_lossy().into_owned(),
        embedded_plain: view.lyrics_plain.clone(),
        embedded_synced: view.lyrics_synced.clone(),
        sidecar_text: sidecar_text(path),
        sidecar_path: sidecar_path(path).to_string_lossy().into_owned(),
        fetched_plain: None,
        fetched_synced: None,
        fetched_instrumental: false,
        note: None,
    };
    if fetch {
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let parsed = musicid::parse_music_filename(&stem);
        let artist = view
            .tags
            .get("artist")
            .map(str::to_string)
            .or(parsed.artist);
        let title = view.tags.get("title").map(str::to_string).or(parsed.title);
        match (artist, title) {
            (Some(a), Some(t)) => {
                let client = LrclibClient::new();
                match client.fetch(
                    &a,
                    &t,
                    view.tags.get("album"),
                    view.duration_secs,
                ) {
                    Ok(Some(hit)) => {
                        out.fetched_plain = hit.plain;
                        out.fetched_synced = hit.synced;
                        out.fetched_instrumental = hit.instrumental;
                        if out.fetched_plain.is_none()
                            && out.fetched_synced.is_none()
                            && !hit.instrumental
                        {
                            out.note =
                                Some("LRCLIB has no lyrics for this track yet.".into());
                        }
                    }
                    Ok(None) => {
                        out.note = Some("LRCLIB has no lyrics for this track yet.".into())
                    }
                    Err(e) => out.note = Some(e),
                }
            }
            _ => {
                out.note = Some(
                    "this file needs an artist and title (tags or filename) before LRCLIB can look it up — you can still type the lyrics in by hand"
                        .into(),
                )
            }
        }
    }
    Ok(out)
}

/// Save hand-edited or fetched lyrics for one file, journaled as
/// its own run (so even a lyrics edit is undoable). `text` is the
/// final text (LRC when synced); None clears embedded lyrics and
/// leaves any sidecar alone.
pub fn lyrics_save(
    path: &Path,
    text: Option<&str>,
    settings: &MuSettings,
) -> Result<String, String> {
    let catalog = Catalog::open(&catalog_path()).map_err(|e| e.to_string())?;
    lyrics_save_with(&catalog, path, text, settings)
}

pub fn lyrics_save_with(
    catalog: &Catalog,
    path: &Path,
    text: Option<&str>,
    settings: &MuSettings,
) -> Result<String, String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match text {
        Some(t) if !t.trim().is_empty() => {
            let item = ApplyItem {
                path: path.to_string_lossy().into_owned(),
                fields: Vec::new(),
                art_release_id: None,
                art_release_group_id: None,
                lyrics_plain: if lrc::looks_synced(t) {
                    None
                } else {
                    Some(t.to_string())
                },
                lyrics_synced: if lrc::looks_synced(t) {
                    Some(t.to_string())
                } else {
                    None
                },
            };
            let row = apply_core(
                &catalog,
                "Lyrics edit",
                None,
                &path.to_string_lossy(),
                &[item],
                Vec::new(),
                settings,
                false,
            )?;
            let failures: Vec<String> = serde_json::from_str::<serde_json::Value>(
                &row.summary_json,
            )
            .ok()
            .and_then(|s| {
                s.get("failures")
                    .and_then(|f| serde_json::from_value(f.clone()).ok())
            })
            .unwrap_or_default();
            if failures.is_empty() {
                Ok(format!("Lyrics saved for {name}."))
            } else {
                Err(failures.join("; "))
            }
        }
        _ => {
            // Clear embedded lyrics only, journaled.
            let mut row = ufm_catalog::MusicCompleteRunRow {
                id: uuid::Uuid::new_v4().to_string(),
                schedule_id: None,
                schedule_name: "Lyrics edit".into(),
                target: path.to_string_lossy().into_owned(),
                started: stamp(now_local()),
                finished: None,
                was_late: false,
                status: "running".into(),
                summary_json: "{}".into(),
                actions_json: "[]".into(),
                pending_json: "[]".into(),
                undone_at: None,
            };
            catalog
                .upsert_musiccomplete_run(&row)
                .map_err(|e| e.to_string())?;
            let prior = write_embedded_lyrics(path, None)?;
            if prior.is_some() {
                row.actions_json = serde_json::json!([{
                    "kind": "lyrics-embedded",
                    "path": path.to_string_lossy(),
                    "prior": prior,
                }])
                .to_string();
            }
            row.summary_json =
                serde_json::json!({ "lyricsSaved": 0, "failures": [] }).to_string();
            row.status = "completed".into();
            row.finished = Some(stamp(now_local()));
            catalog
                .upsert_musiccomplete_run(&row)
                .map_err(|e| e.to_string())?;
            Ok(format!("Embedded lyrics removed from {name}."))
        }
    }
}

// ---------------------------------------------------------------------------
// Scheduled runs + the agent tick (mediacomplete's loop calls
// agent_tick, exactly like the File Auditor's).
// ---------------------------------------------------------------------------

fn pending_entry(row: &TrackProposal) -> serde_json::Value {
    serde_json::json!({
        "path": row.path,
        "name": row.name,
        "state": row.state,
        "score": row.candidate.as_ref().map(|c| c.score),
        "candidate": row.candidate.as_ref().map(|c| serde_json::json!({
            "title": c.candidate.title,
            "artist": c.candidate.artist,
            "album": c.candidate.album,
            "year": c.candidate.year,
        })),
        "changes": row.changes,
        "artAvailable": row.art_available,
        "lyricsAvailable": row.lyrics_available,
        "note": row.note,
    })
}

pub fn run_scheduled(
    sched: &MuSchedule,
    late: bool,
) -> Result<ufm_catalog::MusicCompleteRunRow, String> {
    let base = data_base();
    let settings = load_settings(&base);
    let catalog = Catalog::open(&catalog_path()).map_err(|e| e.to_string())?;
    let scan = propose_paths(
        Path::new(&sched.target),
        &[sched.target.clone()],
        &settings,
    )?;
    let mut items: Vec<ApplyItem> = Vec::new();
    let mut pending: Vec<serde_json::Value> = Vec::new();
    for row in &scan.rows {
        if !row.needs_work {
            continue;
        }
        let confident = row.state == "confident";
        if !confident {
            pending.push(pending_entry(row));
            continue;
        }
        let cand = row.candidate.as_ref().unwrap();
        let fields: Vec<ApplyField> = if sched.steps.tags {
            row.changes
                .iter()
                .filter(|c| !c.overwrite)
                .map(|c| ApplyField {
                    field: c.field.clone(),
                    value: Some(c.proposed.clone()),
                })
                .collect()
        } else {
            Vec::new()
        };
        items.push(ApplyItem {
            path: row.path.clone(),
            fields,
            art_release_id: if sched.steps.artwork && row.art_available {
                cand.candidate.release_id.clone()
            } else {
                None
            },
            art_release_group_id: if sched.steps.artwork && row.art_available {
                cand.candidate.release_group_id.clone()
            } else {
                None
            },
            lyrics_plain: if sched.steps.lyrics && row.lyrics_available {
                row.fetched_plain.clone()
            } else {
                None
            },
            lyrics_synced: if sched.steps.lyrics && row.lyrics_available {
                row.fetched_synced.clone()
            } else {
                None
            },
        });
    }
    apply_core(
        &catalog,
        &sched.name,
        Some(sched.id.clone()),
        &sched.target,
        &items,
        pending,
        &settings,
        late,
    )
}

/// Confirm one pending checklist entry: re-propose the file fresh
/// and complete it (fills only — overwrites still need the review
/// surface). The entry leaves the run's pending list either way
/// it's resolved here; a still-unmatched file is an honest error.
pub fn confirm_pending(run_id: &str, path: &str) -> Result<String, String> {
    let base = data_base();
    let settings = load_settings(&base);
    let catalog = Catalog::open(&catalog_path()).map_err(|e| e.to_string())?;
    let mut run = catalog
        .musiccomplete_run(run_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no such run".to_string())?;
    let mut pending: Vec<serde_json::Value> =
        serde_json::from_str(&run.pending_json).unwrap_or_default();
    let before = pending.len();
    pending.retain(|p| p.get("path").and_then(|x| x.as_str()) != Some(path));
    if pending.len() == before {
        return Err("that item is no longer on the checklist".into());
    }
    run.pending_json = serde_json::Value::Array(pending).to_string();
    catalog
        .upsert_musiccomplete_run(&run)
        .map_err(|e| e.to_string())?;

    let p = PathBuf::from(path);
    let scan = propose_paths(p.parent().unwrap_or(Path::new("/")), &[path.to_string()], &settings)?;
    let row = scan
        .rows
        .into_iter()
        .next()
        .ok_or_else(|| "the file could not be re-read".to_string())?;
    let Some(cand) = &row.candidate else {
        return Err(
            "still no match for this file — open Music Completion on its folder to resolve it by hand"
                .into(),
        );
    };
    let item = ApplyItem {
        path: row.path.clone(),
        fields: row
            .changes
            .iter()
            .filter(|c| !c.overwrite)
            .map(|c| ApplyField {
                field: c.field.clone(),
                value: Some(c.proposed.clone()),
            })
            .collect(),
        art_release_id: if row.art_available {
            cand.candidate.release_id.clone()
        } else {
            None
        },
        art_release_group_id: if row.art_available {
            cand.candidate.release_group_id.clone()
        } else {
            None
        },
        lyrics_plain: if row.lyrics_available {
            row.fetched_plain.clone()
        } else {
            None
        },
        lyrics_synced: if row.lyrics_available {
            row.fetched_synced.clone()
        } else {
            None
        },
    };
    let done = apply_core(
        &catalog,
        "Confirmed from checklist",
        None,
        path,
        &[item],
        Vec::new(),
        &settings,
        false,
    )?;
    Ok(format!(
        "Completed {} — run {} finished as {}.",
        row.name, done.id, done.status
    ))
}

/// The scheduler tick: mediacomplete's background loop calls this
/// beside the auditor's — one heartbeat for all three pipelines.
pub fn agent_tick(base: &Path) {
    let settings_snapshot = load_settings(base);
    if settings_snapshot.schedules.iter().all(|s| !s.enabled) {
        return;
    }
    let mut settings = settings_snapshot.clone();
    let mut changed = false;
    let now = now_local();
    for sched in settings.schedules.iter_mut().filter(|s| s.enabled) {
        let mc = sched.as_mc();
        let late = match crate::mediacomplete::evaluate(&mc, now) {
            crate::mediacomplete::Decision::Due { late } => late,
            crate::mediacomplete::Decision::NotDue => continue,
        };
        if crate::mediacomplete::defer_reason(&mc).is_some() {
            continue;
        }
        sched.last_started = Some(stamp(now_local()));
        match run_scheduled(&sched.clone(), late) {
            Ok(row) => {
                sched.last_status = Some(row.status.clone());
                eprintln!(
                    "[fable-agent] music '{}' finished: {}",
                    sched.name, row.status
                );
            }
            Err(e) => {
                sched.last_status = Some(format!("failed: {e}"));
                eprintln!("[fable-agent] music '{}' failed: {e}", sched.name);
            }
        }
        changed = true;
    }
    if changed {
        let _ = save_settings(base, &settings);
    }
}

#[cfg(test)]
mod tests;
