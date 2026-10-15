//! opensubs: the OpenSubtitles client for Media Completion
//! (Dickles, 2026-10-15) — the subtitle third of the completion
//! trio (normalize the title, the poster, and the subtitles).
//!
//! Laws it obeys:
//! - Matching is by MOVIE HASH first (ufm_media::oshash) — that is
//!   what makes a subtitle actually sync — with TMDB-id and then
//!   plain-name search as fallbacks, in that order.
//! - Credentials are the USER's own free OpenSubtitles account: an
//!   API key (required for any API use) plus an optional
//!   username/password for the logged-in download allowance. All
//!   three live ONLY in the OS keyring (entries
//!   `mediacomplete/opensubs-*`). Nothing is bundled, nothing is
//!   written to a settings file, a job snapshot, or a log.
//! - QUOTA HONESTY: searching is unlimited; downloading is not. The
//!   official docs give the anonymous allowance (5 downloads per IP
//!   per 24 h) and no fixed logged-in number, so this module never
//!   states one — every download reports the API's own `remaining`
//!   and `reset_time` values, and callers surface exactly those.
//!
//! Fixture note: the JSON parsing tests use response shapes MODELED
//! on the official API documentation (unlike mediares' TMDB
//! fixtures, which were captured live). The keyed live path is
//! UNVERIFIED until the user pastes their own account details in
//! Preferences and validates — nothing here pretends otherwise.

use serde::{Deserialize, Serialize};
use std::sync::Mutex;

pub const API_BASE: &str = "https://api.opensubtitles.com/api/v1";
const USER_AGENT: &str = "FableFiles v0.1";

// ---------------------------------------------------------------------------
// Credentials — OS keyring only (rommedia/mediares pattern).
// ---------------------------------------------------------------------------

fn entry(kind: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(
        crate::rommedia::KEYRING_SERVICE,
        &format!("mediacomplete/{kind}"),
    )
    .map_err(|e| format!("system keyring unavailable: {e}"))
}

pub fn secret_get(kind: &str) -> Option<String> {
    let e = entry(kind).ok()?;
    match e.get_password() {
        Ok(v) if !v.trim().is_empty() => Some(v.trim().to_string()),
        _ => None,
    }
}

pub fn secret_set(kind: &str, value: Option<&str>) -> Result<(), String> {
    let e = entry(kind)?;
    match value {
        Some(v) if !v.trim().is_empty() => e
            .set_password(v.trim())
            .map_err(|e| format!("could not save to the system keyring: {e}")),
        _ => match e.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(format!("could not clear the saved secret: {e}")),
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredsState {
    pub has_api_key: bool,
    pub has_login: bool,
    pub keyring_ok: bool,
}

pub fn creds_state() -> CredsState {
    CredsState {
        has_api_key: secret_get("opensubs-apikey").is_some(),
        has_login: secret_get("opensubs-username").is_some()
            && secret_get("opensubs-password").is_some(),
        keyring_ok: crate::rommedia::keyring_available(),
    }
}

// ---------------------------------------------------------------------------
// API shapes (docs-modeled; parsed defensively).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubCandidate {
    pub file_id: i64,
    pub file_name: String,
    pub language: String,
    pub download_count: i64,
    pub rating: f64,
    pub hearing_impaired: bool,
    pub foreign_parts_only: bool,
    /// The release name the subtitle was made for (sync evidence).
    pub release: String,
    /// True when this candidate came from the moviehash search.
    pub from_hash: bool,
}

/// Parse a GET /subtitles response into candidates. Pure; tested.
pub fn candidates_from_search(v: &serde_json::Value, from_hash: bool) -> Vec<SubCandidate> {
    let mut out = Vec::new();
    let Some(data) = v.get("data").and_then(|d| d.as_array()) else {
        return out;
    };
    for item in data {
        let Some(attrs) = item.get("attributes") else {
            continue;
        };
        let language = attrs
            .get("language")
            .and_then(|l| l.as_str())
            .unwrap_or_default()
            .to_string();
        let download_count = attrs
            .get("download_count")
            .and_then(|d| d.as_i64())
            .unwrap_or(0);
        let rating = attrs.get("ratings").and_then(|r| r.as_f64()).unwrap_or(0.0);
        let hearing_impaired = attrs
            .get("hearing_impaired")
            .and_then(|h| h.as_bool())
            .unwrap_or(false);
        let foreign_parts_only = attrs
            .get("foreign_parts_only")
            .and_then(|h| h.as_bool())
            .unwrap_or(false);
        let release = attrs
            .get("release")
            .and_then(|r| r.as_str())
            .unwrap_or_default()
            .to_string();
        if let Some(files) = attrs.get("files").and_then(|f| f.as_array()) {
            for f in files {
                if let Some(fid) = f.get("file_id").and_then(|i| i.as_i64()) {
                    out.push(SubCandidate {
                        file_id: fid,
                        file_name: f
                            .get("file_name")
                            .and_then(|n| n.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        language: language.clone(),
                        download_count,
                        rating,
                        hearing_impaired,
                        foreign_parts_only,
                        release: release.clone(),
                        from_hash,
                    });
                }
            }
        }
    }
    out
}

/// Rank candidates: hash provenance first; plain (non-SDH,
/// non-foreign-parts) before labelled variants; then downloads,
/// then rating. Pure; tested.
pub fn rank_candidates(mut cands: Vec<SubCandidate>) -> Vec<SubCandidate> {
    cands.sort_by(|a, b| {
        b.from_hash
            .cmp(&a.from_hash)
            .then(a.hearing_impaired.cmp(&b.hearing_impaired))
            .then(a.foreign_parts_only.cmp(&b.foreign_parts_only))
            .then(b.download_count.cmp(&a.download_count))
            .then(b.rating.partial_cmp(&a.rating).unwrap_or(std::cmp::Ordering::Equal))
    });
    cands
}

/// What the API itself said about the download allowance after a
/// download — the ONLY quota numbers Fable ever repeats.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaState {
    /// Downloads the API says remain in the current window.
    pub remaining: Option<i64>,
    /// Requests made in the current window (API-reported).
    pub requests: Option<i64>,
    /// When the window resets (API-reported string).
    pub reset_time: Option<String>,
    /// The API's own message, when it sent one.
    pub message: Option<String>,
}

pub fn quota_from_download(v: &serde_json::Value) -> QuotaState {
    QuotaState {
        remaining: v.get("remaining").and_then(|r| r.as_i64()),
        requests: v.get("requests").and_then(|r| r.as_i64()),
        reset_time: v
            .get("reset_time")
            .and_then(|r| r.as_str())
            .map(|s| s.to_string()),
        message: v
            .get("message")
            .and_then(|m| m.as_str())
            .map(|s| s.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

pub struct OpenSubsClient {
    api_key: String,
    agent: ureq::Agent,
    token: Mutex<Option<String>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountView {
    pub ok: bool,
    pub message: String,
    /// Login name / account label when the API provided one.
    pub account: Option<String>,
    /// Logged in (vs API-key-only anonymous mode).
    pub logged_in: bool,
}

impl OpenSubsClient {
    pub fn from_keyring() -> Result<Self, String> {
        let api_key = secret_get("opensubs-apikey").ok_or_else(|| {
            "no OpenSubtitles API key is saved — add your free key in Preferences → Media Completion".to_string()
        })?;
        Ok(Self {
            api_key,
            agent: ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(30))
                .build(),
            token: Mutex::new(None),
        })
    }

    fn base_req(&self, method: &str, path: &str) -> ureq::Request {
        let url = format!("{API_BASE}{path}");
        let req = match method {
            "POST" => self.agent.post(&url),
            _ => self.agent.get(&url),
        };
        req.set("Api-Key", &self.api_key)
            .set("User-Agent", USER_AGENT)
            .set("Accept", "application/json")
    }

    fn login(&self) -> Result<String, String> {
        if let Some(t) = self.token.lock().unwrap().clone() {
            return Ok(t);
        }
        let (Some(user), Some(pass)) = (
            secret_get("opensubs-username"),
            secret_get("opensubs-password"),
        ) else {
            return Err("anonymous".into());
        };
        let resp = self
            .base_req("POST", "/login")
            .set("Content-Type", "application/json")
            .send_string(&format!(
                "{{\"username\":{},\"password\":{}}}",
                serde_json::to_string(&user).unwrap_or_default(),
                serde_json::to_string(&pass).unwrap_or_default()
            ));
        match resp {
            Ok(r) => {
                let text = r
                    .into_string()
                    .map_err(|e| format!("OpenSubtitles sent back something unreadable: {e}"))?;
                let v: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|e| format!("OpenSubtitles sent back something unreadable: {e}"))?;
                let token = v
                    .get("token")
                    .and_then(|t| t.as_str())
                    .ok_or_else(|| {
                        "OpenSubtitles login did not return a token — check the username and password".to_string()
                    })?
                    .to_string();
                *self.token.lock().unwrap() = Some(token.clone());
                Ok(token)
            }
            Err(ureq::Error::Status(401, _)) => Err(
                "OpenSubtitles rejected the login (401) — check the username and password in Preferences"
                    .into(),
            ),
            Err(ureq::Error::Status(code, _)) => {
                Err(format!("OpenSubtitles login answered with an error (HTTP {code})"))
            }
            Err(e) => Err(format!("couldn't reach OpenSubtitles: {e}")),
        }
    }

    /// Validate the saved credentials: proves the API key works
    /// (search is unlimited) and, when login details exist, that the
    /// login works too — reporting the account the API names.
    pub fn validate(&self) -> AccountView {
        // A search call proves the key (searches are not quota'd).
        match self.search(&[("query", "dune".to_string()), ("languages", "en".into())], false) {
            Ok(_) => {}
            Err(e) => {
                return AccountView {
                    ok: false,
                    message: e,
                    account: None,
                    logged_in: false,
                }
            }
        }
        match self.login() {
            Ok(_) => AccountView {
                ok: true,
                message: "API key works and the account login works — downloads use your account's allowance.".into(),
                account: secret_get("opensubs-username"),
                logged_in: true,
            },
            Err(e) if e == "anonymous" => AccountView {
                ok: true,
                message: "API key works. No account login saved, so downloads use the anonymous allowance (5 per day per IP, per the OpenSubtitles docs) — add your free account's username and password to use its allowance instead.".into(),
                account: None,
                logged_in: false,
            },
            Err(e) => AccountView {
                ok: false,
                message: e,
                account: None,
                logged_in: false,
            },
        }
    }

    pub fn search(
        &self,
        query: &[(&str, String)],
        from_hash: bool,
    ) -> Result<Vec<SubCandidate>, String> {
        // Politeness throttle, TMDB-style.
        std::thread::sleep(std::time::Duration::from_millis(34));
        let mut req = self.base_req("GET", "/subtitles");
        for (k, v) in query {
            req = req.query(k, v);
        }
        match req.call() {
            Ok(r) => {
                let text = r
                    .into_string()
                    .map_err(|e| format!("OpenSubtitles sent back something unreadable: {e}"))?;
                let v: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|e| format!("OpenSubtitles sent back something unreadable: {e}"))?;
                Ok(candidates_from_search(&v, from_hash))
            }
            Err(ureq::Error::Status(401, _)) => Err(
                "OpenSubtitles rejected the API key (401) — check it in Preferences → Media Completion".into(),
            ),
            Err(ureq::Error::Status(429, _)) => {
                Err("OpenSubtitles is rate-limiting right now — try again in a bit".into())
            }
            Err(ureq::Error::Status(code, _)) => {
                Err(format!("OpenSubtitles search answered with an error (HTTP {code})"))
            }
            Err(e) => Err(format!("couldn't reach OpenSubtitles: {e}")),
        }
    }

    /// The fallback chain: movie hash → TMDB id → file name query.
    /// Returns the ranked candidates and whether any came from the
    /// hash (the sync-trustworthy route).
    pub fn find_for_file(
        &self,
        video_path: &std::path::Path,
        language: &str,
        media_type: &str,
        tmdb_id: Option<i64>,
        season: Option<u32>,
        episode: Option<u32>,
    ) -> Result<Vec<SubCandidate>, String> {
        let mut queries: Vec<(Vec<(String, String)>, bool)> = Vec::new();
        if let Some(hash) = ufm_media::oshash::opensubtitles_hash(video_path) {
            queries.push((
                vec![
                    ("moviehash".into(), hash),
                    ("languages".into(), language.to_string()),
                ],
                true,
            ));
        }
        if let Some(id) = tmdb_id {
            let mut q = vec![
                ("tmdb_id".into(), id.to_string()),
                ("languages".into(), language.to_string()),
                (
                    "type".into(),
                    if media_type == "episode" { "episode" } else { "movie" }.into(),
                ),
            ];
            if let Some(s) = season {
                q.push(("season_number".into(), s.to_string()));
            }
            if let Some(e) = episode {
                q.push(("episode_number".into(), e.to_string()));
            }
            queries.push((q, false));
        }
        if let Some(stem) = video_path.file_stem().and_then(|s| s.to_str()) {
            queries.push((
                vec![
                    ("query".into(), stem.to_string()),
                    ("languages".into(), language.to_string()),
                ],
                false,
            ));
        }
        for (q, from_hash) in queries {
            let refs: Vec<(&str, String)> =
                q.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
            let found = self.search(&refs, from_hash)?;
            if !found.is_empty() {
                return Ok(rank_candidates(found));
            }
        }
        Ok(Vec::new())
    }

    /// Ask the API for a download link, then fetch the subtitle
    /// bytes from it. Returns (bytes, suggested file name, quota).
    pub fn download(
        &self,
        file_id: i64,
    ) -> Result<(Vec<u8>, String, QuotaState), String> {
        let mut req = self
            .base_req("POST", "/download")
            .set("Content-Type", "application/json");
        if let Ok(token) = self.login() {
            req = req.set("Authorization", &format!("Bearer {token}"));
        }
        let resp = req.send_string(&format!("{{\"file_id\":{file_id}}}"));
        let text = match resp {
            Ok(r) => r
                .into_string()
                .map_err(|e| format!("OpenSubtitles sent back something unreadable: {e}"))?,
            Err(ureq::Error::Status(403, _)) => {
                return Err(
                    "OpenSubtitles refused the download (403) — the daily allowance is likely used up".into(),
                )
            }
            Err(ureq::Error::Status(406, _)) => {
                return Err(
                    "OpenSubtitles refused the download (406) — the daily allowance is likely used up".into(),
                )
            }
            Err(ureq::Error::Status(429, _)) => {
                return Err("OpenSubtitles is rate-limiting downloads right now".into())
            }
            Err(ureq::Error::Status(code, _)) => {
                return Err(format!("OpenSubtitles download answered with an error (HTTP {code})"))
            }
            Err(e) => return Err(format!("couldn't reach OpenSubtitles: {e}")),
        };
        let v: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| format!("OpenSubtitles sent back something unreadable: {e}"))?;
        let quota = quota_from_download(&v);
        if let Some(rem) = quota.remaining {
            if rem < 0 {
                return Err("OpenSubtitles says the download allowance is used up".into());
            }
        }
        let link = v
            .get("link")
            .and_then(|l| l.as_str())
            .ok_or_else(|| {
                format!(
                    "OpenSubtitles did not return a download link{}",
                    quota
                        .message
                        .as_deref()
                        .map(|m| format!(" — it said: {m}"))
                        .unwrap_or_default()
                )
            })?
            .to_string();
        let file_name = v
            .get("file_name")
            .and_then(|n| n.as_str())
            .unwrap_or_default()
            .to_string();
        let mut bytes = Vec::new();
        let resp = self
            .agent
            .get(&link)
            .set("User-Agent", USER_AGENT)
            .call()
            .map_err(|e| format!("subtitle file download failed: {e}"))?;
        resp.into_reader()
            .take(4 * 1024 * 1024)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("subtitle file download failed: {e}"))?;
        if bytes.is_empty() {
            return Err("OpenSubtitles sent an empty subtitle file".into());
        }
        Ok((bytes, file_name, quota))
    }
}

use std::io::Read as _;

#[cfg(test)]
mod tests {
    use super::*;

    /// Docs-modeled search response (shapes per the official API
    /// documentation — NOT a live capture; the keyed path is
    /// unverified until the user validates their own account).
    fn search_fixture() -> serde_json::Value {
        serde_json::json!({
            "total_pages": 1, "total_count": 2, "page": 1,
            "data": [
                {"type": "subtitle", "id": "1", "attributes": {
                    "subtitle_id": "1", "language": "en",
                    "download_count": 12000, "ratings": 8.5,
                    "hearing_impaired": true, "foreign_parts_only": false,
                    "release": "Dune.2021.1080p.WEB-DL",
                    "files": [{"file_id": 101, "file_name": "dune.sdh.srt"}]
                }},
                {"type": "subtitle", "id": "2", "attributes": {
                    "subtitle_id": "2", "language": "en",
                    "download_count": 45000, "ratings": 9.1,
                    "hearing_impaired": false, "foreign_parts_only": false,
                    "release": "Dune.2021.2160p.WEB-DL",
                    "files": [{"file_id": 102, "file_name": "dune.srt"}]
                }}
            ]
        })
    }

    #[test]
    fn parses_candidates_from_docs_shaped_response() {
        let cands = candidates_from_search(&search_fixture(), true);
        assert_eq!(cands.len(), 2);
        assert_eq!(cands[0].file_id, 101);
        assert!(cands[0].hearing_impaired);
        assert!(cands[0].from_hash);
        assert_eq!(cands[1].download_count, 45000);
    }

    #[test]
    fn ranking_prefers_plain_then_popular() {
        let cands = rank_candidates(candidates_from_search(&search_fixture(), true));
        // The plain (non-SDH) subtitle wins despite the SDH one's
        // hash provenance being equal.
        assert_eq!(cands[0].file_id, 102);
        // Hash provenance outranks everything else.
        let mut mixed = candidates_from_search(&search_fixture(), false);
        mixed.push(SubCandidate {
            file_id: 999,
            file_name: "x.srt".into(),
            language: "en".into(),
            download_count: 1,
            rating: 0.0,
            hearing_impaired: true,
            foreign_parts_only: true,
            release: "hash hit".into(),
            from_hash: true,
        });
        let ranked = rank_candidates(mixed);
        assert_eq!(ranked[0].file_id, 999);
    }

    #[test]
    fn quota_comes_from_the_response_never_from_us() {
        let v = serde_json::json!({
            "link": "https://example.invalid/x.srt", "file_name": "x.srt",
            "requests": 3, "remaining": 17, "reset_time": "2026-10-16T00:00:00Z",
            "message": "ok"
        });
        let q = quota_from_download(&v);
        assert_eq!(q.remaining, Some(17));
        assert_eq!(q.requests, Some(3));
        assert!(q.reset_time.is_some());
        // Absent fields stay absent — no invented allowance.
        let q2 = quota_from_download(&serde_json::json!({"link": "x"}));
        assert_eq!(q2.remaining, None);
    }
}
