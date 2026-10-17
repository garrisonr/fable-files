//! playnite: the Playnite library model over the LiteDB v4 reader
//! (PC Games Library P1, 2026-10-17; DESIGN.md §1/§3).
//!
//! A Playnite library directory holds one LiteDB file per
//! collection (`games.db` plus genres/companies/platforms/sources/
//! features/series/ageratings/completionstatuses/regions/tags/
//! categories `.db` files) and a `files/` tree of adopted media,
//! addressed from game records as `<game-GUID>\<file-GUID>.<ext>`
//! (backslash-separated, Windows-style). This module reads the
//! collections and resolves every GUID reference to its display
//! name, producing plain structs the importer can consume. It never
//! writes anything.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::litedb::{BsonValue, LiteDbError, LiteDbFile, Result};

/// Collections Playnite keeps beside games.db. A missing file is
/// an empty collection, not an error — Playnite writes them all,
/// but a hand-carried library may lack the empty ones.
const REF_COLLECTIONS: &[&str] = &[
    "genres",
    "companies",
    "platforms",
    "sources",
    "features",
    "series",
    "ageratings",
    "completionstatuses",
    "regions",
    "tags",
    "categories",
];

/// One play action from a game record ({Type:"File",
/// Path:"{InstallDir}\<exe>", WorkingDir:"{InstallDir}", …}).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlayAction {
    pub action_type: Option<String>,
    pub name: Option<String>,
    pub path: Option<String>,
    pub working_dir: Option<String>,
    pub is_play_action: bool,
}

/// One game, with every reference already resolved to names.
#[derive(Debug, Clone, Default)]
pub struct PlayniteGame {
    /// The record's own GUID (its media folder is named by it).
    pub id: String,
    /// Playnite's stable per-game string id (a GUID in text form).
    pub game_id: Option<String>,
    pub plugin_id: Option<String>,
    pub name: String,
    pub sorting_name: Option<String>,
    pub source_name: Option<String>,
    /// The recorded install directory — an absolute WINDOWS path
    /// (any drive letter; the vendor assembled libraries across
    /// machines). Rebasing onto a real volume is the importer's
    /// job, never done silently here.
    pub install_directory: Option<String>,
    pub is_installed: bool,
    /// Media paths relative to the library's `files/` root,
    /// backslash-separated exactly as recorded.
    pub cover_image: Option<String>,
    pub background_image: Option<String>,
    pub icon: Option<String>,
    pub genres: Vec<String>,
    pub developers: Vec<String>,
    pub publishers: Vec<String>,
    pub platforms: Vec<String>,
    pub features: Vec<String>,
    pub series: Vec<String>,
    pub age_ratings: Vec<String>,
    /// The release date exactly as recorded ("2019-5-14").
    pub release_raw: Option<String>,
    pub release_year: Option<i64>,
    /// HTML, as the vendor wrote it — sanitize at render, never here.
    pub description: Option<String>,
    pub critic_score: Option<i64>,
    pub community_score: Option<i64>,
    pub user_score: Option<i64>,
    pub install_size: Option<i64>,
    /// Vendor play trivia: present in the record, low-trust by the
    /// design's ruling (median 70 s of test launches). Carried into
    /// evidence only; Fable's own play tracking starts at zero.
    pub playtime_secs: i64,
    pub play_count: i64,
    pub play_actions: Vec<PlayAction>,
    pub links: Vec<(String, String)>,
    pub hidden: bool,
    pub favorite: bool,
    pub version: Option<String>,
    pub completion_status: Option<String>,
}

/// One loaded Playnite library.
#[derive(Debug)]
pub struct PlayniteLibrary {
    /// The directory the `.db` files were read from.
    pub library_dir: PathBuf,
    pub games: Vec<PlayniteGame>,
    /// Document counts per collection file actually read
    /// ("games" included), for import reports and reconciliation.
    pub collection_counts: Vec<(String, usize)>,
    /// `library_dir`'s media root (`files/`), when it exists.
    pub files_dir: Option<PathBuf>,
    /// Documents in games.db that were not games (no Name) —
    /// counted, never silently absorbed.
    pub non_game_docs: usize,
}

fn doc_name(doc: &BsonValue) -> Option<String> {
    doc.get("Name").and_then(|v| v.as_str()).map(|s| s.to_string())
}

/// Load one reference collection into a GUID → Name map. Companies
/// in Playnite's own data contain duplicate names under different
/// GUIDs; resolution is by GUID, so that costs nothing here — the
/// importer dedupes names when it writes facets.
fn load_ref(dir: &Path, name: &str, counts: &mut Vec<(String, usize)>) -> Result<HashMap<String, String>> {
    let path = dir.join(format!("{name}.db"));
    if !path.exists() {
        counts.push((name.to_string(), 0));
        return Ok(HashMap::new());
    }
    let file = LiteDbFile::open(&path)?;
    let docs = file.documents()?;
    let mut map = HashMap::with_capacity(docs.len());
    for doc in &docs {
        if let (Some(id), Some(name)) = (doc.get("_id").and_then(|v| v.as_guid()), doc_name(doc)) {
            map.insert(id.to_string(), name);
        }
    }
    // The census counts NAMED entries: a collection file can carry
    // internal bookkeeping documents (an Int32 _id, no Name) that
    // are not Playnite records.
    counts.push((name.to_string(), map.len()));
    Ok(map)
}

fn resolve(map: &HashMap<String, String>, ids: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(name) = map.get(id) {
            if !out.contains(name) {
                out.push(name.clone());
            }
        }
    }
    out
}

fn guid_field(doc: &BsonValue, key: &str) -> Option<String> {
    doc.get(key).and_then(|v| v.as_guid()).map(|g| g.to_string())
}

fn str_field(doc: &BsonValue, key: &str) -> Option<String> {
    doc.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

fn ids_field(doc: &BsonValue, key: &str) -> Vec<String> {
    doc.get(key).map(|v| v.guid_list()).unwrap_or_default()
}

/// Playnite records ReleaseDate as a string ("2019-5-14"). The
/// year is its first dash-separated component; anything else is
/// carried raw with no year claimed.
fn release_year_of(raw: &str) -> Option<i64> {
    raw.split('-').next()?.trim().parse::<i64>().ok().filter(|y| *y > 0)
}

fn parse_game(
    doc: &BsonValue,
    refs: &HashMap<String, HashMap<String, String>>,
) -> Option<PlayniteGame> {
    let name = doc_name(doc)?;
    let id = guid_field(doc, "_id")?;
    let get = |c: &str| refs.get(c);
    let resolve_in = |c: &str, key: &str| -> Vec<String> {
        get(c).map(|m| resolve(m, &ids_field(doc, key))).unwrap_or_default()
    };
    let release_raw = str_field(doc, "ReleaseDate");
    let release_year = release_raw.as_deref().and_then(release_year_of);
    let play_actions = doc
        .get("GameActions")
        .and_then(|v| v.as_array())
        .map(|actions| {
            actions
                .iter()
                .map(|a| PlayAction {
                    action_type: str_field(a, "Type"),
                    name: str_field(a, "Name"),
                    path: str_field(a, "Path"),
                    working_dir: str_field(a, "WorkingDir"),
                    is_play_action: a
                        .get("IsPlayAction")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                })
                .collect()
        })
        .unwrap_or_default();
    let links = doc
        .get("Links")
        .and_then(|v| v.as_array())
        .map(|links| {
            links
                .iter()
                .filter_map(|l| {
                    let name = str_field(l, "Name")?;
                    let url = str_field(l, "Url")?;
                    Some((name, url))
                })
                .collect()
        })
        .unwrap_or_default();
    let completion_status = guid_field(doc, "CompletionStatusId")
        .and_then(|cid| {
            get("completionstatuses")
                .and_then(|m| m.get(&cid).cloned())
        })
        .or_else(|| {
            doc.get("CompletionStatus")
                .and_then(|v| doc_name(v))
        });
    Some(PlayniteGame {
        id,
        game_id: str_field(doc, "GameId"),
        plugin_id: guid_field(doc, "PluginId"),
        name,
        sorting_name: str_field(doc, "SortingName"),
        source_name: guid_field(doc, "SourceId").and_then(|sid| {
            get("sources").and_then(|m| m.get(&sid).cloned())
        }),
        install_directory: str_field(doc, "InstallDirectory"),
        is_installed: doc
            .get("IsInstalled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        cover_image: str_field(doc, "CoverImage"),
        background_image: str_field(doc, "BackgroundImage"),
        icon: str_field(doc, "Icon"),
        genres: resolve_in("genres", "GenreIds"),
        developers: resolve_in("companies", "DeveloperIds"),
        publishers: resolve_in("companies", "PublisherIds"),
        platforms: resolve_in("platforms", "PlatformIds"),
        features: resolve_in("features", "FeatureIds"),
        series: resolve_in("series", "SeriesIds"),
        age_ratings: resolve_in("ageratings", "AgeRatingIds"),
        release_raw,
        release_year,
        description: str_field(doc, "Description"),
        critic_score: doc.get("CriticScore").and_then(|v| v.as_i64()),
        community_score: doc.get("CommunityScore").and_then(|v| v.as_i64()),
        user_score: doc.get("UserScore").and_then(|v| v.as_i64()),
        install_size: doc.get("InstallSize").and_then(|v| v.as_i64()),
        playtime_secs: doc.get("Playtime").and_then(|v| v.as_i64()).unwrap_or(0),
        play_count: doc.get("PlayCount").and_then(|v| v.as_i64()).unwrap_or(0),
        play_actions,
        links,
        hidden: doc.get("Hidden").and_then(|v| v.as_bool()).unwrap_or(false),
        favorite: doc
            .get("Favorite")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        version: str_field(doc, "Version"),
        completion_status,
    })
}

/// Load the Playnite library whose collection files live in `dir`
/// (the folder holding `games.db` — Playnite's `library/` folder).
pub fn load_library(dir: &Path) -> Result<PlayniteLibrary> {
    let games_path = dir.join("games.db");
    if !games_path.exists() {
        return Err(LiteDbError::NotLiteDb(format!(
            "no games.db in {} — not a Playnite library folder",
            dir.display()
        )));
    }
    let mut counts: Vec<(String, usize)> = Vec::new();
    let mut refs: HashMap<String, HashMap<String, String>> = HashMap::new();
    for name in REF_COLLECTIONS {
        let map = load_ref(dir, name, &mut counts)?;
        refs.insert(name.to_string(), map);
    }
    let games_file = LiteDbFile::open(&games_path)?;
    let docs = games_file.documents()?;
    counts.push(("games".to_string(), docs.len()));
    counts.sort();
    let mut games = Vec::with_capacity(docs.len());
    let mut non_game_docs = 0usize;
    for doc in &docs {
        match parse_game(doc, &refs) {
            Some(game) => games.push(game),
            None => non_game_docs += 1,
        }
    }
    let files_dir = {
        let f = dir.join("files");
        if f.is_dir() {
            Some(f)
        } else {
            None
        }
    };
    Ok(PlayniteLibrary {
        library_dir: dir.to_path_buf(),
        games,
        collection_counts: counts,
        files_dir,
        non_game_docs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_year_parses_playnites_string_form() {
        assert_eq!(release_year_of("2019-5-14"), Some(2019));
        assert_eq!(release_year_of("2024-12-1"), Some(2024));
        assert_eq!(release_year_of(""), None);
        assert_eq!(release_year_of("unknown"), None);
    }
}
