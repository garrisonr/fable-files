//! EmulationStation gamelist import — the parsing half
//! (Hyperbase #5, 2026-10-17).
//!
//! EmulationStation front-ends (RetroBat, ES-DE, Batocera) keep a
//! per-system `gamelist.xml`: for every game file, the display name,
//! description, credits, genre, rating and play history a scrape or
//! a decade of launches produced, plus paths to artwork that lives
//! beside the games. Fable's catalog already knows the FILES (it
//! indexed them) and their DAT identity (it hashed them); this
//! module reads the gamelist so the import can attach the metadata
//! to the catalog items by path.
//!
//! Precedence contract (the apply half lives in ufm-catalog and
//! pins these in tests): a gamelist is a *source*, never the truth
//! — DAT identity outranks gamelist names, the user's own edits
//! outrank both, and imported play history merges, never replaces.
//!
//! This module is pure: XML in, records out; filesystem only in
//! [`detect_es_root`], which stats directory shapes and reads no
//! file contents. No game file is ever opened here.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One `<game>` entry from a gamelist. Every field is optional in
/// the wild; `path` is the only one the import cannot proceed
/// without, and entries lacking it are counted, not dropped
/// silently (the report carries the count).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EsGame {
    /// The `<game id="…">` attribute: the scrape source's own id
    /// for this game (ScreenScraper etc.). Provenance only.
    pub source_id: Option<String>,
    /// Raw `<path>` text, exactly as written (`./Game (USA).zip`).
    pub path: Option<String>,
    pub name: Option<String>,
    pub desc: Option<String>,
    /// Raw ES date text (`19921101T000000`) — convert with
    /// [`es_datetime_to_rfc3339`] / [`es_release_year`].
    pub releasedate: Option<String>,
    pub developer: Option<String>,
    pub publisher: Option<String>,
    pub genre: Option<String>,
    pub family: Option<String>,
    pub players: Option<String>,
    /// ES community rating, 0.0–1.0 (`0` means unrated in exports).
    pub rating: Option<f64>,
    pub favorite: bool,
    pub hidden: bool,
    pub kidgame: bool,
    pub playcount: Option<u64>,
    /// Raw ES datetime text (`20230807T114607`).
    pub lastplayed: Option<String>,
    /// Seconds played, where the front-end tracked it.
    pub gametime: Option<u64>,
    pub lang: Option<String>,
    pub region: Option<String>,
    pub crc32: Option<String>,
    pub md5: Option<String>,
    /// `<scrap name="ScreenScraper" date="…"/>` provenance.
    pub scrap_source: Option<String>,
    pub scrap_date: Option<String>,
    // Artwork refs, raw gamelist-relative text. Resolution against
    // the system folder happens in [`resolve_es_ref`].
    pub image: Option<String>,
    pub video: Option<String>,
    pub marquee: Option<String>,
    pub thumbnail: Option<String>,
    pub fanart: Option<String>,
    pub manual: Option<String>,
    /// Less common cover variants some writers emit; the art-slot
    /// precedence in the apply half consults these after `image`.
    pub boxart: Option<String>,
    pub cover: Option<String>,
    pub screenshot: Option<String>,
    pub wheel: Option<String>,
    pub mix: Option<String>,
}

/// A parsed gamelist: the games, and how many `<folder>` entries
/// were seen (folders are navigation, not games — counted so the
/// import report can reconcile against the file).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EsGamelist {
    pub games: Vec<EsGame>,
    pub folder_count: usize,
}

/// Why a gamelist could not be read. Errors are values: the import
/// reports a broken list per system and carries on with the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EsParseError {
    /// The XML reader failed (malformed document). Carries the
    /// reader's message and how many complete games were read
    /// before the failure — the caller decides the file's fate
    /// (the import treats it as failed, never partial).
    Xml { message: String, games_before: usize },
}

impl std::fmt::Display for EsParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EsParseError::Xml { message, games_before } => write!(
                f,
                "gamelist XML is malformed ({message}); {games_before} complete games read before the failure"
            ),
        }
    }
}

impl std::error::Error for EsParseError {}

fn attr_value(e: &quick_xml::events::BytesStart<'_>, want: &str) -> Option<String> {
    for a in e.attributes().with_checks(false) {
        let Ok(a) = a else { continue };
        if a.key.as_ref() == want {
            return Some(a.value.as_ref().to_string());
        }
    }
    None
}

fn parse_bool(text: &str) -> bool {
    matches!(text.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes")
}

fn set_field(game: &mut EsGame, tag: &str, raw_text: &str) {
    let text = raw_text.trim();
    let some = |v: &mut Option<String>| {
        if !text.is_empty() && v.is_none() {
            *v = Some(text.to_string());
        }
    };
    match tag {
        "path" => some(&mut game.path),
        "name" => some(&mut game.name),
        "desc" => some(&mut game.desc),
        "releasedate" => some(&mut game.releasedate),
        "developer" => some(&mut game.developer),
        "publisher" => some(&mut game.publisher),
        "genre" => some(&mut game.genre),
        "family" => some(&mut game.family),
        "players" => some(&mut game.players),
        "lang" => some(&mut game.lang),
        "region" => some(&mut game.region),
        "crc32" => some(&mut game.crc32),
        "md5" => some(&mut game.md5),
        "lastplayed" => some(&mut game.lastplayed),
        "image" => some(&mut game.image),
        "video" => some(&mut game.video),
        "marquee" => some(&mut game.marquee),
        "thumbnail" => some(&mut game.thumbnail),
        "fanart" => some(&mut game.fanart),
        "manual" => some(&mut game.manual),
        "boxart" => some(&mut game.boxart),
        "cover" => some(&mut game.cover),
        "screenshot" => some(&mut game.screenshot),
        "wheel" => some(&mut game.wheel),
        "mix" => some(&mut game.mix),
        "rating" => {
            if game.rating.is_none() {
                game.rating = text.parse::<f64>().ok().filter(|r| r.is_finite());
            }
        }
        "playcount" => {
            if game.playcount.is_none() {
                game.playcount = text.parse::<u64>().ok();
            }
        }
        "gametime" => {
            if game.gametime.is_none() {
                game.gametime = text.parse::<u64>().ok();
            }
        }
        "favorite" => game.favorite = game.favorite || parse_bool(text),
        "hidden" => game.hidden = game.hidden || parse_bool(text),
        "kidgame" => game.kidgame = game.kidgame || parse_bool(text),
        _ => {}
    }
}

/// Parse one gamelist document (the whole text — gamelists are
/// metadata, kilobytes-to-megabytes, never game data).
///
/// The reader is streaming, so a 20k-game list costs one record of
/// memory at a time. A malformed document is an error carrying how
/// far it got; entries are never half-written.
pub fn parse_gamelist(xml: &str) -> Result<EsGamelist, EsParseError> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut out = EsGamelist::default();
    let mut buf: Vec<u8> = Vec::new();
    let mut current: Option<EsGame> = None;
    let mut field: Option<String> = None;
    let mut text = String::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let tag = e.name().as_ref().to_string();
                match tag.as_str() {
                    "game" => {
                        let mut g = EsGame::default();
                        g.source_id = attr_value(&e, "id");
                        current = Some(g);
                        field = None;
                    }
                    "folder" => {
                        out.folder_count += 1;
                        field = None;
                    }
                    "scrap" => {
                        if let Some(g) = current.as_mut() {
                            if g.scrap_source.is_none() {
                                g.scrap_source = attr_value(&e, "name");
                            }
                            if g.scrap_date.is_none() {
                                g.scrap_date = attr_value(&e, "date");
                            }
                        }
                        field = None;
                    }
                    _ => {
                        if current.is_some() {
                            field = Some(tag);
                            text.clear();
                        }
                    }
                }
            }
            Ok(Event::Empty(e)) => {
                let tag = e.name().as_ref().to_string();
                if tag == "scrap" {
                    if let Some(g) = current.as_mut() {
                        if g.scrap_source.is_none() {
                            g.scrap_source = attr_value(&e, "name");
                        }
                        if g.scrap_date.is_none() {
                            g.scrap_date = attr_value(&e, "date");
                        }
                    }
                } else if tag == "game" {
                    let mut g = EsGame::default();
                    g.source_id = attr_value(&e, "id");
                    out.games.push(g);
                }
            }
            Ok(Event::Text(e)) => {
                if field.is_some() {
                    match quick_xml::escape::unescape(e.as_ref()) {
                        Ok(s) => text.push_str(&s),
                        Err(_) => {
                            // A text node the escaper rejects makes
                            // the document untrustworthy as a whole.
                            return Err(EsParseError::Xml {
                                message: "unreadable text content".to_string(),
                                games_before: out.games.len(),
                            });
                        }
                    }
                }
            }
            Ok(Event::CData(e)) => {
                if field.is_some() {
                    text.push_str(e.as_ref());
                }
            }
            Ok(Event::GeneralRef(e)) => {
                // quick-xml 0.42 surfaces `&amp;` & friends as their
                // own events; resolve the XML five + numeric refs,
                // keep anything unknown literal rather than dropping it.
                if field.is_some() {
                    let name = e.as_ref();
                    let resolved: Option<String> = match name {
                        "amp" => Some("&".to_string()),
                        "lt" => Some("<".to_string()),
                        "gt" => Some(">".to_string()),
                        "quot" => Some("\"".to_string()),
                        "apos" => Some("'".to_string()),
                        _ => {
                            let num = name.strip_prefix('#');
                            match num {
                                Some(hex) if hex.starts_with(['x', 'X']) => {
                                    u32::from_str_radix(&hex[1..], 16)
                                        .ok()
                                        .and_then(char::from_u32)
                                        .map(|c| c.to_string())
                                }
                                Some(dec) => dec
                                    .parse::<u32>()
                                    .ok()
                                    .and_then(char::from_u32)
                                    .map(|c| c.to_string()),
                                None => None,
                            }
                        }
                    };
                    match resolved {
                        Some(s) => text.push_str(&s),
                        None => {
                            text.push('&');
                            text.push_str(name);
                            text.push(';');
                        }
                    }
                }
            }
            Ok(Event::End(e)) => {
                let tag = e.name().as_ref().to_string();
                if tag == "game" {
                    if let Some(g) = current.take() {
                        out.games.push(g);
                    }
                    field = None;
                } else if field.as_deref() == Some(tag.as_str()) {
                    if let Some(g) = current.as_mut() {
                        let value = std::mem::take(&mut text);
                        set_field(g, &tag, &value);
                    }
                    field = None;
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(err) => {
                return Err(EsParseError::Xml {
                    message: err.to_string(),
                    games_before: out.games.len(),
                });
            }
        }
        buf.clear();
    }
    // A document that ends inside a <game> is truncated, not done.
    if current.is_some() {
        return Err(EsParseError::Xml {
            message: "document ends inside a <game> entry".to_string(),
            games_before: out.games.len(),
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// ES value conversions.
// ---------------------------------------------------------------------------

/// The release year an ES date string names (`19921101T000000` →
/// 1992). None when the string carries no plausible year.
pub fn es_release_year(raw: &str) -> Option<i64> {
    let t = raw.trim();
    if t.len() < 4 || !t[..4].chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let y: i64 = t[..4].parse().ok()?;
    (1900..=2100).contains(&y).then_some(y)
}

/// Convert an ES datetime (`20230807T114607`, also tolerated:
/// `2023-08-07T11:46:07`, date-only) to RFC 3339 UTC. ES writes
/// naive local times; the import treats them as UTC — the honest
/// approximation, declared in the receipt. None for values that
/// are not plausible datetimes.
pub fn es_datetime_to_rfc3339(raw: &str) -> Option<String> {
    let t = raw.trim();
    let digits: String = t.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() != 8 && digits.len() != 14 {
        return None;
    }
    let (y, mo, d) = (
        digits[0..4].parse::<i64>().ok()?,
        digits[4..6].parse::<u32>().ok()?,
        digits[6..8].parse::<u32>().ok()?,
    );
    let (h, mi, s) = if digits.len() == 14 {
        (
            digits[8..10].parse::<u32>().ok()?,
            digits[10..12].parse::<u32>().ok()?,
            digits[12..14].parse::<u32>().ok()?,
        )
    } else {
        (0, 0, 0)
    };
    if !(1900..=2100).contains(&y)
        || !(1..=12).contains(&mo)
        || !(1..=31).contains(&d)
        || h > 23
        || mi > 59
        || s > 60
    {
        return None;
    }
    Some(format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z"))
}

/// ES community rating (0.0–1.0) → Fable stars (1..=5). Zero and
/// absent both mean unrated; values above 1 clamp.
pub fn es_rating_to_stars(rating: f64) -> Option<i64> {
    if !rating.is_finite() || rating <= 0.0 {
        return None;
    }
    let stars = (rating.clamp(0.0, 1.0) * 5.0).round() as i64;
    (1..=5).contains(&stars).then_some(stars)
}

// ---------------------------------------------------------------------------
// Paths.
// ---------------------------------------------------------------------------

/// Lexically normalize a path (`.` dropped, `..` resolved against
/// accumulated components) without touching the filesystem —
/// catalog provider_keys are canonical absolute paths, so the
/// resolved gamelist path must be shaped the same way to match.
fn normalize_lexical(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Resolve a gamelist reference (`./images/X-image.png`,
/// `.\images\X.png`, a nested `./sub/Game.zip`) against the folder
/// it was written relative to (the system folder for game paths,
/// the same folder for artwork). Windows-absolute references
/// (`C:\…`, `\\server\…`) cannot resolve on this machine and
/// return None — the import counts them as unresolvable rather
/// than guessing.
pub fn resolve_es_ref(base_dir: &Path, raw: &str) -> Option<PathBuf> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    let bytes = t.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return None; // C:\… drive-absolute
    }
    if t.starts_with("\\\\") {
        return None; // UNC
    }
    let slashed = t.replace('\\', "/");
    let p = if let Some(rest) = slashed.strip_prefix('/') {
        PathBuf::from(format!("/{rest}"))
    } else {
        base_dir.join(&slashed)
    };
    Some(normalize_lexical(&p))
}

// ---------------------------------------------------------------------------
// System-name mapping: ES name → Fable platform key.
// ---------------------------------------------------------------------------

/// ES → Fable platform aliases: only true same-library variants
/// merge. Everything unlisted keeps its ES name as its own
/// platform — an unknown system imports as its own section in
/// Games Home rather than being dropped or misfilled.
pub const ES_SYSTEM_MAP: &[(&str, &str)] = &[
    ("3ds", "n3ds"),
    ("amiga1200", "amiga"),
    ("amiga4000", "amiga"),
    ("amiga500", "amiga"),
    ("amigacdtv", "amiga"),
    ("atari800", "atari8bit"),
    ("fbneo", "arcade"),
    ("hbmame", "arcade"),
    ("mame", "arcade"),
    ("megadrive", "genesis"),
    ("msx1", "msx"),
    ("msx2", "msx"),
    ("msx2+", "msx"),
    ("ngp", "neogeopocket"),
    ("ngpc", "neogeopocket"),
    ("pcengine", "tg16"),
    ("pcenginecd", "tg16"),
    ("sgb", "gb"),
    ("snes-msu", "snes"),
    ("wswan", "wonderswan"),
    ("wswanc", "wonderswan"),
];

/// The Fable platform a gamelist's ES system name lands in.
pub fn fable_platform(es_system: &str) -> String {
    let key = es_system.trim().to_ascii_lowercase();
    for (es, fable) in ES_SYSTEM_MAP {
        if *es == key {
            return (*fable).to_string();
        }
    }
    key
}

// ---------------------------------------------------------------------------
// Detection: which ES shape is a root, if any. Metadata only —
// directory listings and existence checks, no file contents.
// ---------------------------------------------------------------------------

/// The ES layout a root turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EsRootKind {
    /// The root IS one system folder (it holds a gamelist.xml).
    SystemDir,
    /// The root is a roms folder (its children hold gamelists).
    RomsDir,
    /// The root is a front-end install (roms/ inside it does).
    InstallRoot,
}

/// One system folder with a gamelist, found by detection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EsSystemDir {
    /// The folder's own name — the ES system name.
    pub es_name: String,
    pub dir: PathBuf,
    pub gamelist_path: PathBuf,
}

/// What detection found under a root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EsDetection {
    pub kind: EsRootKind,
    pub systems: Vec<EsSystemDir>,
}

/// Systems considered per directory — generous for real packs
/// (RetroBat ships ~180), bounded so a pathological tree can't
/// turn detection into a walk.
const MAX_SYSTEMS: usize = 512;

fn systems_in(dir: &Path) -> Vec<EsSystemDir> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut names: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect();
    names.sort();
    for (name, path) in names {
        let gamelist = path.join("gamelist.xml");
        if gamelist.is_file() {
            out.push(EsSystemDir {
                es_name: name,
                dir: path,
                gamelist_path: gamelist,
            });
            if out.len() >= MAX_SYSTEMS {
                break;
            }
        }
    }
    out
}

/// Detect the EmulationStation layout under `root`, if any:
/// a system folder, a roms folder, or a front-end install root
/// (RetroBat / ES-DE / Batocera all reduce to these shapes).
/// Reads directory metadata only — never file contents.
pub fn detect_es_root(root: &Path) -> Option<EsDetection> {
    // One system folder: the gamelist sits in the root itself.
    let own = root.join("gamelist.xml");
    if own.is_file() {
        let es_name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        return Some(EsDetection {
            kind: EsRootKind::SystemDir,
            systems: vec![EsSystemDir {
                es_name,
                dir: root.to_path_buf(),
                gamelist_path: own,
            }],
        });
    }
    // A roms folder: gamelists in its immediate children.
    let systems = systems_in(root);
    if !systems.is_empty() {
        return Some(EsDetection {
            kind: EsRootKind::RomsDir,
            systems,
        });
    }
    // An install root: roms/ inside it.
    let roms = root.join("roms");
    if roms.is_dir() {
        let systems = systems_in(&roms);
        if !systems.is_empty() {
            return Some(EsDetection {
                kind: EsRootKind::InstallRoot,
                systems,
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0"?>
<gameList>
  <game id="8743">
    <path>./LEGO Star Wars III - The Clone Wars (USA).wbfs</path>
    <name>LEGO Star Wars III: The Clone Wars</name>
    <desc>The beloved &amp; critically acclaimed LEGO® Star Wars franchise.</desc>
    <releasedate>20110322T000000</releasedate>
    <developer>Travellers Tales</developer>
    <publisher>LucasArts</publisher>
    <genre>Adventure</genre>
    <players>1-2</players>
    <rating>0.85</rating>
    <playcount>6</playcount>
    <lastplayed>20230807T114607</lastplayed>
    <gametime>3600</gametime>
    <image>./images/LEGO-image.jpg</image>
    <marquee>./images/LEGO-marquee.png</marquee>
    <thumbnail>./images/LEGO-thumb.png</thumbnail>
    <scrap name="ScreenScraper" date="20230801T155707" />
  </game>
  <game id="99">
    <path>./Hidden Gem (USA).wbfs</path>
    <name>Hidden Gem</name>
    <hidden>true</hidden>
    <favorite>1</favorite>
  </game>
  <folder><name>Sub</name><path>./sub</path></folder>
</gameList>"#;

    #[test]
    fn parses_a_real_shaped_gamelist() {
        let list = parse_gamelist(SAMPLE).unwrap();
        assert_eq!(list.games.len(), 2);
        assert_eq!(list.folder_count, 1);
        let g = &list.games[0];
        assert_eq!(g.source_id.as_deref(), Some("8743"));
        assert_eq!(g.path.as_deref(), Some("./LEGO Star Wars III - The Clone Wars (USA).wbfs"));
        assert_eq!(g.name.as_deref(), Some("LEGO Star Wars III: The Clone Wars"));
        assert_eq!(
            g.desc.as_deref(),
            Some("The beloved & critically acclaimed LEGO® Star Wars franchise."),
            "entities unescaped"
        );
        assert_eq!(g.rating, Some(0.85));
        assert_eq!(g.playcount, Some(6));
        assert_eq!(g.gametime, Some(3600));
        assert_eq!(g.scrap_source.as_deref(), Some("ScreenScraper"));
        assert_eq!(g.scrap_date.as_deref(), Some("20230801T155707"));
        let h = &list.games[1];
        assert!(h.hidden && h.favorite, "bool forms true/1 both parse");
        assert_eq!(h.rating, None);
    }

    #[test]
    fn cdata_and_nested_markup_survive() {
        let xml = "<gameList><game><path>./x.zip</path><desc><![CDATA[A <b>bold</b> claim]]></desc></game></gameList>";
        let list = parse_gamelist(xml).unwrap();
        assert_eq!(list.games[0].desc.as_deref(), Some("A <b>bold</b> claim"));
    }

    #[test]
    fn malformed_documents_are_errors_with_progress() {
        // Unclosed game.
        let bad = "<gameList><game><path>./x.zip</path>";
        match parse_gamelist(bad) {
            Err(EsParseError::Xml { games_before, .. }) => assert_eq!(games_before, 0),
            other => panic!("expected Xml error, got {other:?}"),
        }
        // Broken mid-second-game: one complete game was read.
        let bad2 = "<gameList><game><path>./a.zip</path></game><game><path>./b.zip</broken>";
        match parse_gamelist(bad2) {
            Err(EsParseError::Xml { games_before, .. }) => assert_eq!(games_before, 1),
            other => panic!("expected Xml error, got {other:?}"),
        }
    }

    #[test]
    fn dates_convert_and_reject() {
        assert_eq!(es_release_year("19921101T000000"), Some(1992));
        assert_eq!(es_release_year("garbage"), None);
        assert_eq!(es_release_year("18000101T000000"), None);
        assert_eq!(
            es_datetime_to_rfc3339("20230807T114607").as_deref(),
            Some("2023-08-07T11:46:07Z")
        );
        assert_eq!(
            es_datetime_to_rfc3339("20110322T000000").as_deref(),
            Some("2011-03-22T00:00:00Z")
        );
        assert_eq!(es_datetime_to_rfc3339("20231307T114607"), None, "month 13");
        assert_eq!(es_datetime_to_rfc3339(""), None);
    }

    #[test]
    fn rating_maps_to_stars() {
        assert_eq!(es_rating_to_stars(0.85), Some(4));
        assert_eq!(es_rating_to_stars(0.9), Some(5));
        assert_eq!(es_rating_to_stars(0.65), Some(3));
        assert_eq!(es_rating_to_stars(0.0), None, "zero is unrated in ES exports");
        assert_eq!(es_rating_to_stars(1.7), Some(5), "over-range clamps");
    }

    #[test]
    fn refs_resolve_against_the_system_folder() {
        let base = Path::new("/roms/snes");
        assert_eq!(
            resolve_es_ref(base, "./Hook (USA).zip"),
            Some(PathBuf::from("/roms/snes/Hook (USA).zip"))
        );
        assert_eq!(
            resolve_es_ref(base, ".\\images\\Hook-image.png"),
            Some(PathBuf::from("/roms/snes/images/Hook-image.png")),
            "windows separators from RetroBat-written lists"
        );
        assert_eq!(
            resolve_es_ref(base, "./sub/Game.zip"),
            Some(PathBuf::from("/roms/snes/sub/Game.zip"))
        );
        assert_eq!(
            resolve_es_ref(base, "./images/../Game.zip"),
            Some(PathBuf::from("/roms/snes/Game.zip")),
            "lexical .. normalization matches canonical provider keys"
        );
        assert_eq!(resolve_es_ref(base, "C:\\roms\\Game.zip"), None);
        assert_eq!(resolve_es_ref(base, "  "), None);
    }

    #[test]
    fn system_map_merges_only_aliases() {
        assert_eq!(fable_platform("megadrive"), "genesis");
        assert_eq!(fable_platform("atari800"), "atari8bit");
        assert_eq!(fable_platform("snes-msu"), "snes");
        assert_eq!(fable_platform("pcengine"), "tg16");
        assert_eq!(fable_platform("snes"), "snes");
        assert_eq!(fable_platform("x68000"), "x68000", "unknown keeps its own section");
        assert_eq!(fable_platform("sufami"), "sufami", "distinct hardware keeps its own section");
    }

    #[test]
    fn detection_finds_all_three_shapes() {
        let base = std::env::temp_dir().join(format!("ufm-es-detect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        // Install root shape.
        let snes = base.join("retrobat/roms/snes");
        std::fs::create_dir_all(&snes).unwrap();
        std::fs::write(snes.join("gamelist.xml"), "<gameList/>").unwrap();
        let det = detect_es_root(&base.join("retrobat")).unwrap();
        assert_eq!(det.kind, EsRootKind::InstallRoot);
        assert_eq!(det.systems.len(), 1);
        assert_eq!(det.systems[0].es_name, "snes");
        // Roms dir shape.
        let det = detect_es_root(&base.join("retrobat/roms")).unwrap();
        assert_eq!(det.kind, EsRootKind::RomsDir);
        // System dir shape.
        let det = detect_es_root(&snes).unwrap();
        assert_eq!(det.kind, EsRootKind::SystemDir);
        assert_eq!(det.systems[0].es_name, "snes");
        // Nothing there → None.
        let empty = base.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(detect_es_root(&empty).is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn real_fixtures_parse() {
        // The real RetroBat lists pulled from the Hyperbase drive
        // (point ES_FIXTURES_DIR at the folder holding
        // gamelist-{wii,psx,snes}.xml; skips quietly when unset).
        let Ok(dir) = std::env::var("ES_FIXTURES_DIR") else {
            return;
        };
        for (name, expect) in [("wii", 2usize), ("psx", 155), ("snes", 784)] {
            let path = format!("{dir}/gamelist-{name}.xml");
            let xml = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{name} fixture unreadable: {e}"));
            let list = parse_gamelist(&xml).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(list.games.len(), expect, "{name} game count");
            assert!(list.games.iter().all(|g| g.path.is_some()), "{name}: every entry has a path");
        }
    }
}
