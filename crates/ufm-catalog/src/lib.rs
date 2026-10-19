//! ufm-catalog: the durable metadata catalog.
//!
//! Spec §§25–26: SQLite in WAL mode. Full-text search tables, migrations with
//! integrity checks, labels, collections, view profiles, media identities,
//! job journals, theme/plugin package records, and MCP audit chains all live
//! here. The catalog is the reason search is instant and everything survives
//! a restart — the filesystem is never walked live for metadata.

use rusqlite::{params, Connection};
use ufm_core::{CollectionId, ItemId, JobId, LocationId, Result, UfmError};

/// Schema version. Migrations run in order; downgrades are refused.
pub const SCHEMA_VERSION: u32 = 1;

/// Parse the lowercase kind string the catalog stores
/// (`format!("{:?}", kind).to_lowercase()`). Unknown values degrade to
/// `File` so one odd row can never fail a whole search.
fn parse_kind(s: &str) -> ufm_vfs::ItemKind {
    match s {
        "file" => ufm_vfs::ItemKind::File,
        "folder" => ufm_vfs::ItemKind::Folder,
        "symlink" => ufm_vfs::ItemKind::Symlink,
        "archive" => ufm_vfs::ItemKind::Archive,
        "volume" => ufm_vfs::ItemKind::Volume,
        _ => ufm_vfs::ItemKind::File,
    }
}

pub struct Catalog {
    conn: Connection,
}

/// A stored media identity (see `Catalog::media_identity_for`).
#[derive(Debug, Clone)]
/// One cached ROM-media lookup result (ROM media enrichment): for
/// one identified ROM and one media type, whether the cascade found
/// a file, where it came from, and where it now lives. Negative
/// results are cached too — re-runs only chase what is still missing
/// (or what errored, which always replans).
pub struct RomMediaRow {
    pub item_id: String,
    pub media_type: String,
    /// found | missing | ambiguous | error
    pub status: String,
    /// libretro | screenscraper | thegamesdb | steamgriddb |
    /// emumovies | manual | null
    pub source: Option<String>,
    pub local_path: Option<String>,
    /// JSON: candidates for ambiguous rows, note for error rows.
    pub detail_json: String,
    pub checked_at: String,
}

pub struct MediaIdentityRow {
    pub item_id: String,
    pub media_kind: String,
    pub title: Option<String>,
    pub platform: Option<String>,
    pub sha1: Option<String>,
    pub evidence_json: String,
    /// Film/TV identity (Media Resolution); NULL for ROM rows.
    pub year: Option<i64>,
    pub tmdb_id: Option<i64>,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    pub episode_end: Option<i64>,
    pub confidence: Option<f64>,
    /// The user confirmed (or hand-corrected) this identity.
    pub confirmed: bool,
}

/// One collection as the sidebar lists it (Phase 2): identity plus
/// live membership counts. `missing_count` counts member paths that
/// no longer resolve to a catalog item (moved/deleted outside Fable).
#[derive(Debug, Clone)]
pub struct CollectionRow {
    pub id: String,
    pub name: String,
    /// manual | saved_search | smart
    pub kind: String,
    pub member_count: u64,
    pub missing_count: u64,
}

/// A stored per-location view profile (Phase 2): how a location
/// likes to be looked at. `sort_json` is `{"key":…,"dir":…}` shaped
/// by the UI; the catalog treats it as opaque text.
#[derive(Debug, Clone)]
pub struct ViewProfileRow {
    pub view_mode: String,
    pub sort_json: String,
    pub show_extensions: bool,
    /// Grand Pass steal 5: the group-by bucket key ("" | date |
    /// type | size), remembered with the rest of the view state.
    pub group_by: String,
}

/// Grand Pass steal 1: one row of the Home base "Recent" shelf.
#[derive(Debug, Clone, PartialEq)]
pub struct HomeRecentRow {
    pub path: String,
    pub name: String,
    /// The catalog item kind ("file", "folder", provider kinds…).
    pub kind: String,
    pub location_id: String,
    pub item_id: String,
    /// Unix seconds the file was last opened/previewed here.
    pub opened_at: i64,
}

/// Grand Pass steal 1: one row of the "You visit these a lot"
/// shelf — a folder, how often, and when last.
#[derive(Debug, Clone, PartialEq)]
pub struct HomeVisitRow {
    pub path: String,
    pub name: String,
    pub location_id: String,
    pub item_id: String,
    pub visits: i64,
    /// Unix seconds of the most recent visit.
    pub last_at: i64,
}

/// One persisted MCP audit row (Phase 3): every tool call the MCP
/// server authorizes lands here — allowed, denied, parked for human
/// approval, or failed — with its argument JSON, whether it was a
/// write, and how long it took. Append-only from the server's side;
/// retention pruning is the only delete (see `prune_mcp_audit`).
#[derive(Debug, Clone)]
pub struct McpAuditRow {
    pub seq: i64,
    /// RFC 3339 timestamp of the call.
    pub at: String,
    /// The paired client's id (UUID string).
    pub client: String,
    /// Tool name (`ufm_mcp::McpTool::name`).
    pub tool: String,
    /// The call's arguments as JSON text (server-truncated).
    pub args_json: String,
    /// JSON outcome object: `{"outcome":"allowed"|"denied"|"needs_approval"|"failed","detail":...}`.
    pub outcome: String,
    /// True when the tool mutates user metadata or files (tag, and
    /// the destructive trio) — denied/parked writes are flagged too.
    pub is_write: bool,
    pub duration_ms: i64,
}

/// One Media Completion run (scheduled or headless-invoked): the
/// morning summary's row. JSON columns keep the detail schemaless —
/// summary counts, the undo journal (every move + placed file),
/// pending confirmations awaiting a human, and the subtitle quota
/// state the run observed (API-reported values, never invented).
#[derive(Debug, Clone)]
pub struct MediaCompleteRunRow {
    pub id: String,
    pub schedule_id: Option<String>,
    pub schedule_name: String,
    pub target: String,
    /// RFC 3339 start/finish.
    pub started: String,
    pub finished: Option<String>,
    /// The run started after its scheduled slot (catch-up) — the
    /// summary admits it ran late.
    pub was_late: bool,
    /// running | completed | completed_with_failures | failed | undone
    pub status: String,
    pub summary_json: String,
    pub actions_json: String,
    pub pending_json: String,
    pub quota_json: Option<String>,
    pub undone_at: Option<String>,
}

fn map_mc_run(r: &rusqlite::Row) -> rusqlite::Result<MediaCompleteRunRow> {
    Ok(MediaCompleteRunRow {
        id: r.get(0)?,
        schedule_id: r.get(1)?,
        schedule_name: r.get(2)?,
        target: r.get(3)?,
        started: r.get(4)?,
        finished: r.get(5)?,
        was_late: r.get::<_, i64>(6)? != 0,
        status: r.get(7)?,
        summary_json: r.get(8)?,
        actions_json: r.get(9)?,
        pending_json: r.get(10)?,
        quota_json: r.get(11)?,
        undone_at: r.get(12)?,
    })
}

/// One Music Completion run (manual apply, lyrics save, or
/// scheduled): the music sibling of MediaCompleteRunRow — same
/// column shape, its own table. `actions_json` is the undo journal:
/// every tag write with its PRIOR values, every lyric write with
/// its prior text, every artwork embed, every sidecar created.
/// `pending_json` is the morning checklist: probable/unknown
/// matches a scheduled run refused to guess at.
#[derive(Debug, Clone)]
pub struct MusicCompleteRunRow {
    pub id: String,
    pub schedule_id: Option<String>,
    pub schedule_name: String,
    pub target: String,
    pub started: String,
    pub finished: Option<String>,
    pub was_late: bool,
    /// running | completed | completed_with_failures | failed | undone
    pub status: String,
    pub summary_json: String,
    pub actions_json: String,
    pub pending_json: String,
    pub undone_at: Option<String>,
}

fn map_mu_run(r: &rusqlite::Row) -> rusqlite::Result<MusicCompleteRunRow> {
    Ok(MusicCompleteRunRow {
        id: r.get(0)?,
        schedule_id: r.get(1)?,
        schedule_name: r.get(2)?,
        target: r.get(3)?,
        started: r.get(4)?,
        finished: r.get(5)?,
        was_late: r.get::<_, i64>(6)? != 0,
        status: r.get(7)?,
        summary_json: r.get(8)?,
        actions_json: r.get(9)?,
        pending_json: r.get(10)?,
        undone_at: r.get(11)?,
    })
}

/// One File Auditor run (manual or scheduled): the summary's durable
/// home. `summary_json` carries the counts per family, how much was
/// scanned, whether the walk truncated, and honest notes (e.g. a
/// checker that could not run because its tool was absent).
#[derive(Debug, Clone)]
pub struct AuditorRunRow {
    pub id: String,
    pub schedule_id: Option<String>,
    pub schedule_name: String,
    pub target: String,
    pub started: String,
    pub finished: Option<String>,
    pub was_late: bool,
    /// running | completed | failed
    pub status: String,
    pub summary_json: String,
}

fn map_auditor_run(r: &rusqlite::Row) -> rusqlite::Result<AuditorRunRow> {
    Ok(AuditorRunRow {
        id: r.get(0)?,
        schedule_id: r.get(1)?,
        schedule_name: r.get(2)?,
        target: r.get(3)?,
        started: r.get(4)?,
        finished: r.get(5)?,
        was_late: r.get::<_, i64>(6)? != 0,
        status: r.get(7)?,
        summary_json: r.get(8)?,
    })
}

/// One File Auditor finding — a checklist row. `lane` is `certain`
/// (stated as fact: signature, hash, or deterministic rule) or
/// `question` (a heuristic the user is ASKED about — never asserted).
/// `class_key` is the ask-lane identity: answers scoped "all similar"
/// or "never for this class" write `auditor_rules` rows keyed by it.
/// `proposal_json`: {"kind":"rename","to":...} |
/// {"kind":"trash"} | {"kind":"convert","convertKind":"flac"|"hevc"|"png","replace":bool,...} |
/// {"kind":"none"}.
#[derive(Debug, Clone)]
pub struct AuditorFlagRow {
    pub id: String,
    pub run_id: String,
    pub path: String,
    /// typo | pattern | corrupt | compat | space | junk | clutter |
    /// style | editions | cold
    pub family: String,
    /// certain | question
    pub lane: String,
    pub class_key: String,
    pub title: String,
    pub detail: String,
    pub proposal_json: Option<String>,
    /// open | confirmed | dismissed | superseded
    pub state: String,
    pub created: String,
    pub updated: String,
}

fn map_auditor_flag(r: &rusqlite::Row) -> rusqlite::Result<AuditorFlagRow> {
    Ok(AuditorFlagRow {
        id: r.get(0)?,
        run_id: r.get(1)?,
        path: r.get(2)?,
        family: r.get(3)?,
        lane: r.get(4)?,
        class_key: r.get(5)?,
        title: r.get(6)?,
        detail: r.get(7)?,
        proposal_json: r.get(8)?,
        state: r.get(9)?,
        created: r.get(10)?,
        updated: r.get(11)?,
    })
}

/// One duplicate-candidate group for `dedupe_analyze` (Phase 3):
/// files sharing an identical byte size. Candidates only — content
/// is NOT hashed, so equal size never proves equal content.
#[derive(Debug, Clone)]
pub struct DuplicateGroup {
    pub size_bytes: u64,
    pub count: u64,
    /// Bytes reclaimable IF all-but-one copies were identical:
    /// size * (count - 1). An upper bound, not a promise.
    pub wasted_bytes: u64,
    /// Up to five member paths (provider keys) as samples.
    pub sample_paths: Vec<String>,
}

impl Catalog {
    /// Open (or create) the catalog. WAL mode + foreign keys, always.
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let conn = Connection::open(path).map_err(|e| UfmError::Database(e.to_string()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;",
        )
        .map_err(|e| UfmError::Database(e.to_string()))?;
        let catalog = Self { conn };
        catalog.migrate()?;
        Ok(catalog)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn =
            Connection::open_in_memory().map_err(|e| UfmError::Database(e.to_string()))?;
        let catalog = Self { conn };
        catalog.migrate()?;
        Ok(catalog)
    }

    fn migrate(&self) -> Result<()> {
        // Migration 1: the core records from spec §26.
        self.conn
            .execute_batch(
                "
                CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY);
                CREATE TABLE IF NOT EXISTS locations (
                    id TEXT PRIMARY KEY,
                    provider TEXT NOT NULL,
                    display_name TEXT NOT NULL,
                    capabilities_json TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS items (
                    id TEXT PRIMARY KEY,
                    location_id TEXT NOT NULL REFERENCES locations(id),
                    name TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    size_bytes INTEGER,
                    modified TEXT,
                    provider_key TEXT,
                    UNIQUE(location_id, provider_key)
                );
                CREATE TABLE IF NOT EXISTS labels (
                    item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
                    label TEXT NOT NULL,
                    PRIMARY KEY (item_id, label)
                );
                CREATE TABLE IF NOT EXISTS path_labels (
                    provider_key TEXT NOT NULL,
                    label TEXT NOT NULL,
                    PRIMARY KEY (provider_key, label)
                );
                CREATE TABLE IF NOT EXISTS collections (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    kind TEXT NOT NULL,          -- manual | saved_search | smart
                    query_json TEXT              -- for saved_search / smart
                );
                CREATE TABLE IF NOT EXISTS collection_members (
                    collection_id TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
                    item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
                    PRIMARY KEY (collection_id, item_id)
                );
                CREATE TABLE IF NOT EXISTS view_profiles (
                    location_id TEXT NOT NULL REFERENCES locations(id) ON DELETE CASCADE,
                    dir_key TEXT NOT NULL DEFAULT '',
                    show_extensions INTEGER NOT NULL DEFAULT 1,
                    view_mode TEXT NOT NULL DEFAULT 'list',
                    sort_json TEXT NOT NULL DEFAULT '{}',
                    group_by TEXT NOT NULL DEFAULT '',
                    PRIMARY KEY (location_id, dir_key)
                );
                CREATE TABLE IF NOT EXISTS media_identities (
                    item_id TEXT PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
                    media_kind TEXT NOT NULL,    -- movie | episode | rom | …
                    title TEXT,
                    year INTEGER,
                    tmdb_id INTEGER,
                    platform TEXT,              -- for ROMs: nes | snes | …
                    sha1 TEXT,
                    match_evidence_json TEXT NOT NULL DEFAULT '{}'
                );
                CREATE TABLE IF NOT EXISTS job_journal (
                    seq INTEGER PRIMARY KEY AUTOINCREMENT,
                    job_id TEXT NOT NULL,
                    step_id TEXT NOT NULL,
                    from_state TEXT NOT NULL,
                    to_state TEXT NOT NULL,
                    at TEXT NOT NULL,
                    note TEXT
                );
                -- FTS over names (content extraction feeds more tables later)
                CREATE VIRTUAL TABLE IF NOT EXISTS items_fts USING fts5(name, content='items', content_rowid='rowid');
                INSERT OR IGNORE INTO schema_version (version) VALUES (1);
                ",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        // Migration 3: job snapshots for the transfer center (spec §25).
        // Step-level transitions live in job_journal (migration 1); this
        // table holds the whole-Job snapshot the executor rewrites on
        // every transition and crash recovery loads at startup.
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS jobs (
                    job_id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    state TEXT NOT NULL,
                    created TEXT NOT NULL,
                    job_json TEXT NOT NULL
                );",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        // Rule (smart folder) result store: the durable memory of
        // rule evaluations — see rule_snapshot / rule_refresh /
        // rule_probe_batch. A rule's last result set renders instantly
        // on revisit; the duration sweep resumes where it stopped.
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS rule_results (
                    rule_key TEXT PRIMARY KEY,
                    extensions TEXT NOT NULL,
                    min_duration_secs REAL,
                    evaluated_at TEXT NOT NULL,
                    candidates INTEGER NOT NULL DEFAULT 0,
                    member_count INTEGER NOT NULL DEFAULT 0,
                    pending INTEGER NOT NULL DEFAULT 0,
                    pending_offline INTEGER NOT NULL DEFAULT 0,
                    sweep_complete INTEGER NOT NULL DEFAULT 0
                );
                CREATE TABLE IF NOT EXISTS rule_result_items (
                    rule_key TEXT NOT NULL,
                    provider_key TEXT NOT NULL,
                    is_member INTEGER NOT NULL DEFAULT 0,
                    size_bytes INTEGER,
                    modified TEXT,
                    PRIMARY KEY (rule_key, provider_key)
                );",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        // Migration 2: cached media durations (seconds), filled lazily
        // by ffprobe when duration-based rules / smart folders evaluate.
        let has_duration: bool = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('items') WHERE name = 'duration_secs'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|c| c > 0)
            .unwrap_or(false);
        if !has_duration {
            self.conn
                .execute("ALTER TABLE items ADD COLUMN duration_secs REAL", [])
                .map_err(|e| UfmError::Database(e.to_string()))?;
        }
        // Media Resolution (film/TV identity): media_identities gains
        // the episode + confidence columns. ROM rows never set them;
        // the ROM upsert path is untouched.
        for (col, decl) in [
            ("season", "INTEGER"),
            ("episode", "INTEGER"),
            ("episode_end", "INTEGER"),
            ("confidence", "REAL"),
            ("confirmed", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            let has: bool = self
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('media_identities') WHERE name = ?1",
                    params![col],
                    |r| r.get::<_, i64>(0),
                )
                .map(|c| c > 0)
                .unwrap_or(false);
            if !has {
                self.conn
                    .execute(
                        &format!("ALTER TABLE media_identities ADD COLUMN {col} {decl}"),
                        [],
                    )
                    .map_err(|e| UfmError::Database(e.to_string()))?;
            }
        }
        // ROM media enrichment: cached per-(rom, media type) lookup
        // results. Lives beside media_identities; cascade misses are
        // data here, not absence of data.
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS rom_media (
                    item_id TEXT NOT NULL,
                    media_type TEXT NOT NULL,
                    status TEXT NOT NULL,
                    source TEXT,
                    local_path TEXT,
                    detail_json TEXT NOT NULL DEFAULT '{}',
                    checked_at TEXT NOT NULL,
                    PRIMARY KEY (item_id, media_type)
                );",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        // EmulationStation import (Hyperbase #5, 2026-10-17): the
        // descriptive metadata a gamelist carries that has no other
        // catalog home — description, credits, family, region, the
        // scrape provenance, and artwork refs Fable has no surface
        // for yet (marquee/fanart/manual). Path-keyed like
        // rom_game_state so it survives re-indexing; import-owned
        // (a re-import refreshes it wholesale), while identity,
        // personal state and found art merge under the precedence
        // rules in es_apply. Declared schema addition — see the
        // Hyperbase #5 receipt.
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS es_metadata (
                    provider_key TEXT PRIMARY KEY,
                    es_system TEXT NOT NULL,
                    platform TEXT NOT NULL,
                    gamelist_path TEXT NOT NULL,
                    name TEXT,
                    description TEXT,
                    developer TEXT,
                    publisher TEXT,
                    genre TEXT,
                    family TEXT,
                    players TEXT,
                    lang TEXT,
                    region TEXT,
                    release_year INTEGER,
                    releasedate_raw TEXT,
                    source_id TEXT,
                    scrap_source TEXT,
                    scrap_date TEXT,
                    md5 TEXT,
                    crc32 TEXT,
                    hidden INTEGER NOT NULL DEFAULT 0,
                    kidgame INTEGER NOT NULL DEFAULT 0,
                    gametime_secs INTEGER,
                    image_path TEXT,
                    marquee_path TEXT,
                    thumbnail_path TEXT,
                    fanart_path TEXT,
                    manual_path TEXT,
                    imported_at TEXT NOT NULL
                );",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        // PC Games (2026-10-17, PC Games Library P1): the imported
        // descriptive long tail for pc-game identities (Playnite /
        // folder-scan provenance), path-keyed and import-owned like
        // es_metadata — a re-import refreshes it wholesale, while
        // identity and personal state (rom_game_state /
        // rom_group_state, shared with ROMs by the design's one
        // game-state ruling) live apart. pc_game_state carries ONLY
        // install/launch facts the importer or a launch proof can
        // swear to: presence (verified on disk at import time) and
        // the launch badge, which starts at 'not-checked' and only
        // moves on an observed launch or Dickles' own mark — never
        // on a guess (the Forge "honest badge" rule).
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS pc_metadata (
                    provider_key TEXT PRIMARY KEY,
                    source_kind TEXT NOT NULL,
                    library_name TEXT NOT NULL,
                    playnite_id TEXT,
                    game_guid TEXT,
                    description TEXT,
                    genres_json TEXT NOT NULL DEFAULT '[]',
                    developers_json TEXT NOT NULL DEFAULT '[]',
                    publishers_json TEXT NOT NULL DEFAULT '[]',
                    series_json TEXT NOT NULL DEFAULT '[]',
                    features_json TEXT NOT NULL DEFAULT '[]',
                    age_ratings_json TEXT NOT NULL DEFAULT '[]',
                    links_json TEXT NOT NULL DEFAULT '[]',
                    storefronts_json TEXT NOT NULL DEFAULT '[]',
                    critic_score INTEGER,
                    community_score INTEGER,
                    install_size INTEGER,
                    version TEXT,
                    exe_name TEXT,
                    exe_rel TEXT,
                    release_raw TEXT,
                    mislabel_note TEXT,
                    imported_at TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS pc_game_state (
                    provider_key TEXT PRIMARY KEY,
                    present INTEGER NOT NULL DEFAULT 0,
                    launch_badge TEXT NOT NULL DEFAULT 'not-checked',
                    launch_note TEXT,
                    checked_at TEXT,
                    updated_at TEXT NOT NULL
                );",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        // Ebook P1 — "The Library" (2026-10-17): a book's identity and
        // descriptive metadata, decoded from the file itself at
        // identify time (EPUB OPF Dublin Core + series; PDF info
        // dictionary). book_metadata is keyed by item_id like
        // media_identities — the metadata is re-derivable from the
        // file at any time, so it needs no path-survival of its own;
        // the freshness snapshot (size_bytes + modified, the ES
        // import contract) decides when a re-identify is owed.
        // book_authors carries every credited author as an exact row
        // (position 0 = primary) so Author shelves and the author:
        // facet match exactly, never by LIKE over JSON. book_state is
        // the reading state (want | reading | read), keyed by
        // provider_key per the Phase-2 user-metadata pattern so it
        // survives re-indexing; origin records whether the user set
        // it or progress automation did.
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS book_metadata (
                    item_id TEXT PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
                    provider_key TEXT NOT NULL,
                    format TEXT NOT NULL,
                    title TEXT,
                    series TEXT,
                    series_index REAL,
                    publisher TEXT,
                    published_date TEXT,
                    pub_year INTEGER,
                    language TEXT,
                    description TEXT,
                    subjects_json TEXT NOT NULL DEFAULT '[]',
                    isbn TEXT,
                    identifier TEXT,
                    spine_count INTEGER,
                    page_count INTEGER,
                    cover_path TEXT,
                    source TEXT NOT NULL,
                    size_bytes INTEGER,
                    modified TEXT,
                    identified_at TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_book_metadata_series
                    ON book_metadata (series);
                CREATE INDEX IF NOT EXISTS idx_book_metadata_provider_key
                    ON book_metadata (provider_key);
                CREATE TABLE IF NOT EXISTS book_authors (
                    item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
                    author TEXT NOT NULL,
                    position INTEGER NOT NULL,
                    PRIMARY KEY (item_id, position)
                );
                CREATE INDEX IF NOT EXISTS idx_book_authors_author
                    ON book_authors (author);
                CREATE TABLE IF NOT EXISTS book_state (
                    provider_key TEXT PRIMARY KEY,
                    state TEXT NOT NULL,
                    origin TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        // ROM Home (2026-10-15): per-game personal state — ratings,
        // favorites, completion, play history, and the user's own
        // overrides — keyed by path like path_stars so it survives
        // re-indexing. The room's shelf queries join it in.
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS rom_game_state (
                    provider_key TEXT PRIMARY KEY,
                    rating INTEGER,
                    favorite INTEGER NOT NULL DEFAULT 0,
                    completed INTEGER NOT NULL DEFAULT 0,
                    play_count INTEGER NOT NULL DEFAULT 0,
                    last_played TEXT,
                    title_override TEXT,
                    genre TEXT,
                    players TEXT,
                    notes TEXT,
                    updated_at TEXT NOT NULL
                );",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        // Music Glow-Up Phase 2 (crew Diva, 2026-10-18): per-track
        // listening state — play history, resume positions, and
        // completion counts — keyed by provider_key (the file's
        // path) exactly like rom_game_state, so it survives
        // re-indexing. Written by the shared player core (player.rs).
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS audio_play_state (
                    provider_key TEXT PRIMARY KEY,
                    play_count INTEGER NOT NULL DEFAULT 0,
                    last_played_at TEXT,
                    last_position_secs REAL NOT NULL DEFAULT 0,
                    completed_count INTEGER NOT NULL DEFAULT 0,
                    updated_at TEXT NOT NULL
                );",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        // Phase 3 grouping: the GAME-scoped half of rom_game_state
        // (rating, favorite, completed, title_override, genre,
        // players, notes) lives here, keyed by group key
        // (platform + normalized base title) — rating Klax once
        // rates every Klax. Play history stays per-path in
        // rom_game_state (it is history of a file). Rows arrive via
        // backfill_rom_group_state at the end of migrate().
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS rom_group_state (
                    group_key TEXT PRIMARY KEY,
                    rating INTEGER,
                    favorite INTEGER NOT NULL DEFAULT 0,
                    completed INTEGER NOT NULL DEFAULT 0,
                    title_override TEXT,
                    genre TEXT,
                    players TEXT,
                    notes TEXT,
                    updated_at TEXT NOT NULL
                );",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        // Indexer v2 (Library Memory): persistent per-volume records.
        // A volume's identity is its FILESYSTEM UUID; where it has
        // been mounted is history, never identity (the same drive at
        // a new mount point is the same volume). Items resolve to a
        // volume by longest-prefix match of provider_key against the
        // current mounts, then this table's mount history.
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS volumes (
                    fs_uuid TEXT PRIMARY KEY,
                    label TEXT,
                    first_seen TEXT NOT NULL,
                    last_seen TEXT NOT NULL,
                    last_indexed TEXT,
                    mount_history_json TEXT NOT NULL DEFAULT '[]'
                );",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        // Network Locations: the volumes table's mirror for servers.
        // Identity is the normalized server URI (scheme://[user@]host
        // [:port]/path — never a password); the GVfs FUSE prefixes it
        // has been mounted at are history, never identity. Items on a
        // server that has gone away keep their catalog rows and badge
        // from this table exactly like unplugged drives do.
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS network_locations (
                    uri TEXT PRIMARY KEY,
                    display_name TEXT,
                    first_seen TEXT NOT NULL,
                    last_seen TEXT NOT NULL,
                    last_indexed TEXT,
                    mount_history_json TEXT NOT NULL DEFAULT '[]'
                );",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        // Phase 2 (collections / stars / view profiles made real):
        // stars are path-keyed exactly like labels (path_labels) — the
        // provider_key is the identity that survives re-indexing.
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS path_stars (
                    provider_key TEXT PRIMARY KEY,
                    starred_at TEXT NOT NULL
                );",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        // collection_members shipped (migration 1) keyed by item_id with
        // ON DELETE CASCADE — a design that cannot survive a file move:
        // the moved file's old item row is deleted (delete_unseen) and
        // the cascade wiped its memberships. Rebuild path-keyed:
        // membership stores the provider_key and resolves to the live
        // item at read time. Rows that still resolve are copied across;
        // the old table was never writable through any command, so in
        // practice this copies nothing — but it is correct if it does.
        let members_path_keyed: bool = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('collection_members') WHERE name = 'provider_key'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|c| c > 0)
            .unwrap_or(false);
        if !members_path_keyed {
            self.conn
                .execute_batch(
                    "CREATE TABLE collection_members_pk (
                        collection_id TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
                        provider_key TEXT NOT NULL,
                        added_at TEXT,
                        PRIMARY KEY (collection_id, provider_key)
                    );
                    INSERT OR IGNORE INTO collection_members_pk (collection_id, provider_key)
                        SELECT cm.collection_id, i.provider_key
                        FROM collection_members cm
                        JOIN items i ON i.id = cm.item_id
                        WHERE i.provider_key IS NOT NULL;
                    DROP TABLE collection_members;
                    ALTER TABLE collection_members_pk RENAME TO collection_members;",
                )
                .map_err(|e| UfmError::Database(e.to_string()))?;
        }
        // Grand Pass steal 5 (2026-10-18): view_profiles grows from
        // per-LOCATION rows to per-(location, folder) rows. The old
        // table's PRIMARY KEY is location_id alone, which SQLite
        // cannot widen in place — rebuild it: every existing row
        // becomes the location-level row (dir_key '') for its
        // location, so nothing remembered is lost; folder overrides
        // land as their own rows from here on. Feature-detected by
        // the dir_key column, same discipline as collection_members.
        let view_profiles_folder_keyed: bool = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('view_profiles') WHERE name = 'dir_key'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|c| c > 0)
            .unwrap_or(false);
        if !view_profiles_folder_keyed {
            self.conn
                .execute_batch(
                    "CREATE TABLE view_profiles_new (
                        location_id TEXT NOT NULL REFERENCES locations(id) ON DELETE CASCADE,
                        dir_key TEXT NOT NULL DEFAULT '',
                        show_extensions INTEGER NOT NULL DEFAULT 1,
                        view_mode TEXT NOT NULL DEFAULT 'list',
                        sort_json TEXT NOT NULL DEFAULT '{}',
                        group_by TEXT NOT NULL DEFAULT '',
                        PRIMARY KEY (location_id, dir_key)
                    );
                    INSERT OR IGNORE INTO view_profiles_new
                        (location_id, dir_key, show_extensions, view_mode, sort_json, group_by)
                    SELECT location_id, '', show_extensions, view_mode, sort_json, ''
                    FROM view_profiles;
                    DROP TABLE view_profiles;
                    ALTER TABLE view_profiles_new RENAME TO view_profiles;",
                )
                .map_err(|e| UfmError::Database(e.to_string()))?;
        }
        // Grand Pass steal 1 (Home base): what this computer has
        // seen its owner touch — recent files and visited folders.
        // LOCAL-ONLY by doctrine: these tables never leave the
        // catalog, nothing syncs them, and the Preferences switch
        // can stop the recording and wipe both tables.
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS home_recents (
                     path TEXT PRIMARY KEY,
                     name TEXT NOT NULL,
                     kind TEXT NOT NULL DEFAULT '',
                     location_id TEXT NOT NULL DEFAULT '',
                     item_id TEXT NOT NULL DEFAULT '',
                     opened_at INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE INDEX IF NOT EXISTS idx_home_recents_opened
                     ON home_recents(opened_at DESC);
                 CREATE TABLE IF NOT EXISTS home_visits (
                     path TEXT PRIMARY KEY,
                     name TEXT NOT NULL,
                     location_id TEXT NOT NULL DEFAULT '',
                     item_id TEXT NOT NULL DEFAULT '',
                     visits INTEGER NOT NULL DEFAULT 0,
                     last_at INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE INDEX IF NOT EXISTS idx_home_visits_rank
                     ON home_visits(visits DESC, last_at DESC);",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;

        // Phase 3: the MCP audit chain (docs/MCP_API.md item 5 —
        // append-only, queryable in the app). One row per tool call.
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS mcp_audit (
                    seq INTEGER PRIMARY KEY AUTOINCREMENT,
                    at TEXT NOT NULL,
                    client TEXT NOT NULL,
                    tool TEXT NOT NULL,
                    args_json TEXT NOT NULL,
                    outcome TEXT NOT NULL,
                    is_write INTEGER NOT NULL DEFAULT 0,
                    duration_ms INTEGER NOT NULL DEFAULT 0
                );
                CREATE INDEX IF NOT EXISTS idx_mcp_audit_seq ON mcp_audit (seq DESC);",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        // Media Completion: one row per scheduled (or manually
        // invoked headless) completion run — the morning summary's
        // durable home. The JSON columns carry the run's detail:
        // summary counts, the undo journal (moves + placed files),
        // pending confirmations, failures, and quota state. Rows are
        // updated once (undo stamp); retention prunes old runs.
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS mediacomplete_runs (
                    id TEXT PRIMARY KEY,
                    schedule_id TEXT,
                    schedule_name TEXT NOT NULL,
                    target TEXT NOT NULL,
                    started TEXT NOT NULL,
                    finished TEXT,
                    was_late INTEGER NOT NULL DEFAULT 0,
                    status TEXT NOT NULL,
                    summary_json TEXT NOT NULL DEFAULT '{}',
                    actions_json TEXT NOT NULL DEFAULT '[]',
                    pending_json TEXT NOT NULL DEFAULT '[]',
                    quota_json TEXT,
                    undone_at TEXT
                );
                CREATE INDEX IF NOT EXISTS idx_mc_runs_started ON mediacomplete_runs (started DESC);",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        // Music Completion: one row per enrichment run (manual
        // apply, lyrics save, or scheduled) — the run journal whose
        // actions_json is what Undo reads (prior tag values ride
        // along). Mirrors mediacomplete_runs minus the subtitle
        // quota column (LRCLIB/MusicBrainz have no quota to report).
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS musiccomplete_runs (
                    id TEXT PRIMARY KEY,
                    schedule_id TEXT,
                    schedule_name TEXT NOT NULL,
                    target TEXT NOT NULL,
                    started TEXT NOT NULL,
                    finished TEXT,
                    was_late INTEGER NOT NULL DEFAULT 0,
                    status TEXT NOT NULL,
                    summary_json TEXT NOT NULL DEFAULT '{}',
                    actions_json TEXT NOT NULL DEFAULT '[]',
                    pending_json TEXT NOT NULL DEFAULT '[]',
                    undone_at TEXT
                );
                CREATE INDEX IF NOT EXISTS idx_mu_runs_started ON musiccomplete_runs (started DESC);",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        // File Auditor: runs (the summary's durable home, mirroring
        // mediacomplete_runs), flags (the checklist — one row per
        // finding, with its lane, its ask-lane class identity, and
        // its lifecycle state), dismissals (the permanent "this one
        // is OK" memory, per path + family), rules (class-level law
        // made by ask-lane answers), user words (confirmed names —
        // the typo hunt's proper-noun ground truth), and the content
        // hash cache that makes re-audits cheap.
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS auditor_runs (
                    id TEXT PRIMARY KEY,
                    schedule_id TEXT,
                    schedule_name TEXT NOT NULL,
                    target TEXT NOT NULL,
                    started TEXT NOT NULL,
                    finished TEXT,
                    was_late INTEGER NOT NULL DEFAULT 0,
                    status TEXT NOT NULL,
                    summary_json TEXT NOT NULL DEFAULT '{}'
                );
                CREATE INDEX IF NOT EXISTS idx_auditor_runs_started ON auditor_runs (started DESC);
                CREATE TABLE IF NOT EXISTS auditor_flags (
                    id TEXT PRIMARY KEY,
                    run_id TEXT NOT NULL,
                    path TEXT NOT NULL,
                    family TEXT NOT NULL,
                    lane TEXT NOT NULL,
                    class_key TEXT NOT NULL DEFAULT '',
                    title TEXT NOT NULL,
                    detail TEXT NOT NULL DEFAULT '',
                    proposal_json TEXT,
                    state TEXT NOT NULL DEFAULT 'open',
                    created TEXT NOT NULL,
                    updated TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_auditor_flags_state ON auditor_flags (state);
                CREATE INDEX IF NOT EXISTS idx_auditor_flags_path ON auditor_flags (path);
                CREATE TABLE IF NOT EXISTS auditor_dismissals (
                    path TEXT NOT NULL,
                    family TEXT NOT NULL,
                    created TEXT NOT NULL,
                    PRIMARY KEY (path, family)
                );
                CREATE TABLE IF NOT EXISTS auditor_rules (
                    class_key TEXT PRIMARY KEY,
                    decision TEXT NOT NULL,
                    created TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS auditor_user_words (
                    word TEXT PRIMARY KEY,
                    created TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS auditor_hashes (
                    path TEXT PRIMARY KEY,
                    size INTEGER NOT NULL,
                    mtime TEXT NOT NULL,
                    sha256 TEXT NOT NULL
                );",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        // Hyperbase #4 (2026-10-17): every ROM Home scan filters
        // media_identities by kind + platform; the index keeps shelf
        // windows cheap at 100k+ identified games. Additive only.
        self.conn
            .execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_mi_kind_platform
                 ON media_identities (media_kind, platform, title);",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        self.backfill_rom_group_state()?;
        Ok(())
    }

    /// Mount a location in the catalog. Locations must exist before items
    /// reference them (foreign keys are enforced).
    pub fn insert_location(&self, location: &ufm_vfs::Location) -> Result<()> {
        let caps = serde_json::to_string(&location.capabilities)
            .map_err(|e| UfmError::Database(e.to_string()))?;
        self.conn
            .execute(
                "INSERT OR REPLACE INTO locations (id, provider, display_name, capabilities_json)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    location.id.to_string(),
                    location.provider,
                    location.display_name,
                    caps,
                ],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// Record an item discovered by the indexer/watcher. Items must exist
    /// before labels, collection memberships, or media identities reference
    /// them (foreign keys are enforced).
    pub fn insert_item(&self, item: &ufm_vfs::Item) -> Result<()> {
        // Re-walks mint fresh ItemIds; reuse the stored id for the same
        // (location_id, provider_key) so labels/collections survive a
        // refresh and the FTS row stays put.
        let existing: Option<(
            String,
            i64,
            String,
            Option<i64>,
            Option<String>,
            Option<f64>,
        )> = match item.provider_key.as_deref() {
            Some(pk) => self
                .conn
                .query_row(
                    "SELECT id, rowid, name, size_bytes, modified, duration_secs
                     FROM items WHERE location_id = ?1 AND provider_key = ?2",
                    params![item.location.to_string(), pk],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                        ))
                    },
                )
                .ok(),
            None => None,
        };
        // A probed duration describes THIS (size, modified) of the
        // file. Carry it across the REPLACE when the file is
        // unchanged; a changed file honestly becomes unprobed again
        // instead of wearing a stale duration. (Before this, the
        // REPLACE reset every re-indexed file's duration to NULL,
        // so smart folders re-probed from scratch on every visit.)
        let carried_duration: Option<f64> = match &existing {
            Some((_, _, _, esize, emod, edur)) => {
                let unchanged = *esize == item.size_bytes.map(|s| s as i64)
                    && *emod == item.modified.map(|m| m.to_rfc3339());
                if unchanged {
                    *edur
                } else {
                    None
                }
            }
            None => None,
        };
        let stable_id: String = existing
            .as_ref()
            .map(|(id, ..)| id.clone())
            .unwrap_or_else(|| item.id.to_string());
        // items_fts is external-content: the REPLACE below deletes the
        // items row and re-inserts it under a NEW rowid, which would
        // orphan the old FTS entry (ghost hits, and worse, a recycled
        // rowid can join the ghost to an unrelated item). Retire the
        // old entry with the FTS 'delete' command first, exactly as
        // delete_unseen does.
        if let Some((_, old_rowid, old_name, _, _, _)) = &existing {
            self.conn
                .execute(
                    "INSERT INTO items_fts (items_fts, rowid, name) VALUES ('delete', ?1, ?2)",
                    params![old_rowid, old_name],
                )
                .map_err(|e| UfmError::Database(e.to_string()))?;
        }
        self.conn
            .execute(
                "INSERT OR REPLACE INTO items
                 (id, location_id, name, kind, size_bytes, modified, provider_key, duration_secs)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    stable_id,
                    item.location.to_string(),
                    item.name,
                    format!("{:?}", item.kind).to_lowercase(),
                    item.size_bytes.map(|s| s as i64),
                    item.modified.map(|m| m.to_rfc3339()),
                    item.provider_key,
                    carried_duration,
                ],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        // Keep the FTS index in sync (triggers come with the indexer milestone;
        // the scaffold writes through explicitly).
        self.conn
            .execute(
                "INSERT OR REPLACE INTO items_fts (rowid, name) VALUES
                 ((SELECT rowid FROM items WHERE id = ?1), ?2)",
                params![stable_id, item.name],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// Delete every item of `location` whose provider_key is not in
    /// `seen` — used after a fresh full re-walk to drop rows for files
    /// that no longer exist. Rows without a provider_key are kept (they
    /// did not come from the filesystem walk). Returns the delete count.
    pub fn delete_unseen(
        &self,
        location: &LocationId,
        seen: &std::collections::HashSet<String>,
    ) -> Result<u64> {
        let mut stmt = self
            .conn
            .prepare("SELECT rowid, id, name, provider_key FROM items WHERE location_id = ?1")
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![location.to_string()], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut stale: Vec<(i64, String, String)> = Vec::new();
        for row in rows {
            let (rowid, id, name, pk) = row.map_err(|e| UfmError::Database(e.to_string()))?;
            if let Some(pk) = pk {
                if !seen.contains(&pk) {
                    stale.push((rowid, id, name));
                }
            }
        }
        drop(stmt);
        let mut deleted = 0u64;
        for (rowid, id, name) in stale {
            // External-content FTS5: remove the index entry explicitly.
            self.conn
                .execute(
                    "INSERT INTO items_fts (items_fts, rowid, name) VALUES ('delete', ?1, ?2)",
                    params![rowid, name],
                )
                .map_err(|e| UfmError::Database(e.to_string()))?;
            self.conn
                .execute("DELETE FROM items WHERE id = ?1", params![id])
                .map_err(|e| UfmError::Database(e.to_string()))?;
            deleted += 1;
        }
        Ok(deleted)
    }

    /// Full-text search over item names via the `items_fts` FTS5 table,
    /// ranked with `bm25` (lower = better match). Returns `(item, rank)`
    /// pairs ordered best-first, capped at `limit` rows. Empty or
    /// whitespace-only queries return no rows instead of erroring.
    ///
    /// Items are reconstructed from the `items` rows joined back on
    /// `rowid`; `labels`/`starred` are not populated here (they come from
    /// their own tables in a later milestone).
    pub fn search_fts(&self, fts_query: &str, limit: u32) -> Result<Vec<(ufm_vfs::Item, f64)>> {
        if fts_query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self
            .conn
            .prepare(
                "SELECT i.id, i.location_id, i.name, i.kind, i.size_bytes,
                        i.modified, i.provider_key, bm25(items_fts) AS rank
                 FROM items_fts
                 JOIN items AS i ON i.rowid = items_fts.rowid
                 WHERE items_fts MATCH ?1
                 ORDER BY rank
                 LIMIT ?2",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![fts_query, limit as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, f64>(7)?,
                ))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, location_id, name, kind_s, size_bytes, modified_s, provider_key, rank) =
                row.map_err(|e| UfmError::Database(e.to_string()))?;
            let parse_id = |s: &str, what: &str| {
                uuid::Uuid::parse_str(s)
                    .map_err(|e| UfmError::Database(format!("bad {what} id {s:?}: {e}")))
            };
            out.push((
                ufm_vfs::Item {
                    id: ufm_core::ItemId(parse_id(&id, "item")?),
                    location: ufm_core::LocationId(parse_id(&location_id, "location")?),
                    name,
                    kind: parse_kind(&kind_s),
                    size_bytes: size_bytes.map(|s| s as u64),
                    modified: modified_s.and_then(|s| {
                        chrono::DateTime::parse_from_rfc3339(&s)
                            .ok()
                            .map(|dt| dt.with_timezone(&chrono::Utc))
                    }),
                    provider_key,
                    labels: Vec::new(),
                    starred: false,
                },
                rank,
            ));
        }
        Ok(out)
    }

    /// Fetch one item by id (Phase 3: MCP `read_item`/`get_metadata`
    /// address items by the ids search handed out). Labels/starred are
    /// NOT hydrated — the stores are path-keyed; callers hydrate via
    /// `labels_for_path` / `is_starred_path` like `finish_search` does.
    pub fn item_by_id(&self, id: &ItemId) -> Result<Option<ufm_vfs::Item>> {
        self.item_by_query("SELECT id, location_id, name, kind, size_bytes, modified, provider_key
                 FROM items WHERE id = ?1", &id.to_string())
    }

    /// Fetch one item by provider key (absolute path for local
    /// providers) — the by-path twin of `item_by_id`, for MCP
    /// `get_metadata` calls that name a path. If several locations
    /// somehow hold the same key, the first row wins.
    pub fn item_by_path(&self, provider_key: &str) -> Result<Option<ufm_vfs::Item>> {
        self.item_by_query("SELECT id, location_id, name, kind, size_bytes, modified, provider_key
                 FROM items WHERE provider_key = ?1 LIMIT 1", provider_key)
    }

    /// Shared row mapping for the single-item getters above.
    fn item_by_query(&self, sql: &str, key: &str) -> Result<Option<ufm_vfs::Item>> {
        use rusqlite::OptionalExtension;
        let row = self
            .conn
            .query_row(sql, params![key], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                ))
            })
            .optional()
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let Some((id, location_id, name, kind_s, size_bytes, modified_s, provider_key)) = row
        else {
            return Ok(None);
        };
        let parse_id = |s: &str, what: &str| {
            uuid::Uuid::parse_str(s)
                .map_err(|e| UfmError::Database(format!("bad {what} id {s:?}: {e}")))
        };
        Ok(Some(ufm_vfs::Item {
            id: ufm_core::ItemId(parse_id(&id, "item")?),
            location: ufm_core::LocationId(parse_id(&location_id, "location")?),
            name,
            kind: parse_kind(&kind_s),
            size_bytes: size_bytes.map(|s| s as u64),
            modified: modified_s.and_then(|s| {
                chrono::DateTime::parse_from_rfc3339(&s)
                    .ok()
                    .map(|dt| dt.with_timezone(&chrono::Utc))
            }),
            provider_key,
            labels: Vec::new(),
            starred: false,
        }))
    }

    /// Browse items by file extension (advanced-search type filter with
    /// no text query): files whose name ends in one of `exts`
    /// (case-insensitive), most recently modified first.
    pub fn browse_by_extensions(
        &self,
        exts: &[&str],
        limit: u32,
    ) -> Result<Vec<ufm_vfs::Item>> {
        if exts.is_empty() {
            return Ok(Vec::new());
        }
        let likes: Vec<String> = exts.iter().map(|e| format!("%.{}", e.to_lowercase())).collect();
        let where_clause = likes
            .iter()
            .enumerate()
            .map(|(i, _)| format!("LOWER(i.name) LIKE ?{}", i + 1))
            .collect::<Vec<_>>()
            .join(" OR ");
        let sql = format!(
            "SELECT i.id, i.location_id, i.name, i.kind, i.size_bytes, i.modified, i.provider_key
             FROM items AS i
             WHERE ({where_clause}) AND i.kind = 'file'
             ORDER BY i.modified DESC
             LIMIT ?{}",
            likes.len() + 1
        );
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut params: Vec<rusqlite::types::Value> =
            likes.iter().map(|l| rusqlite::types::Value::Text(l.clone())).collect();
        params.push(rusqlite::types::Value::Integer(limit as i64));
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, location_id, name, kind_s, size_bytes, modified_s, provider_key) =
                row.map_err(|e| UfmError::Database(e.to_string()))?;
            let parse_id = |s: &str, what: &str| {
                uuid::Uuid::parse_str(s)
                    .map_err(|e| UfmError::Database(format!("bad {what} id {s:?}: {e}")))
            };
            out.push(ufm_vfs::Item {
                id: ufm_core::ItemId(parse_id(&id, "item")?),
                location: ufm_core::LocationId(parse_id(&location_id, "location")?),
                name,
                kind: parse_kind(&kind_s),
                size_bytes: size_bytes.map(|s| s as u64),
                modified: modified_s.and_then(|s| {
                    chrono::DateTime::parse_from_rfc3339(&s)
                        .ok()
                        .map(|dt| dt.with_timezone(&chrono::Utc))
                }),
                provider_key,
                labels: Vec::new(),
                starred: false,
            });
        }
        Ok(out)
    }

    /// Record a label/tag on an item. Tags are virtual organization —
    /// they never move the file (spec §11).
    pub fn tag(&self, item: &ItemId, label: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO labels (item_id, label) VALUES (?1, ?2)",
                params![item.to_string(), label],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn untag(&self, item: &ItemId, label: &str) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM labels WHERE item_id = ?1 AND label = ?2",
                params![item.to_string(), label],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn labels_for(&self, item: &ItemId) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT label FROM labels WHERE item_id = ?1 ORDER BY label")
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![item.to_string()], |row| row.get(0))
            .map_err(|e| UfmError::Database(e.to_string()))?;
        rows.collect::<std::result::Result<Vec<String>, _>>()
            .map_err(|e| UfmError::Database(e.to_string()))
    }

    /// Path-keyed label assignment. The VFS mints ephemeral item ids
    /// per listing, so user-facing tags anchor to the provider key
    /// (absolute path) — stable across listings and re-indexing.
    pub fn tag_path(&self, provider_key: &str, label: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO path_labels (provider_key, label) VALUES (?1, ?2)",
                params![provider_key, label],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn untag_path(&self, provider_key: &str, label: &str) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM path_labels WHERE provider_key = ?1 AND label = ?2",
                params![provider_key, label],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn labels_for_path(&self, provider_key: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT label FROM path_labels WHERE provider_key = ?1 ORDER BY label")
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![provider_key], |row| row.get(0))
            .map_err(|e| UfmError::Database(e.to_string()))?;
        rows.collect::<std::result::Result<Vec<String>, _>>()
            .map_err(|e| UfmError::Database(e.to_string()))
    }

    /// Rename a label everywhere it is assigned (definition renames in
    /// the UI cascade here).
    pub fn rename_path_label(&self, old: &str, new: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE OR IGNORE path_labels SET label = ?2 WHERE label = ?1",
                params![old, new],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        self.conn
            .execute("DELETE FROM path_labels WHERE label = ?1", params![old])
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// Strip a label from every path (definition deleted in the UI).
    pub fn delete_path_label(&self, label: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM path_labels WHERE label = ?1", params![label])
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// All catalog items carrying a label (sidebar label browsing).
    pub fn items_by_label(&self, label: &str) -> Result<Vec<ufm_vfs::Item>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT i.id, i.location_id, i.name, i.kind, i.size_bytes,
                        i.modified, i.provider_key
                 FROM items AS i
                 JOIN path_labels AS pl ON pl.provider_key = i.provider_key
                 WHERE pl.label = ?1
                 ORDER BY i.name COLLATE NOCASE",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![label], |row| {
                let provider_key: Option<String> = row.get(6)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    provider_key,
                ))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut items = Vec::new();
        for row in rows {
            let (id, location_id, name, kind_s, size_bytes, modified_s, provider_key) =
                row.map_err(|e| UfmError::Database(e.to_string()))?;
            let parse_id = |s: &str, what: &str| {
                uuid::Uuid::parse_str(s)
                    .map_err(|e| UfmError::Database(format!("bad {what} id {s:?}: {e}")))
            };
            let labels = match &provider_key {
                Some(pk) => self.labels_for_path(pk).unwrap_or_default(),
                None => Vec::new(),
            };
            items.push(ufm_vfs::Item {
                id: ufm_core::ItemId(parse_id(&id, "item")?),
                location: ufm_core::LocationId(parse_id(&location_id, "location")?),
                name,
                kind: parse_kind(&kind_s),
                size_bytes: size_bytes.map(|v| v as u64),
                modified: modified_s.and_then(|m| {
                    chrono::DateTime::parse_from_rfc3339(&m)
                        .ok()
                        .map(|d| d.with_timezone(&chrono::Utc))
                }),
                provider_key,
                labels,
                starred: false,
            });
        }
        Ok(items)
    }

    /// Candidates for extension/duration rules: every item whose name
    /// ends in one of `exts` (no dot, lowercase), with its cached
    /// duration (None = not probed yet).
    pub fn rule_candidates(
        &self,
        exts: &[String],
    ) -> Result<Vec<(ufm_vfs::Item, Option<f64>)>> {
        if exts.is_empty() {
            return Ok(Vec::new());
        }
        let conds: Vec<String> = exts
            .iter()
            .map(|_| "lower(i.name) LIKE '%.' || ?".to_string())
            .collect();
        let sql = format!(
            "SELECT i.id, i.location_id, i.name, i.kind, i.size_bytes,
                    i.modified, i.provider_key, i.duration_secs
             FROM items AS i
             WHERE ({})
             ORDER BY i.name COLLATE NOCASE",
            conds.join(" OR ")
        );
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(exts.iter()), |row| {
                let provider_key: Option<String> = row.get(6)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    provider_key,
                    row.get::<_, Option<f64>>(7)?,
                ))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, location_id, name, kind_s, size_bytes, modified_s, provider_key, duration) =
                row.map_err(|e| UfmError::Database(e.to_string()))?;
            let parse_id = |s: &str, what: &str| {
                uuid::Uuid::parse_str(s)
                    .map_err(|e| UfmError::Database(format!("bad {what} id {s:?}: {e}")))
            };
            let labels = match &provider_key {
                Some(pk) => self.labels_for_path(pk).unwrap_or_default(),
                None => Vec::new(),
            };
            out.push((
                ufm_vfs::Item {
                    id: ufm_core::ItemId(parse_id(&id, "item")?),
                    location: ufm_core::LocationId(parse_id(&location_id, "location")?),
                    name,
                    kind: parse_kind(&kind_s),
                    size_bytes: size_bytes.map(|v| v as u64),
                    modified: modified_s.and_then(|m| {
                        chrono::DateTime::parse_from_rfc3339(&m)
                            .ok()
                            .map(|d| d.with_timezone(&chrono::Utc))
                    }),
                    provider_key,
                    labels,
                    starred: false,
                },
                duration,
            ));
        }
        Ok(out)
    }

    /// One stored media-identity row (ROM identification results).
    pub fn media_identity_for(&self, item_id: &ItemId) -> Result<Option<MediaIdentityRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT item_id, media_kind, title, platform, sha1, match_evidence_json,
                        year, tmdb_id, season, episode, episode_end, confidence, confirmed
                 FROM media_identities WHERE item_id = ?1",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut rows = stmt
            .query_map(params![item_id.to_string()], |row| {
                Ok(MediaIdentityRow {
                    item_id: row.get(0)?,
                    media_kind: row.get(1)?,
                    title: row.get(2)?,
                    platform: row.get(3)?,
                    sha1: row.get(4)?,
                    evidence_json: row.get(5)?,
                    year: row.get(6)?,
                    tmdb_id: row.get(7)?,
                    season: row.get(8)?,
                    episode: row.get(9)?,
                    episode_end: row.get(10)?,
                    confidence: row.get(11)?,
                    confirmed: row.get::<_, i64>(12)? != 0,
                })
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        match rows.next() {
            Some(r) => Ok(Some(r.map_err(|e| UfmError::Database(e.to_string()))?)),
            None => Ok(None),
        }
    }

    /// Insert or replace a media identity (identification is recomputed
    /// only when the file's size/mtime changed — the caller checks the
    /// evidence payload for freshness before rehashing).
    pub fn upsert_media_identity(
        &self,
        item_id: &ItemId,
        media_kind: &str,
        title: Option<&str>,
        platform: Option<&str>,
        sha1: Option<&str>,
        evidence_json: &str,
    ) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO media_identities
                 (item_id, media_kind, title, year, tmdb_id, platform, sha1, match_evidence_json)
                 VALUES (?1, ?2, ?3, NULL, NULL, ?4, ?5, ?6)",
                params![
                    item_id.to_string(),
                    media_kind,
                    title,
                    platform,
                    sha1,
                    evidence_json
                ],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// Insert or replace a FILM/TV identity (Media Resolution): the
    /// TMDB-matched (or user-confirmed) truth about one video file.
    /// `media_kind` is "movie" | "episode". Distinct from the ROM
    /// upsert above, which never touches these columns.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_video_identity(
        &self,
        item_id: &ItemId,
        media_kind: &str,
        title: Option<&str>,
        year: Option<i64>,
        tmdb_id: Option<i64>,
        season: Option<i64>,
        episode: Option<i64>,
        episode_end: Option<i64>,
        confidence: Option<f64>,
        confirmed: bool,
        evidence_json: &str,
    ) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO media_identities
                 (item_id, media_kind, title, year, tmdb_id, platform, sha1,
                  match_evidence_json, season, episode, episode_end, confidence, confirmed)
                 VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    item_id.to_string(),
                    media_kind,
                    title,
                    year,
                    tmdb_id,
                    evidence_json,
                    season,
                    episode,
                    episode_end,
                    confidence,
                    if confirmed { 1_i64 } else { 0 },
                ],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// Resolve a catalog item by its provider_key (for local files,
    /// the absolute path — the identity that survives re-indexing).
    pub fn item_by_provider_key(
        &self,
        provider_key: &str,
    ) -> Result<Option<ufm_vfs::Item>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, location_id, name, kind, size_bytes, modified, provider_key
                 FROM items WHERE provider_key = ?1 LIMIT 1",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut rows = stmt
            .query_map(params![provider_key], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        match rows.next() {
            Some(r) => {
                let (id, location_id, name, kind_s, size_bytes, modified_s, pk) =
                    r.map_err(|e| UfmError::Database(e.to_string()))?;
                let parse_id = |s: &str, what: &str| {
                    uuid::Uuid::parse_str(s)
                        .map_err(|e| UfmError::Database(format!("bad {what} id {s:?}: {e}")))
                };
                Ok(Some(ufm_vfs::Item {
                    id: ufm_core::ItemId(parse_id(&id, "item")?),
                    location: ufm_core::LocationId(parse_id(&location_id, "location")?),
                    name,
                    kind: parse_kind(&kind_s),
                    size_bytes: size_bytes.map(|s| s as u64),
                    modified: modified_s.and_then(|s| {
                        chrono::DateTime::parse_from_rfc3339(&s)
                            .ok()
                            .map(|dt| dt.with_timezone(&chrono::Utc))
                    }),
                    provider_key: pk,
                    labels: Vec::new(),
                    starred: false,
                }))
            }
            None => Ok(None),
        }
    }

    /// ROM collection membership: items with a stored DAT identification
    /// for `platform` — either as the primary platform or in the
    /// evidence's `also` list (MAME sets belong to Arcade AND their home
    /// system). `location_ids` restricts scope (None = every location).
    /// Returns (item, DAT display name) ordered by DAT name.
    pub fn rom_collection_items(
        &self,
        platform: &str,
        location_ids: Option<&[String]>,
    ) -> Result<Vec<(ufm_vfs::Item, String)>> {
        let mut sql = String::from(
            "SELECT i.id, i.location_id, i.name, i.kind, i.size_bytes,
                    i.modified, i.provider_key, mi.title
             FROM media_identities AS mi
             JOIN items AS i ON i.id = mi.item_id
             WHERE mi.media_kind = 'rom' AND mi.title IS NOT NULL
               AND (mi.platform = ?1
                    OR EXISTS (SELECT 1 FROM json_each(mi.match_evidence_json, '$.also')
                               WHERE value = ?1))",
        );
        if let Some(ids) = location_ids {
            if ids.is_empty() {
                return Ok(Vec::new());
            }
            let placeholders: Vec<String> =
                ids.iter().enumerate().map(|(i, _)| format!("?{}", i + 2)).collect();
            sql.push_str(&format!(" AND i.location_id IN ({})", placeholders.join(",")));
        }
        sql.push_str(" ORDER BY mi.title COLLATE NOCASE");
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut values: Vec<rusqlite::types::Value> =
            vec![rusqlite::types::Value::Text(platform.to_string())];
        if let Some(ids) = location_ids {
            values.extend(ids.iter().map(|s| rusqlite::types::Value::Text(s.clone())));
        }
        let rows = stmt
            .query_map(rusqlite::params_from_iter(values.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, String>(7)?,
                ))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, location_id, name, kind_s, size_bytes, modified_s, provider_key, title) =
                row.map_err(|e| UfmError::Database(e.to_string()))?;
            let parse_id = |s: &str, what: &str| {
                uuid::Uuid::parse_str(s)
                    .map_err(|e| UfmError::Database(format!("bad {what} id {s:?}: {e}")))
            };
            let labels = match &provider_key {
                Some(pk) => self.labels_for_path(pk).unwrap_or_default(),
                None => Vec::new(),
            };
            out.push((
                ufm_vfs::Item {
                    id: ufm_core::ItemId(parse_id(&id, "item")?),
                    location: ufm_core::LocationId(parse_id(&location_id, "location")?),
                    name,
                    kind: parse_kind(&kind_s),
                    size_bytes: size_bytes.map(|s| s as u64),
                    modified: modified_s.and_then(|s| {
                        chrono::DateTime::parse_from_rfc3339(&s)
                            .ok()
                            .map(|dt| dt.with_timezone(&chrono::Utc))
                    }),
                    provider_key,
                    labels,
                    starred: false,
                },
                title,
            ));
        }
        Ok(out)
    }

    /// Collection Completeness (2026-10-17): every owned
    /// (system, DAT title) pair, membership-expanded exactly like
    /// the shelf scan — primary platform plus the evidence `also`
    /// list — ROM identities only (PC games have no DAT universe).
    /// The completeness engine folds these into owned title groups
    /// with the shelf's own key, so the score can never contradict
    /// the shelves.
    pub fn rom_completeness_owned(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT platform, title, match_evidence_json
                 FROM media_identities
                 WHERE media_kind = 'rom' AND title IS NOT NULL AND platform IS NOT NULL",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            let (platform, title, evidence) =
                row.map_err(|e| UfmError::Database(e.to_string()))?;
            out.push((platform.clone(), title.clone()));
            let (also, _) = shelf_evidence(&evidence);
            for a in also {
                if a != platform {
                    out.push((a, title.clone()));
                }
            }
        }
        Ok(out)
    }

    /// Collection Completeness: file-extension census over the
    /// DAT-identified ROM items, per platform — the wanted-format
    /// suggestion source ("you collect SNES as ZIP" is read off his
    /// own shelves, never assumed). Returns (platform, extension
    /// with leading dot, lowercased, item count).
    pub fn rom_completeness_ext_census(&self) -> Result<Vec<(String, String, i64)>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT mi.platform, i.provider_key
                 FROM media_identities AS mi
                 JOIN items AS i ON i.id = mi.item_id
                 WHERE mi.media_kind = 'rom' AND mi.title IS NOT NULL
                   AND mi.platform IS NOT NULL AND i.provider_key IS NOT NULL",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut counts: std::collections::HashMap<(String, String), i64> =
            std::collections::HashMap::new();
        for row in rows {
            let (platform, pk) = row.map_err(|e| UfmError::Database(e.to_string()))?;
            let name = pk.rsplit('/').next().unwrap_or(&pk);
            if let Some(dot) = name.rfind('.') {
                if dot > 0 && dot + 1 < name.len() {
                    let ext = name[dot..].to_lowercase();
                    *counts.entry((platform, ext)).or_default() += 1;
                }
            }
        }
        let mut out: Vec<(String, String, i64)> =
            counts.into_iter().map(|((p, e), n)| (p, e, n)).collect();
        out.sort();
        Ok(out)
    }

    /// Platforms of identified ROMs for a set of items — the Files
    /// banner's truth source (identities first; extension guesses
    /// only as fallback). Returns (item_id, platform) for the items
    /// that HAVE a ROM identification; unidentified items are absent.
    pub fn rom_platforms_for_items(
        &self,
        item_ids: &[String],
    ) -> Result<Vec<(String, String)>> {
        let mut out = Vec::new();
        if item_ids.is_empty() {
            return Ok(out);
        }
        // Chunked IN lists keep statement counts low for big folders.
        for chunk in item_ids.chunks(400) {
            let placeholders: Vec<String> =
                (1..=chunk.len()).map(|i| format!("?{i}")).collect();
            let sql = format!(
                "SELECT item_id, platform FROM media_identities
                 WHERE media_kind = 'rom' AND platform IS NOT NULL
                   AND item_id IN ({})",
                placeholders.join(",")
            );
            let mut stmt = self
                .conn
                .prepare(&sql)
                .map_err(|e| UfmError::Database(e.to_string()))?;
            let values: Vec<rusqlite::types::Value> = chunk
                .iter()
                .map(|s| rusqlite::types::Value::Text(s.clone()))
                .collect();
            let rows = stmt
                .query_map(rusqlite::params_from_iter(values.iter()), |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| UfmError::Database(e.to_string()))?;
            for row in rows {
                out.push(row.map_err(|e| UfmError::Database(e.to_string()))?);
            }
        }
        Ok(out)
    }

    /// Unidentified ROM candidates for the ROM Home Needs-ID shelf:
    /// items named like a candidate extension that either have no ROM
    /// identity row at all or a title-less (matched:false) one.
    /// Extension filtering mirrors `rule_candidates`. Returns
    /// (item, has_identity_row). Membership scoping (which of these
    /// the shelf actually shows) lives in `rom_needs_id_scoped`
    /// below — it needs the path evidence of `rom_identified_folders`.
    pub fn rom_needs_id_candidates(
        &self,
        extensions: &[String],
    ) -> Result<Vec<(ufm_vfs::Item, bool)>> {
        if extensions.is_empty() {
            return Ok(Vec::new());
        }
        let likes: Vec<String> = extensions
            .iter()
            .map(|e| format!("%.{}", e.to_lowercase()))
            .collect();
        let like_clause = likes
            .iter()
            .enumerate()
            .map(|(i, _)| format!("lower(i.name) LIKE ?{}", i + 1))
            .collect::<Vec<_>>()
            .join(" OR ");
        let sql = format!(
            "SELECT i.id, i.location_id, i.name, i.kind, i.size_bytes,
                    i.modified, i.provider_key,
                    CASE WHEN mi.item_id IS NULL THEN 0 ELSE 1 END
             FROM items AS i
             LEFT JOIN media_identities AS mi
               ON mi.item_id = i.id AND mi.media_kind = 'rom'
             WHERE ({like_clause})
               AND (mi.item_id IS NULL OR mi.title IS NULL)
               AND i.kind = 'file'
             ORDER BY i.name COLLATE NOCASE"
        );
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let values: Vec<rusqlite::types::Value> = likes
            .iter()
            .map(|s| rusqlite::types::Value::Text(s.clone()))
            .collect();
        let rows = stmt
            .query_map(rusqlite::params_from_iter(values.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, location_id, name, kind_s, size_bytes, modified_s, provider_key, has_row) =
                row.map_err(|e| UfmError::Database(e.to_string()))?;
            let parse_id = |s: &str, what: &str| {
                uuid::Uuid::parse_str(s)
                    .map_err(|e| UfmError::Database(format!("bad {what} id {s:?}: {e}")))
            };
            let labels = match &provider_key {
                Some(pk) => self.labels_for_path(pk).unwrap_or_default(),
                None => Vec::new(),
            };
            out.push((
                ufm_vfs::Item {
                    id: ufm_core::ItemId(parse_id(&id, "item")?),
                    location: ufm_core::LocationId(parse_id(&location_id, "location")?),
                    name,
                    kind: parse_kind(&kind_s),
                    size_bytes: size_bytes.map(|s| s as u64),
                    modified: modified_s.and_then(|s| {
                        chrono::DateTime::parse_from_rfc3339(&s)
                            .ok()
                            .map(|dt| dt.with_timezone(&chrono::Utc))
                    }),
                    provider_key,
                    labels,
                    starred: false,
                },
                has_row != 0,
            ));
        }
        Ok(out)
    }

    /// Parent folders (provider_key directory parts) holding at least
    /// one identified ROM — the data-driven half of the Needs-ID
    /// shelf's ROM-context scope: an unmatched file sitting beside
    /// identified games is in ROM territory whatever its path says.
    pub fn rom_identified_folders(&self) -> Result<std::collections::HashSet<String>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT i.provider_key FROM items AS i
                 JOIN media_identities AS mi ON mi.item_id = i.id
                 WHERE mi.media_kind = 'rom' AND mi.title IS NOT NULL
                   AND i.provider_key IS NOT NULL",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut out = std::collections::HashSet::new();
        for row in rows {
            let pk = row.map_err(|e| UfmError::Database(e.to_string()))?;
            if let Some(pos) = pk.rfind('/') {
                out.insert(pk[..pos].to_string());
            }
        }
        Ok(out)
    }

    /// The Needs-ID shelf's membership rule (Bouncer, 2026-10-18),
    /// applied over `rom_needs_id_candidates`: a candidate that HAS
    /// a title-less ROM identity row was already judged ROM-kind by
    /// an identification pass — that judgment is its territory
    /// evidence, so it lists wherever it lives (the Klax audio
    /// evidence dumps sat in no honest surface at all: not ROM
    /// territory by path, yet dressed as games by the extension
    /// gather). A never-scanned candidate (no identity row) still
    /// needs ROM territory — a folder holding an identified game,
    /// or a path segment naming a collection — so the raw
    /// matched-nothing bucket (~75% non-ROM noise) stays shut.
    pub fn rom_needs_id_scoped(
        &self,
        extensions: &[String],
    ) -> Result<Vec<(ufm_vfs::Item, bool)>> {
        let rows = self.rom_needs_id_candidates(extensions)?;
        let identified_folders = self.rom_identified_folders()?;
        Ok(rows
            .into_iter()
            .filter(|(item, has_row)| {
                if *has_row {
                    return true;
                }
                match &item.provider_key {
                    Some(pk) => Self::rom_context_path(pk, &identified_folders),
                    None => false,
                }
            })
            .collect())
    }

    /// Is this path in ROM territory? Either its folder already
    /// holds an identified game (data-driven), or some path segment
    /// names a ROM collection outright. PC-game data files and
    /// documentation trees that merely share an extension with a
    /// ROM format never qualify on their own — they arrive here
    /// only when an identification pass judged them ROM-kind (the
    /// `has_row` door in `rom_needs_id_scoped`).
    fn rom_context_path(
        provider_key: &str,
        identified_folders: &std::collections::HashSet<String>,
    ) -> bool {
        if let Some((parent, _)) = provider_key.rsplit_once('/') {
            if identified_folders.contains(parent) {
                return true;
            }
        }
        provider_key.split('/').any(|seg| {
            matches!(
                seg.to_ascii_lowercase().as_str(),
                "roms" | "rom" | "games" | "game"
            )
        })
    }

    /// Reset the ROM collection: delete every ROM identity and every
    /// cached ROM media row, so the next identification pass starts
    /// from a blank slate (Dickles, 2026-10-16 — the on-ramp for a
    /// big new games drive). Personal state (`rom_game_state`:
    /// ratings, favorites, play history) is deliberately NOT touched:
    /// it is keyed by file path and re-attaches when items
    /// re-identify. Returns (identities removed, media rows removed).
    pub fn reset_rom_collection(&self) -> Result<(u64, u64)> {
        let identities = self
            .conn
            .execute(
                "DELETE FROM media_identities WHERE media_kind = 'rom'",
                [],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let media = self
            .conn
            .execute("DELETE FROM rom_media", [])
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok((identities as u64, media as u64))
    }

    /// Cache a probed media duration for a path (0.0 = probed, unreadable).
    pub fn set_duration(&self, provider_key: &str, secs: f64) -> Result<()> {
        self.conn
            .execute(
                "UPDATE items SET duration_secs = ?2 WHERE provider_key = ?1",
                params![provider_key, secs],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// tag_path that reports whether the assignment is new.
    pub fn tag_path_counted(&self, provider_key: &str, label: &str) -> Result<bool> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO path_labels (provider_key, label) VALUES (?1, ?2)",
                params![provider_key, label],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(self.conn.changes() > 0)
    }

    pub fn create_collection(&self, id: &CollectionId, name: &str, kind: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO collections (id, name, kind) VALUES (?1, ?2, ?3)",
                params![id.to_string(), name, kind],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// Built-in virtual collections from spec §12 — aggregate by type without
    /// moving a single file.
    pub fn ensure_builtin_collections(&self) -> Result<()> {
        for name in [
            "All PDFs",
            "All Photos",
            "All Documents",
            "All Films",
            "All Other Video",
            "All Ebooks",
        ] {
            self.conn
                .execute(
                    "INSERT OR IGNORE INTO collections (id, name, kind) VALUES (?1, ?2, 'smart')",
                    params![uuid::Uuid::new_v4().to_string(), name],
                )
                .map_err(|e| UfmError::Database(e.to_string()))?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Phase 2 — stars, manual collections, view profiles.
    //
    // All user metadata is keyed by provider_key (the absolute path for
    // local files), exactly like path_labels: the provider_key is the
    // identity that survives re-indexing (items keep stable ids per
    // (location_id, provider_key)), and read paths resolve keys back
    // to live items. Keys that no longer resolve are reported, never
    // silently dropped.
    // ------------------------------------------------------------------

    fn hydrate_item(
        &self,
        id: &str,
        location_id: &str,
        name: String,
        kind_s: &str,
        size_bytes: Option<i64>,
        modified_s: Option<String>,
        provider_key: Option<String>,
    ) -> Result<ufm_vfs::Item> {
        let parse_id = |s: &str, what: &str| {
            uuid::Uuid::parse_str(s)
                .map_err(|e| UfmError::Database(format!("bad {what} id {s:?}: {e}")))
        };
        let (labels, starred) = match &provider_key {
            Some(pk) => (
                self.labels_for_path(pk).unwrap_or_default(),
                self.is_starred_path(pk).unwrap_or(false),
            ),
            None => (Vec::new(), false),
        };
        Ok(ufm_vfs::Item {
            id: ufm_core::ItemId(parse_id(id, "item")?),
            location: ufm_core::LocationId(parse_id(location_id, "location")?),
            name,
            kind: parse_kind(kind_s),
            size_bytes: size_bytes.map(|v| v as u64),
            modified: modified_s.and_then(|m| {
                chrono::DateTime::parse_from_rfc3339(&m)
                    .ok()
                    .map(|d| d.with_timezone(&chrono::Utc))
            }),
            provider_key,
            labels,
            starred,
        })
    }

    /// Run `sql` selecting the standard item columns and hydrate the
    /// rows into Items (labels + starred populated).
    fn items_by_query(&self, sql: &str, param: &str) -> Result<Vec<ufm_vfs::Item>> {
        let mut stmt = self
            .conn
            .prepare(sql)
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![param], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut items = Vec::new();
        for row in rows {
            let (id, location_id, name, kind_s, size, modified, pk) =
                row.map_err(|e| UfmError::Database(e.to_string()))?;
            items.push(self.hydrate_item(&id, &location_id, name, &kind_s, size, modified, pk)?);
        }
        Ok(items)
    }

    // -- Stars -------------------------------------------------------

    pub fn set_star_path(&self, provider_key: &str, on: bool) -> Result<()> {
        if on {
            self.conn
                .execute(
                    "INSERT OR REPLACE INTO path_stars (provider_key, starred_at) VALUES (?1, ?2)",
                    params![provider_key, chrono::Utc::now().to_rfc3339()],
                )
                .map_err(|e| UfmError::Database(e.to_string()))?;
        } else {
            self.conn
                .execute(
                    "DELETE FROM path_stars WHERE provider_key = ?1",
                    params![provider_key],
                )
                .map_err(|e| UfmError::Database(e.to_string()))?;
        }
        Ok(())
    }

    /// Flip the star for a path; returns the NEW state.
    pub fn toggle_star_path(&self, provider_key: &str) -> Result<bool> {
        let now = !self.is_starred_path(provider_key)?;
        self.set_star_path(provider_key, now)?;
        Ok(now)
    }

    pub fn is_starred_path(&self, provider_key: &str) -> Result<bool> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM path_stars WHERE provider_key = ?1",
                params![provider_key],
                |r| r.get::<_, i64>(0),
            )
            .map(|c| c > 0)
            .map_err(|e| UfmError::Database(e.to_string()))
    }

    /// Which of `keys` are starred — one query for list hydration.
    pub fn starred_paths(
        &self,
        keys: &[String],
    ) -> Result<std::collections::HashSet<String>> {
        let mut out = std::collections::HashSet::new();
        if keys.is_empty() {
            return Ok(out);
        }
        let mut stmt = self
            .conn
            .prepare("SELECT provider_key FROM path_stars")
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let all = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let wanted: std::collections::HashSet<&str> =
            keys.iter().map(|k| k.as_str()).collect();
        for row in all {
            let pk = row.map_err(|e| UfmError::Database(e.to_string()))?;
            if wanted.contains(pk.as_str()) {
                out.insert(pk);
            }
        }
        Ok(out)
    }

    /// Every catalog item currently starred (the Starred view).
    pub fn starred_items(&self) -> Result<Vec<ufm_vfs::Item>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT i.id, i.location_id, i.name, i.kind, i.size_bytes,
                        i.modified, i.provider_key
                 FROM items AS i
                 JOIN path_stars AS ps ON ps.provider_key = i.provider_key
                 ORDER BY i.name COLLATE NOCASE",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut items = Vec::new();
        for row in rows {
            let (id, location_id, name, kind_s, size, modified, pk) =
                row.map_err(|e| UfmError::Database(e.to_string()))?;
            items.push(self.hydrate_item(&id, &location_id, name, &kind_s, size, modified, pk)?);
        }
        Ok(items)
    }

    // -- Collections -------------------------------------------------

    /// All collections with live membership counts (sidebar list).
    pub fn list_collections(&self) -> Result<Vec<CollectionRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT c.id, c.name, c.kind,
                        (SELECT COUNT(*) FROM collection_members m WHERE m.collection_id = c.id),
                        (SELECT COUNT(*) FROM collection_members m
                          WHERE m.collection_id = c.id
                            AND NOT EXISTS (SELECT 1 FROM items i WHERE i.provider_key = m.provider_key))
                 FROM collections c
                 ORDER BY c.name COLLATE NOCASE",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(CollectionRow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    kind: row.get(2)?,
                    member_count: row.get::<_, i64>(3)? as u64,
                    missing_count: row.get::<_, i64>(4)? as u64,
                })
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| UfmError::Database(e.to_string()))?);
        }
        Ok(out)
    }

    pub fn rename_collection(&self, id: &CollectionId, name: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE collections SET name = ?2 WHERE id = ?1",
                params![id.to_string(), name],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// Delete a collection; its members go with it (FK cascade),
    /// the files themselves are never touched.
    pub fn delete_collection(&self, id: &CollectionId) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM collections WHERE id = ?1",
                params![id.to_string()],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn add_collection_member(
        &self,
        id: &CollectionId,
        provider_key: &str,
    ) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO collection_members (collection_id, provider_key, added_at)
                 VALUES (?1, ?2, ?3)",
                params![
                    id.to_string(),
                    provider_key,
                    chrono::Utc::now().to_rfc3339()
                ],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn remove_collection_member(
        &self,
        id: &CollectionId,
        provider_key: &str,
    ) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM collection_members WHERE collection_id = ?1 AND provider_key = ?2",
                params![id.to_string(), provider_key],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// Member provider_keys of a collection, in add order.
    pub fn collection_member_keys(&self, id: &CollectionId) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT provider_key FROM collection_members
                 WHERE collection_id = ?1 ORDER BY added_at, provider_key",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![id.to_string()], |row| row.get::<_, String>(0))
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| UfmError::Database(e.to_string()))?);
        }
        Ok(out)
    }

    /// Resolve a collection's members to live items. Returns
    /// (resolved items in name order, member keys that no longer
    /// resolve — moved or deleted outside Fable's sight).
    pub fn collection_items(
        &self,
        id: &CollectionId,
    ) -> Result<(Vec<ufm_vfs::Item>, Vec<String>)> {
        let items = self.items_by_query(
            "SELECT i.id, i.location_id, i.name, i.kind, i.size_bytes,
                    i.modified, i.provider_key
             FROM items AS i
             JOIN collection_members AS m ON m.provider_key = i.provider_key
             WHERE m.collection_id = ?1
             ORDER BY i.name COLLATE NOCASE",
            &id.to_string(),
        )?;
        let keys = self.collection_member_keys(id)?;
        let resolved: std::collections::HashSet<&str> = items
            .iter()
            .filter_map(|i| i.provider_key.as_deref())
            .collect();
        let missing = keys
            .into_iter()
            .filter(|k| !resolved.contains(k.as_str()))
            .collect();
        Ok((items, missing))
    }

    // -- View profiles ------------------------------------------------
    //
    // Grand Pass steal 5: profiles are keyed by (location, folder).
    // dir_key is the folder's path inside the location; the empty
    // key is the location-level row — every pre-pass profile lives
    // there (the migration carried them over untouched) and acts as
    // the inheritance base for folders without their own row.
    // Resolution order: folder row → location row → nothing (the
    // app default decides).

    /// The exact row for (location, dir_key), if one was saved.
    pub fn view_profile(
        &self,
        location: &LocationId,
        dir_key: &str,
    ) -> Result<Option<ViewProfileRow>> {
        use rusqlite::OptionalExtension;
        self.conn
            .query_row(
                "SELECT view_mode, sort_json, show_extensions, group_by FROM view_profiles WHERE location_id = ?1 AND dir_key = ?2",
                params![location.to_string(), dir_key],
                |r| {
                    Ok(ViewProfileRow {
                        view_mode: r.get(0)?,
                        sort_json: r.get(1)?,
                        show_extensions: r.get::<_, i64>(2)? != 0,
                        group_by: r.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(|e| UfmError::Database(e.to_string()))
    }

    /// The profile a folder should wear: its own row when one
    /// exists, else the location-level row. The bool reports
    /// whether the winning row is the folder's own (the UI only
    /// offers "Reset this folder's view" when it is).
    pub fn view_profile_resolved(
        &self,
        location: &LocationId,
        dir_key: &str,
    ) -> Result<(Option<ViewProfileRow>, bool)> {
        if !dir_key.is_empty() {
            if let Some(row) = self.view_profile(location, dir_key)? {
                return Ok((Some(row), true));
            }
        }
        Ok((self.view_profile(location, "")?, false))
    }

    pub fn set_view_profile(
        &self,
        location: &LocationId,
        dir_key: &str,
        profile: &ViewProfileRow,
    ) -> Result<()> {
        // view_profiles.location_id is an FK to locations; profile keys
        // are runtime location ids that may not be catalog locations,
        // so ensure a minimal row (invisible to the UI: its sidebar
        // reads AppState, not this table).
        self.conn
            .execute(
                "INSERT OR IGNORE INTO locations (id, provider, display_name, capabilities_json)
                 VALUES (?1, 'view-profile', ?1, '{}')",
                params![location.to_string()],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        self.conn
            .execute(
                "INSERT OR REPLACE INTO view_profiles (location_id, dir_key, show_extensions, view_mode, sort_json, group_by)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    location.to_string(),
                    dir_key,
                    profile.show_extensions as i64,
                    profile.view_mode,
                    profile.sort_json,
                    profile.group_by
                ],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// Forget one row (the folder Reset). The location-level row is
    /// never deleted through here — only a folder's own override.
    pub fn delete_view_profile(&self, location: &LocationId, dir_key: &str) -> Result<()> {
        if dir_key.is_empty() {
            return Ok(());
        }
        self.conn
            .execute(
                "DELETE FROM view_profiles WHERE location_id = ?1 AND dir_key = ?2",
                params![location.to_string(), dir_key],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    // ----------------------------------------------------------
    // Home base (Grand Pass steal 1): recent files + frequent
    // folders, learned locally, forgotten on request. `now` is
    // unix seconds, stamped by the caller so tests stay
    // deterministic. Recents are capped at the newest 200 rows;
    // the shelves read far fewer.
    // ----------------------------------------------------------

    /// Note a file being opened or previewed. Re-opening refreshes
    /// the row's clock instead of duplicating it.
    pub fn record_recent(&self, row: &HomeRecentRow) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO home_recents
                     (path, name, kind, location_id, item_id, opened_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(path) DO UPDATE SET
                     name = excluded.name,
                     kind = excluded.kind,
                     location_id = excluded.location_id,
                     item_id = excluded.item_id,
                     opened_at = excluded.opened_at",
                params![
                    row.path,
                    row.name,
                    row.kind,
                    row.location_id,
                    row.item_id,
                    row.opened_at
                ],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        self.conn
            .execute(
                "DELETE FROM home_recents WHERE path NOT IN
                     (SELECT path FROM home_recents
                      ORDER BY opened_at DESC LIMIT 200)",
                [],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// The Recent shelf, newest first.
    pub fn home_recents(&self, limit: i64) -> Result<Vec<HomeRecentRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT path, name, kind, location_id, item_id, opened_at
                 FROM home_recents ORDER BY opened_at DESC LIMIT ?1",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![limit], |r| {
                Ok(HomeRecentRow {
                    path: r.get(0)?,
                    name: r.get(1)?,
                    kind: r.get(2)?,
                    location_id: r.get(3)?,
                    item_id: r.get(4)?,
                    opened_at: r.get(5)?,
                })
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| UfmError::Database(e.to_string()))?);
        }
        Ok(out)
    }

    /// Forget one recent ("Forget this").
    pub fn forget_recent(&self, path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM home_recents WHERE path = ?1", params![path])
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// Wipe the whole Recent shelf (the Clear button).
    pub fn clear_recents(&self) -> Result<()> {
        self.conn
            .execute("DELETE FROM home_recents", [])
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// Note a folder being visited: first visit inserts, later
    /// visits count up and refresh the clock.
    pub fn record_folder_visit(
        &self,
        path: &str,
        name: &str,
        location_id: &str,
        item_id: &str,
        now: i64,
    ) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO home_visits
                     (path, name, location_id, item_id, visits, last_at)
                 VALUES (?1, ?2, ?3, ?4, 1, ?5)
                 ON CONFLICT(path) DO UPDATE SET
                     name = excluded.name,
                     location_id = excluded.location_id,
                     item_id = excluded.item_id,
                     visits = home_visits.visits + 1,
                     last_at = excluded.last_at",
                params![path, name, location_id, item_id, now],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// The frequent shelf: most-visited first, recency breaks
    /// ties.
    pub fn home_frequent(&self, limit: i64) -> Result<Vec<HomeVisitRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT path, name, location_id, item_id, visits, last_at
                 FROM home_visits
                 ORDER BY visits DESC, last_at DESC LIMIT ?1",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![limit], |r| {
                Ok(HomeVisitRow {
                    path: r.get(0)?,
                    name: r.get(1)?,
                    location_id: r.get(2)?,
                    item_id: r.get(3)?,
                    visits: r.get(4)?,
                    last_at: r.get(5)?,
                })
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| UfmError::Database(e.to_string()))?);
        }
        Ok(out)
    }

    /// Forget one frequent folder ("Forget this").
    pub fn forget_frequent(&self, path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM home_visits WHERE path = ?1", params![path])
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// Wipe the whole frequent shelf (the Clear button).
    pub fn clear_frequent(&self) -> Result<()> {
        self.conn
            .execute("DELETE FROM home_visits", [])
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    // -- Path re-keying (in-app rename/move) --------------------------

    /// A file or folder was renamed/moved inside Fable: carry its
    /// stars and collection memberships to the new path. Prefix-aware:
    /// descendants of a renamed folder follow. (Path labels deliberately
    /// keep their existing behavior — this serves the Phase 2 stores.)
    ///
    /// The item row itself follows too, in place: same id, new
    /// provider_key (and name, for a rename). Everything keyed by item
    /// id — media identities, item labels — rides along untouched.
    /// Before this, a moved item kept its old path until the next
    /// re-walk deleted the row as unseen, and the media identity the
    /// user had confirmed died with it via ON DELETE CASCADE (the
    /// 2026-10-16 Border incident: Organize for media servers moved a
    /// confirmed film; its preview kept resolving the dead old path,
    /// then the reconcile destroyed the match).
    pub fn rekey_path(&self, old: &str, new: &str) -> Result<()> {
        if old == new {
            return Ok(());
        }
        let old_prefix = format!("{old}/");
        // substr() is 1-indexed on characters: the matched prefix
        // occupies chars 1..=prefix_len INCLUDING its trailing slash,
        // and the remainder starts AT that slash (char prefix_len),
        // so `new || remainder` keeps exactly one separator.
        let prefix_len = old_prefix.chars().count() as i64;
        for table in ["path_stars", "collection_members"] {
            self.conn
                .execute(
                    &format!("UPDATE {table} SET provider_key = ?2 WHERE provider_key = ?1"),
                    params![old, new],
                )
                .map_err(|e| UfmError::Database(e.to_string()))?;
            self.conn
                .execute(
                    &format!(
                        "UPDATE {table} SET provider_key = ?1 || substr(provider_key, ?2)
                         WHERE substr(provider_key, 1, ?3) = ?4"
                    ),
                    params![new, prefix_len, prefix_len, old_prefix],
                )
                .map_err(|e| UfmError::Database(e.to_string()))?;
        }
        self.rekey_item_row(old, new)?;
        // Descendants of a moved folder follow, exactly like the
        // stores above: provider_keys are re-prefixed, ids (and so
        // identities) stay put, names don't change so the FTS index
        // needs no work.
        self.conn
            .execute(
                "UPDATE items SET provider_key = ?1 || substr(provider_key, ?2)
                 WHERE substr(provider_key, 1, ?3) = ?4",
                params![new, prefix_len, prefix_len, old_prefix],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// Carry the item row at `old` to `new` (see rekey_path). If a
    /// row already exists at `new` — a re-walk minted it before the
    /// follow ran — the survivor keeps its id: hand it this row's
    /// media identity and item labels where it has none, then retire
    /// this row the way delete_unseen does (FTS entry first).
    fn rekey_item_row(&self, old: &str, new: &str) -> Result<()> {
        use rusqlite::OptionalExtension;
        let old_row: Option<(String, i64, String)> = self
            .conn
            .query_row(
                "SELECT id, rowid, name FROM items WHERE provider_key = ?1 LIMIT 1",
                params![old],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let Some((old_id, old_rowid, old_name)) = old_row else {
            return Ok(());
        };
        let survivor: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM items WHERE provider_key = ?1 LIMIT 1",
                params![new],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| UfmError::Database(e.to_string()))?;
        match survivor {
            Some(new_id) if new_id != old_id => {
                // A destination identity, if one exists, wins; the
                // moved one fills a gap, never overwrites.
                self.conn
                    .execute(
                        "INSERT OR IGNORE INTO media_identities
                         (item_id, media_kind, title, year, tmdb_id, platform,
                          sha1, match_evidence_json, season, episode,
                          episode_end, confidence, confirmed)
                         SELECT ?1, media_kind, title, year, tmdb_id, platform,
                                sha1, match_evidence_json, season, episode,
                                episode_end, confidence, confirmed
                         FROM media_identities WHERE item_id = ?2",
                        params![new_id, old_id],
                    )
                    .map_err(|e| UfmError::Database(e.to_string()))?;
                self.conn
                    .execute(
                        "INSERT OR IGNORE INTO labels (item_id, label)
                         SELECT ?1, label FROM labels WHERE item_id = ?2",
                        params![new_id, old_id],
                    )
                    .map_err(|e| UfmError::Database(e.to_string()))?;
                self.conn
                    .execute(
                        "INSERT INTO items_fts (items_fts, rowid, name) VALUES ('delete', ?1, ?2)",
                        params![old_rowid, old_name],
                    )
                    .map_err(|e| UfmError::Database(e.to_string()))?;
                self.conn
                    .execute("DELETE FROM items WHERE id = ?1", params![old_id])
                    .map_err(|e| UfmError::Database(e.to_string()))?;
            }
            _ => {
                let new_name = std::path::Path::new(new)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| new.to_string());
                self.conn
                    .execute(
                        "UPDATE items SET provider_key = ?2, name = ?3 WHERE id = ?1",
                        params![old_id, new, new_name],
                    )
                    .map_err(|e| UfmError::Database(e.to_string()))?;
                if new_name != old_name {
                    self.conn
                        .execute(
                            "INSERT INTO items_fts (items_fts, rowid, name) VALUES ('delete', ?1, ?2)",
                            params![old_rowid, old_name],
                        )
                        .map_err(|e| UfmError::Database(e.to_string()))?;
                    self.conn
                        .execute(
                            "INSERT OR REPLACE INTO items_fts (rowid, name) VALUES (?1, ?2)",
                            params![old_rowid, new_name],
                        )
                        .map_err(|e| UfmError::Database(e.to_string()))?;
                }
            }
        }
        Ok(())
    }

    /// Total catalogued items, all locations (Phase 3 fix:
    /// `integrity_check` first shipped counting via the prefix
    /// helper, whose SQL treats "/" as a literal two-character
    /// prefix and so reported 0 on a full catalog).
    pub fn item_count(&self) -> Result<u64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
            .map_err(|e| UfmError::Database(e.to_string()))
    }

    pub fn location_count(&self) -> Result<u64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM locations", [], |r| r.get(0))
            .map_err(|e| UfmError::Database(e.to_string()))
    }

    /// `PRAGMA integrity_check` — run on open in debug builds and on demand.
    pub fn integrity_check(&self) -> Result<String> {
        self.conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .map_err(|e| UfmError::Database(e.to_string()))
    }

    // ------------------------------------------------------------------
    // Jobs — snapshots + journal (spec §25). Step transitions land in
    // job_journal (migration 1); whole-Job snapshots in jobs (migration 3).
    // ------------------------------------------------------------------

    /// Insert or replace the whole-Job snapshot. The executor calls this
    /// on every transition; crash recovery loads from here.
    pub fn upsert_job(&self, job: &ufm_jobs::Job) -> Result<()> {
        let json =
            serde_json::to_string(job).map_err(|e| UfmError::Database(e.to_string()))?;
        self.conn
            .execute(
                "INSERT OR REPLACE INTO jobs (job_id, title, state, created, job_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    job.id.to_string(),
                    job.title,
                    state_name(&job.state),
                    job.created.to_rfc3339(),
                    json
                ],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn load_job(&self, id: &JobId) -> Result<Option<ufm_jobs::Job>> {
        use rusqlite::OptionalExtension;
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT job_json FROM jobs WHERE job_id = ?1",
                params![id.to_string()],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| UfmError::Database(e.to_string()))?;
        match json {
            None => Ok(None),
            Some(json) => Ok(Some(
                serde_json::from_str(&json).map_err(|e| UfmError::Database(e.to_string()))?,
            )),
        }
    }

    /// Jobs a previous run left behind: queued, running, or paused.
    /// Crash recovery re-registers these at startup.
    pub fn unfinished_jobs(&self) -> Result<Vec<ufm_jobs::Job>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT job_json FROM jobs
                 WHERE state IN ('queued', 'running', 'paused') ORDER BY created",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut jobs = Vec::new();
        for row in rows {
            let json = row.map_err(|e| UfmError::Database(e.to_string()))?;
            jobs.push(
                serde_json::from_str(&json).map_err(|e| UfmError::Database(e.to_string()))?,
            );
        }
        Ok(jobs)
    }

    /// Append one step transition to the durable journal.
    pub fn record_journal_entry(&self, entry: &ufm_jobs::JournalEntry) -> Result<()> {
        insert_journal_entry(&self.conn, entry)
    }

    /// All journal entries for one job, oldest first (seq order).
    pub fn journal_entries(&self, job: &JobId) -> Result<Vec<ufm_jobs::JournalEntry>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT step_id, from_state, to_state, at, note FROM job_journal
                 WHERE job_id = ?1 ORDER BY seq",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![job.to_string()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut entries = Vec::new();
        for row in rows {
            let (step, from_s, to_s, at_s, note) =
                row.map_err(|e| UfmError::Database(e.to_string()))?;
            let at = chrono::DateTime::parse_from_rfc3339(&at_s)
                .map(|d| d.with_timezone(&chrono::Utc))
                .map_err(|e| UfmError::Database(e.to_string()))?;
            entries.push(ufm_jobs::JournalEntry {
                job: *job,
                step,
                from: parse_state(&from_s)?,
                to: parse_state(&to_s)?,
                at,
                note,
            });
        }
        Ok(entries)
    }
    /// The most recent jobs of ANY state, newest first (Phase 3: MCP
    /// `list_jobs`). The GUI lists its in-memory handles; a headless
    /// reader's truthful source is this table — the executor
    /// checkpoints every job here, and terminal snapshots keep their
    /// tallies after hygiene slimming, so deserialized jobs report
    /// honest step counts either way.
    pub fn recent_jobs(&self, limit: u32) -> Result<Vec<ufm_jobs::Job>> {
        let mut stmt = self
            .conn
            .prepare("SELECT job_json FROM jobs ORDER BY created DESC LIMIT ?1")
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![limit as i64], |r| r.get::<_, String>(0))
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut jobs = Vec::new();
        for row in rows {
            let json = row.map_err(|e| UfmError::Database(e.to_string()))?;
            jobs.push(
                serde_json::from_str(&json).map_err(|e| UfmError::Database(e.to_string()))?,
            );
        }
        Ok(jobs)
    }

    // -----------------------------------------------------------------
    // MCP audit chain (Phase 3) — see docs/MCP_API.md item 5.
    // -----------------------------------------------------------------

    /// Append one MCP tool-call record. Returns the row's seq.
    /// `outcome` is the JSON outcome object (see `McpAuditRow`).
    pub fn append_mcp_audit(
        &self,
        client: &str,
        tool: &str,
        args_json: &str,
        outcome: &str,
        is_write: bool,
        duration_ms: i64,
    ) -> Result<i64> {
        self.conn
            .execute(
                "INSERT INTO mcp_audit (at, client, tool, args_json, outcome, is_write, duration_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    ufm_core::now_utc().to_rfc3339(),
                    client,
                    tool,
                    args_json,
                    outcome,
                    is_write as i64,
                    duration_ms
                ],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(self.conn.last_insert_rowid())
    }

    /// The newest `limit` audit rows, newest first — the read path
    /// behind `fable-files --mcp-audit` and the `mcp_audit_recent`
    /// Tauri command.
    pub fn recent_mcp_audit(&self, limit: u32) -> Result<Vec<McpAuditRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT seq, at, client, tool, args_json, outcome, is_write, duration_ms
                 FROM mcp_audit ORDER BY seq DESC LIMIT ?1",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![limit as i64], |r| {
                Ok(McpAuditRow {
                    seq: r.get(0)?,
                    at: r.get(1)?,
                    client: r.get(2)?,
                    tool: r.get(3)?,
                    args_json: r.get(4)?,
                    outcome: r.get(5)?,
                    is_write: r.get::<_, i64>(6)? != 0,
                    duration_ms: r.get(7)?,
                })
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| UfmError::Database(e.to_string()))?);
        }
        Ok(out)
    }

    /// Retention for the audit chain: keep only the newest `keep`
    /// rows. Returns how many were pruned. This is the ONLY delete
    /// the audit table ever sees — the chain is otherwise append-only.
    pub fn prune_mcp_audit(&self, keep: u64) -> Result<u64> {
        let n = self
            .conn
            .execute(
                "DELETE FROM mcp_audit
                 WHERE seq NOT IN (SELECT seq FROM mcp_audit ORDER BY seq DESC LIMIT ?1)",
                params![keep as i64],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(n as u64)
    }

    // -----------------------------------------------------------------
    // Media Completion runs — the scheduled pipeline's journal.
    // -----------------------------------------------------------------

    /// Insert (or replace) a completion run row. The runner writes
    /// the row at start (status "running") and again at finish with
    /// the full journal, so a crash mid-run still leaves evidence.
    pub fn upsert_mediacomplete_run(&self, row: &MediaCompleteRunRow) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO mediacomplete_runs (id, schedule_id, schedule_name, target, started, finished, was_late, status, summary_json, actions_json, pending_json, quota_json, undone_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 ON CONFLICT(id) DO UPDATE SET finished=excluded.finished, status=excluded.status, summary_json=excluded.summary_json, actions_json=excluded.actions_json, pending_json=excluded.pending_json, quota_json=excluded.quota_json, undone_at=excluded.undone_at",
                params![
                    row.id,
                    row.schedule_id,
                    row.schedule_name,
                    row.target,
                    row.started,
                    row.finished,
                    row.was_late as i64,
                    row.status,
                    row.summary_json,
                    row.actions_json,
                    row.pending_json,
                    row.quota_json,
                    row.undone_at
                ],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn mediacomplete_run(&self, id: &str) -> Result<Option<MediaCompleteRunRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, schedule_id, schedule_name, target, started, finished, was_late, status, summary_json, actions_json, pending_json, quota_json, undone_at
                 FROM mediacomplete_runs WHERE id = ?1",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![id], map_mc_run)
            .map_err(|e| UfmError::Database(e.to_string()))?;
        for row in rows {
            return Ok(Some(row.map_err(|e| UfmError::Database(e.to_string()))?));
        }
        Ok(None)
    }

    /// The newest `limit` runs, newest first — the read path behind
    /// the Preferences "Scheduled completion" run history.
    pub fn recent_mediacomplete_runs(
        &self,
        limit: u32,
    ) -> Result<Vec<MediaCompleteRunRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, schedule_id, schedule_name, target, started, finished, was_late, status, summary_json, actions_json, pending_json, quota_json, undone_at
                 FROM mediacomplete_runs ORDER BY started DESC LIMIT ?1",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![limit as i64], map_mc_run)
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| UfmError::Database(e.to_string()))?);
        }
        Ok(out)
    }

    /// Retention: keep only the newest `keep` runs.
    pub fn prune_mediacomplete_runs(&self, keep: u64) -> Result<u64> {
        let n = self
            .conn
            .execute(
                "DELETE FROM mediacomplete_runs
                 WHERE id NOT IN (SELECT id FROM mediacomplete_runs ORDER BY started DESC LIMIT ?1)",
                params![keep as i64],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(n as u64)
    }

    // -----------------------------------------------------------------
    // Music Completion runs — the enrichment pipeline's journal.
    // -----------------------------------------------------------------

    /// Insert (or replace) a music completion run row. The runner
    /// writes the row at start (status "running") and again at
    /// finish with the full journal, so a crash mid-run still
    /// leaves evidence.
    pub fn upsert_musiccomplete_run(&self, row: &MusicCompleteRunRow) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO musiccomplete_runs (id, schedule_id, schedule_name, target, started, finished, was_late, status, summary_json, actions_json, pending_json, undone_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                 ON CONFLICT(id) DO UPDATE SET finished=excluded.finished, status=excluded.status, summary_json=excluded.summary_json, actions_json=excluded.actions_json, pending_json=excluded.pending_json, undone_at=excluded.undone_at",
                params![
                    row.id,
                    row.schedule_id,
                    row.schedule_name,
                    row.target,
                    row.started,
                    row.finished,
                    row.was_late as i64,
                    row.status,
                    row.summary_json,
                    row.actions_json,
                    row.pending_json,
                    row.undone_at
                ],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn musiccomplete_run(&self, id: &str) -> Result<Option<MusicCompleteRunRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, schedule_id, schedule_name, target, started, finished, was_late, status, summary_json, actions_json, pending_json, undone_at
                 FROM musiccomplete_runs WHERE id = ?1",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![id], map_mu_run)
            .map_err(|e| UfmError::Database(e.to_string()))?;
        for row in rows {
            return Ok(Some(row.map_err(|e| UfmError::Database(e.to_string()))?));
        }
        Ok(None)
    }

    /// The newest `limit` runs, newest first — the read path behind
    /// the Preferences run history + morning checklist.
    pub fn recent_musiccomplete_runs(
        &self,
        limit: u32,
    ) -> Result<Vec<MusicCompleteRunRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, schedule_id, schedule_name, target, started, finished, was_late, status, summary_json, actions_json, pending_json, undone_at
                 FROM musiccomplete_runs ORDER BY started DESC LIMIT ?1",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![limit as i64], map_mu_run)
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| UfmError::Database(e.to_string()))?);
        }
        Ok(out)
    }

    /// Retention: keep only the newest `keep` runs.
    pub fn prune_musiccomplete_runs(&self, keep: u64) -> Result<u64> {
        let n = self
            .conn
            .execute(
                "DELETE FROM musiccomplete_runs
                 WHERE id NOT IN (SELECT id FROM musiccomplete_runs ORDER BY started DESC LIMIT ?1)",
                params![keep as i64],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(n as u64)
    }

    // ------------------------------------------------------------------
    // File Auditor: runs, flags, dismissals, rules, user words, hashes.
    // ------------------------------------------------------------------

    pub fn upsert_auditor_run(&self, row: &AuditorRunRow) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO auditor_runs (id, schedule_id, schedule_name, target, started, finished, was_late, status, summary_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(id) DO UPDATE SET finished=excluded.finished, status=excluded.status, summary_json=excluded.summary_json",
                params![
                    row.id,
                    row.schedule_id,
                    row.schedule_name,
                    row.target,
                    row.started,
                    row.finished,
                    row.was_late as i64,
                    row.status,
                    row.summary_json
                ],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn auditor_run(&self, id: &str) -> Result<Option<AuditorRunRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, schedule_id, schedule_name, target, started, finished, was_late, status, summary_json
                 FROM auditor_runs WHERE id = ?1",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![id], map_auditor_run)
            .map_err(|e| UfmError::Database(e.to_string()))?;
        for row in rows {
            return Ok(Some(row.map_err(|e| UfmError::Database(e.to_string()))?));
        }
        Ok(None)
    }

    pub fn recent_auditor_runs(&self, limit: u32) -> Result<Vec<AuditorRunRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, schedule_id, schedule_name, target, started, finished, was_late, status, summary_json
                 FROM auditor_runs ORDER BY started DESC LIMIT ?1",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![limit as i64], map_auditor_run)
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| UfmError::Database(e.to_string()))?);
        }
        Ok(out)
    }

    pub fn prune_auditor_runs(&self, keep: u64) -> Result<u64> {
        let n = self
            .conn
            .execute(
                "DELETE FROM auditor_runs
                 WHERE id NOT IN (SELECT id FROM auditor_runs ORDER BY started DESC LIMIT ?1)",
                params![keep as i64],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(n as u64)
    }

    /// Close out runs a dead process left behind (Red Pen,
    /// 2026-10-18): a row still marked "running" whose id is not
    /// in the caller's in-flight set belongs to a process that
    /// died mid-audit — mark it "interrupted" so no surface can
    /// present the corpse as live work forever. Returns the
    /// number of rows flipped.
    pub fn mark_stale_auditor_runs_interrupted(
        &self,
        except_ids: &[String],
    ) -> Result<usize> {
        let n = if except_ids.is_empty() {
            self.conn.execute(
                "UPDATE auditor_runs SET status = 'interrupted' WHERE status = 'running'",
                [],
            )
        } else {
            let marks = except_ids
                .iter()
                .map(|_| "?")
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "UPDATE auditor_runs SET status = 'interrupted'                  WHERE status = 'running' AND id NOT IN ({marks})"
            );
            self.conn
                .execute(&sql, rusqlite::params_from_iter(except_ids.iter()))
        }
        .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(n)
    }

    pub fn insert_auditor_flag(&self, row: &AuditorFlagRow) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO auditor_flags (id, run_id, path, family, lane, class_key, title, detail, proposal_json, state, created, updated)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    row.id,
                    row.run_id,
                    row.path,
                    row.family,
                    row.lane,
                    row.class_key,
                    row.title,
                    row.detail,
                    row.proposal_json,
                    row.state,
                    row.created,
                    row.updated
                ],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn auditor_flag(&self, id: &str) -> Result<Option<AuditorFlagRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, run_id, path, family, lane, class_key, title, detail, proposal_json, state, created, updated
                 FROM auditor_flags WHERE id = ?1",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![id], map_auditor_flag)
            .map_err(|e| UfmError::Database(e.to_string()))?;
        for row in rows {
            return Ok(Some(row.map_err(|e| UfmError::Database(e.to_string()))?));
        }
        Ok(None)
    }

    /// Flags by state (`None` = every state), newest first.
    pub fn auditor_flags(&self, state: Option<&str>, limit: u32) -> Result<Vec<AuditorFlagRow>> {
        const COLS: &str = "id, run_id, path, family, lane, class_key, title, detail, proposal_json, state, created, updated";
        let mut out = Vec::new();
        match state {
            Some(s) => {
                let mut stmt = self
                    .conn
                    .prepare(&format!(
                        "SELECT {COLS} FROM auditor_flags WHERE state = ?1 ORDER BY created DESC LIMIT ?2"
                    ))
                    .map_err(|e| UfmError::Database(e.to_string()))?;
                let rows = stmt
                    .query_map(params![s, limit as i64], map_auditor_flag)
                    .map_err(|e| UfmError::Database(e.to_string()))?;
                for row in rows {
                    out.push(row.map_err(|e| UfmError::Database(e.to_string()))?);
                }
            }
            None => {
                let mut stmt = self
                    .conn
                    .prepare(&format!(
                        "SELECT {COLS} FROM auditor_flags ORDER BY created DESC LIMIT ?1"
                    ))
                    .map_err(|e| UfmError::Database(e.to_string()))?;
                let rows = stmt
                    .query_map(params![limit as i64], map_auditor_flag)
                    .map_err(|e| UfmError::Database(e.to_string()))?;
                for row in rows {
                    out.push(row.map_err(|e| UfmError::Database(e.to_string()))?);
                }
            }
        }
        Ok(out)
    }

    pub fn auditor_open_flag_count(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM auditor_flags WHERE state = 'open'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(n as u64)
    }

    pub fn set_auditor_flag_state(&self, id: &str, state: &str, updated: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE auditor_flags SET state = ?2, updated = ?3 WHERE id = ?1",
                params![id, state, updated],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// Move a flag between lanes (question -> fact) without touching
    /// its state — the "fix" standing rule promotes future matches
    /// of a class Dickles already answered with Fix All Similar.
    pub fn set_auditor_flag_lane(&self, id: &str, lane: &str, updated: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE auditor_flags SET lane = ?2, updated = ?3 WHERE id = ?1",
                params![id, lane, updated],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// Clear findings (Auditor item 13, Dickles 2026-10-15):
    /// delete EVERY flag row, whatever its state — the findings
    /// list starts empty and a fresh audit rebuilds it. Dismissal
    /// memory, standing rules, taught words and run history all
    /// SURVIVE on purpose: this is list hygiene for test/retest,
    /// never a factory reset (the full reset lives in Preferences).
    /// Returns how many findings were cleared, for the journal.
    pub fn clear_auditor_flags(&self) -> Result<u64> {
        let n = self
            .conn
            .execute("DELETE FROM auditor_flags", [])
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(n as u64)
    }

    /// Open flags whose path is `target` itself or lives under it —
    /// the supersede candidate set when a fresh run covers a target.
    pub fn open_auditor_flags_under(&self, target: &str) -> Result<Vec<AuditorFlagRow>> {
        let prefix = format!("{}/", target.trim_end_matches('/'));
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, run_id, path, family, lane, class_key, title, detail, proposal_json, state, created, updated
                 FROM auditor_flags WHERE state = 'open' AND (path = ?1 OR path LIKE ?2 ESCAPE '\\')
                 ORDER BY created DESC",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let like = prefix.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_") + "%";
        let rows = stmt
            .query_map(params![target, like], map_auditor_flag)
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| UfmError::Database(e.to_string()))?);
        }
        Ok(out)
    }

    // Dismissals: the permanent per-(path, family) "this one is OK".

    pub fn add_auditor_dismissal(&self, path: &str, family: &str, created: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO auditor_dismissals (path, family, created) VALUES (?1, ?2, ?3)",
                params![path, family, created],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn auditor_is_dismissed(&self, path: &str, family: &str) -> Result<bool> {
        let n: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM auditor_dismissals WHERE path = ?1 AND family = ?2",
                params![path, family],
                |r| r.get(0),
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(n > 0)
    }

    /// All dismissals as (path, family, created), path order.
    pub fn auditor_dismissals(&self) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, family, created FROM auditor_dismissals ORDER BY path, family")
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| UfmError::Database(e.to_string()))?);
        }
        Ok(out)
    }

    pub fn delete_auditor_dismissal(&self, path: &str, family: &str) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM auditor_dismissals WHERE path = ?1 AND family = ?2",
                params![path, family],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    // Rules: class-level law from ask-lane answers.
    // decision: "accept" (this class is fine / apply the standing
    // verdict) or "suppress" (never flag this class again).

    pub fn set_auditor_rule(&self, class_key: &str, decision: &str, created: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO auditor_rules (class_key, decision, created) VALUES (?1, ?2, ?3)",
                params![class_key, decision, created],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn auditor_rule(&self, class_key: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT decision FROM auditor_rules WHERE class_key = ?1")
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![class_key], |r| r.get::<_, String>(0))
            .map_err(|e| UfmError::Database(e.to_string()))?;
        for row in rows {
            return Ok(Some(row.map_err(|e| UfmError::Database(e.to_string()))?));
        }
        Ok(None)
    }

    /// All rules as (class_key, decision, created), key order.
    pub fn auditor_rules(&self) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT class_key, decision, created FROM auditor_rules ORDER BY class_key")
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| UfmError::Database(e.to_string()))?);
        }
        Ok(out)
    }

    pub fn delete_auditor_rule(&self, class_key: &str) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM auditor_rules WHERE class_key = ?1",
                params![class_key],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    // User words: confirmed ground truth for the typo hunt.

    pub fn add_auditor_user_word(&self, word: &str, created: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO auditor_user_words (word, created) VALUES (?1, ?2)",
                params![word.to_lowercase(), created],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn auditor_user_words(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT word FROM auditor_user_words ORDER BY word")
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| UfmError::Database(e.to_string()))?);
        }
        Ok(out)
    }

    // Hash cache: one SHA-256 per (path, size, mtime) — a changed
    // file invalidates by key mismatch, never by deletion passes.

    pub fn auditor_hash(&self, path: &str, size: u64, mtime: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT sha256 FROM auditor_hashes WHERE path = ?1 AND size = ?2 AND mtime = ?3")
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![path, size as i64, mtime], |r| r.get::<_, String>(0))
            .map_err(|e| UfmError::Database(e.to_string()))?;
        for row in rows {
            return Ok(Some(row.map_err(|e| UfmError::Database(e.to_string()))?));
        }
        Ok(None)
    }

    pub fn set_auditor_hash(&self, path: &str, size: u64, mtime: &str, sha256: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO auditor_hashes (path, size, mtime, sha256) VALUES (?1, ?2, ?3, ?4)",
                params![path, size as i64, mtime, sha256],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// Duplicate candidates for MCP `dedupe_analyze`: groups of files
    /// (folders excluded, zero-byte excluded) sharing an identical
    /// byte size, biggest potential waste first. Report-only data —
    /// content is never hashed here, so a group is a *candidate* set,
    /// never proof of duplication.
    pub fn duplicate_candidates(&self, limit: u32) -> Result<Vec<DuplicateGroup>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT size_bytes, COUNT(*) AS n FROM items
                 WHERE kind = 'file' AND size_bytes IS NOT NULL AND size_bytes > 0
                 GROUP BY size_bytes HAVING n >= 2
                 ORDER BY size_bytes * (n - 1) DESC LIMIT ?1",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let groups = stmt
            .query_map(params![limit as i64], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut sizes: Vec<(i64, i64)> = Vec::new();
        for g in groups {
            sizes.push(g.map_err(|e| UfmError::Database(e.to_string()))?);
        }
        let mut out = Vec::new();
        for (size, n) in sizes {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT provider_key FROM items
                     WHERE kind = 'file' AND size_bytes = ?1 AND provider_key IS NOT NULL
                     LIMIT 5",
                )
                .map_err(|e| UfmError::Database(e.to_string()))?;
            let rows = stmt
                .query_map(params![size], |r| r.get::<_, String>(0))
                .map_err(|e| UfmError::Database(e.to_string()))?;
            let mut sample_paths = Vec::new();
            for row in rows {
                sample_paths.push(row.map_err(|e| UfmError::Database(e.to_string()))?);
            }
            let size = size as u64;
            let n = n as u64;
            out.push(DuplicateGroup {
                size_bytes: size,
                count: n,
                wasted_bytes: size * (n - 1),
                sample_paths,
            });
        }
        Ok(out)
    }

    /// One cached ROM-media result for (item, media type).
    pub fn rom_media_for(
        &self,
        item_id: &str,
        media_type: &str,
    ) -> Result<Option<RomMediaRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT item_id, media_type, status, source, local_path,
                        detail_json, checked_at
                 FROM rom_media WHERE item_id = ?1 AND media_type = ?2",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut rows = stmt
            .query_map(params![item_id, media_type], |row| {
                Ok(RomMediaRow {
                    item_id: row.get(0)?,
                    media_type: row.get(1)?,
                    status: row.get(2)?,
                    source: row.get(3)?,
                    local_path: row.get(4)?,
                    detail_json: row.get(5)?,
                    checked_at: row.get(6)?,
                })
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        match rows.next() {
            Some(r) => Ok(Some(r.map_err(|e| UfmError::Database(e.to_string()))?)),
            None => Ok(None),
        }
    }

    /// Record (or replace) one ROM-media lookup result.
    pub fn upsert_rom_media(
        &self,
        item_id: &str,
        media_type: &str,
        status: &str,
        source: Option<&str>,
        local_path: Option<&str>,
        detail_json: &str,
    ) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO rom_media
                 (item_id, media_type, status, source, local_path, detail_json, checked_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    item_id,
                    media_type,
                    status,
                    source,
                    local_path,
                    detail_json,
                    ufm_core::now_utc().to_rfc3339()
                ],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// Every cached ROM-media row for ROMs identified on `platform`
    /// (the missing-media report's raw material), with DAT titles.
    /// Returns (row, DAT title, item provider key).
    pub fn rom_media_for_system(
        &self,
        platform: &str,
    ) -> Result<Vec<(RomMediaRow, String, Option<String>)>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT rm.item_id, rm.media_type, rm.status, rm.source,
                        rm.local_path, rm.detail_json, rm.checked_at,
                        mi.title, i.provider_key
                 FROM rom_media AS rm
                 JOIN media_identities AS mi ON mi.item_id = rm.item_id
                 JOIN items AS i ON i.id = rm.item_id
                 WHERE mi.media_kind = 'rom' AND mi.platform = ?1
                 ORDER BY mi.title COLLATE NOCASE, rm.media_type",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![platform], |row| {
                Ok((
                    RomMediaRow {
                        item_id: row.get(0)?,
                        media_type: row.get(1)?,
                        status: row.get(2)?,
                        source: row.get(3)?,
                        local_path: row.get(4)?,
                        detail_json: row.get(5)?,
                        checked_at: row.get(6)?,
                    },
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                ))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            let (row, title, pk) = r.map_err(|e| UfmError::Database(e.to_string()))?;
            out.push((row, title.unwrap_or_default(), pk));
        }
        Ok(out)
    }

    /// Box-art paths for a set of items (art-forward collections):
    /// item id -> canonical local path, only for rows the cascade (or
    /// the user, by hand) actually landed.
    pub fn rom_media_art(
        &self,
        item_ids: &[String],
    ) -> Result<std::collections::HashMap<String, String>> {
        let mut out = std::collections::HashMap::new();
        if item_ids.is_empty() {
            return Ok(out);
        }
        let placeholders: Vec<String> =
            item_ids.iter().enumerate().map(|(i, _)| format!("?{}", i + 1)).collect();
        let sql = format!(
            "SELECT item_id, local_path FROM rom_media
             WHERE media_type = 'boxart' AND status = 'found'
               AND local_path IS NOT NULL AND item_id IN ({})",
            placeholders.join(",")
        );
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let values: Vec<rusqlite::types::Value> =
            item_ids.iter().map(|s| rusqlite::types::Value::Text(s.clone())).collect();
        let rows = stmt
            .query_map(rusqlite::params_from_iter(values.iter()), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        for r in rows {
            let (id, path) = r.map_err(|e| UfmError::Database(e.to_string()))?;
            out.insert(id, path);
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// EmulationStation import (Hyperbase #5, 2026-10-17): attach gamelist
// metadata to the catalog items Fable already indexed, by path.
//
// Precedence — the contract the whole feature stands on (pinned by
// the es_import tests in this crate):
//   P1  DAT identity is never modified. A media_identities row the
//       identify pass wrote (evidence matched, or any titled row
//       that isn't the import's own) keeps title / platform / sha1 /
//       evidence / year, untouched.
//   P2  A gamelist identity is written only onto: no row at all, a
//       row the import itself wrote before (evidence source
//       "gamelist" — refreshed), or an unmatched title-less row.
//       The evidence carries the item's current size+modified, so
//       the identify pass's freshness check treats the row as
//       fresh and leaves it; if the file's bytes later change,
//       identification re-runs and DAT gets first claim again.
//   P3  The user's title_override is never written by the import.
//   P4  Personal state merges, never replaces: rating / genre /
//       players fill only empty slots; favorite is set-only;
//       play_count and last_played take the max of two histories
//       (his ES launches and his Fable launches are one history).
//   P5  Found artwork is never displaced: a rom_media row already
//       'found' from any source wins; the import fills absent
//       slots and upgrades 'missing' rows, source "gamelist".
// The descriptive long tail (description, credits, scrape
// provenance, no-surface-yet art refs) lives in es_metadata —
// import-owned, refreshed wholesale on every import.
// ---------------------------------------------------------------------------

/// What the import decided about one item's identity row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EsIdentityOutcome {
    /// A DAT (or otherwise titled) identity already stood — untouched.
    DatKept,
    /// No identity row existed; the gamelist's was written.
    GamelistWritten,
    /// A previous gamelist identity was refreshed in place.
    GamelistRefreshed,
    /// An unmatched, title-less row gained the gamelist identity.
    FilledUnmatched,
    /// The gamelist entry carries no name — nothing honest to title it.
    NoName,
    /// The item's identity belongs to another medium — skipped.
    NotRom,
}

/// Counters for one system's import (the totals use the same shape).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EsCounters {
    pub entries: u64,
    pub folder_count: u64,
    pub no_path: u64,
    pub unresolvable: u64,
    pub duplicates: u64,
    pub matched_exact: u64,
    pub matched_casefold: u64,
    /// Matched a catalog item whose file is not on disk right now
    /// (an offline volume's Library Memory row) — imported anyway.
    pub matched_offline: u64,
    pub unmatched_not_indexed: u64,
    pub unmatched_missing_file: u64,
    pub dat_kept: u64,
    pub identity_written: u64,
    pub identity_refreshed: u64,
    pub identity_filled: u64,
    pub identity_no_name: u64,
    pub not_rom: u64,
    pub art_filled: u64,
    pub art_kept: u64,
    pub state_updates: u64,
}

impl EsCounters {
    /// Fold another counter set into this one (report totals).
    pub fn add(&mut self, o: &EsCounters) {
        self.entries += o.entries;
        self.folder_count += o.folder_count;
        self.no_path += o.no_path;
        self.unresolvable += o.unresolvable;
        self.duplicates += o.duplicates;
        self.matched_exact += o.matched_exact;
        self.matched_casefold += o.matched_casefold;
        self.matched_offline += o.matched_offline;
        self.unmatched_not_indexed += o.unmatched_not_indexed;
        self.unmatched_missing_file += o.unmatched_missing_file;
        self.dat_kept += o.dat_kept;
        self.identity_written += o.identity_written;
        self.identity_refreshed += o.identity_refreshed;
        self.identity_filled += o.identity_filled;
        self.identity_no_name += o.identity_no_name;
        self.not_rom += o.not_rom;
        self.art_filled += o.art_filled;
        self.art_kept += o.art_kept;
        self.state_updates += o.state_updates;
    }
}

/// One system's import outcome; `parse_error` set means the
/// gamelist itself could not be read or parsed and every counter
/// is zero — the failure is this system's alone.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EsSystemReport {
    pub es_system: String,
    pub platform: String,
    pub gamelist_path: String,
    pub parse_error: Option<String>,
    pub counts: EsCounters,
}

/// A whole import run: one report per system plus folded totals.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EsImportReport {
    /// Set when an import was asked for while one was already
    /// running — the running one is the truth; apart from this
    /// flag the report is empty.
    pub already_running: bool,
    /// Location roots examined for ES data.
    pub roots_searched: u64,
    /// Of those, roots where an ES layout was detected.
    pub roots_with_es: u64,
    pub systems: Vec<EsSystemReport>,
    pub totals: EsCounters,
}

/// The stored es_metadata row (read-back for surfaces + proof).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EsMetadataRow {
    pub provider_key: String,
    pub es_system: String,
    pub platform: String,
    pub gamelist_path: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub developer: Option<String>,
    pub publisher: Option<String>,
    pub genre: Option<String>,
    pub family: Option<String>,
    pub players: Option<String>,
    pub lang: Option<String>,
    pub region: Option<String>,
    pub release_year: Option<i64>,
    pub releasedate_raw: Option<String>,
    pub source_id: Option<String>,
    pub scrap_source: Option<String>,
    pub scrap_date: Option<String>,
    pub md5: Option<String>,
    pub crc32: Option<String>,
    pub hidden: bool,
    pub kidgame: bool,
    pub gametime_secs: Option<i64>,
    pub image_path: Option<String>,
    pub marquee_path: Option<String>,
    pub thumbnail_path: Option<String>,
    pub fanart_path: Option<String>,
    pub manual_path: Option<String>,
    pub imported_at: String,
}

/// Everything the apply step needs for one matched game: the
/// gamelist's facts (converted where a Fable home wants another
/// shape), the resolved artwork, and the item's freshness facts.
#[derive(Debug, Clone)]
pub struct EsApplyInput {
    pub item_id: ItemId,
    pub provider_key: String,
    pub es_system: String,
    pub platform: String,
    pub gamelist_path: String,
    pub name: Option<String>,
    pub desc: Option<String>,
    pub developer: Option<String>,
    pub publisher: Option<String>,
    pub genre: Option<String>,
    pub family: Option<String>,
    pub players: Option<String>,
    pub lang: Option<String>,
    pub region: Option<String>,
    pub release_year: Option<i64>,
    pub releasedate_raw: Option<String>,
    pub source_id: Option<String>,
    pub scrap_source: Option<String>,
    pub scrap_date: Option<String>,
    pub md5: Option<String>,
    pub crc32: Option<String>,
    pub hidden: bool,
    pub kidgame: bool,
    pub gametime_secs: Option<i64>,
    pub rating_stars: Option<i64>,
    pub favorite: bool,
    pub playcount: Option<i64>,
    pub last_played_rfc3339: Option<String>,
    /// Resolved absolute paths of artwork that EXISTS on disk.
    pub boxart_path: Option<String>,
    /// Which gamelist tag won the boxart slot (image|boxart|cover|mix|thumbnail).
    pub boxart_kind: Option<String>,
    pub trailer_path: Option<String>,
    pub screenshot_path: Option<String>,
    /// Resolved refs kept as provenance (existence not required).
    pub image_path: Option<String>,
    pub marquee_path: Option<String>,
    pub thumbnail_path: Option<String>,
    pub fanart_path: Option<String>,
    pub manual_path: Option<String>,
    /// The item's freshness facts, exactly as the identify pass
    /// computes them (evidence contract, P2).
    pub item_size: u64,
    pub item_modified_rfc3339: Option<String>,
}

/// What one apply did, in the report's vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EsApplyOutcome {
    pub identity: EsIdentityOutcome,
    pub art_filled: Vec<String>,
    pub art_kept: Vec<String>,
    pub state_fields: Vec<String>,
}

/// Parse an RFC 3339 string for instant comparison; unparseable
/// values sort as the epoch so a stored oddity never blocks a
/// genuinely newer import.
fn rfc3339_instant(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&chrono::Utc))
}

impl Catalog {
    /// Every catalog item whose provider_key lives under a
    /// directory (passed WITHOUT a trailing separator). The ES
    /// import loads one system's candidates this way and matches
    /// gamelist paths against them in memory.
    pub fn items_under_prefix(&self, dir: &str) -> Result<Vec<ufm_vfs::Item>> {
        let lower = format!("{dir}/");
        let upper = format!("{dir}0"); // '/' is 0x2F; '0' bounds every child.
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, location_id, name, kind, size_bytes, modified, provider_key
                 FROM items WHERE provider_key >= ?1 AND provider_key < ?2
                 ORDER BY provider_key",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![lower, upper], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let parse_id = |s: &str, what: &str| {
            uuid::Uuid::parse_str(s)
                .map_err(|e| UfmError::Database(format!("bad {what} id {s:?}: {e}")))
        };
        let mut out = Vec::new();
        for r in rows {
            let (id, location_id, name, kind_s, size_bytes, modified_s, pk) =
                r.map_err(|e| UfmError::Database(e.to_string()))?;
            out.push(ufm_vfs::Item {
                id: ufm_core::ItemId(parse_id(&id, "item")?),
                location: ufm_core::LocationId(parse_id(&location_id, "location")?),
                name,
                kind: parse_kind(&kind_s),
                size_bytes: size_bytes.map(|s| s as u64),
                modified: modified_s.and_then(|s| {
                    chrono::DateTime::parse_from_rfc3339(&s)
                        .ok()
                        .map(|dt| dt.with_timezone(&chrono::Utc))
                }),
                provider_key: pk,
                labels: Vec::new(),
                starred: false,
            });
        }
        Ok(out)
    }

    /// The es_metadata row for one path, when an import wrote one.
    pub fn es_metadata_for(&self, provider_key: &str) -> Result<Option<EsMetadataRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT provider_key, es_system, platform, gamelist_path, name,
                        description, developer, publisher, genre, family, players,
                        lang, region, release_year, releasedate_raw, source_id,
                        scrap_source, scrap_date, md5, crc32, hidden, kidgame,
                        gametime_secs, image_path, marquee_path, thumbnail_path,
                        fanart_path, manual_path, imported_at
                 FROM es_metadata WHERE provider_key = ?1",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut rows = stmt
            .query_map(params![provider_key], |row| {
                Ok(EsMetadataRow {
                    provider_key: row.get(0)?,
                    es_system: row.get(1)?,
                    platform: row.get(2)?,
                    gamelist_path: row.get(3)?,
                    name: row.get(4)?,
                    description: row.get(5)?,
                    developer: row.get(6)?,
                    publisher: row.get(7)?,
                    genre: row.get(8)?,
                    family: row.get(9)?,
                    players: row.get(10)?,
                    lang: row.get(11)?,
                    region: row.get(12)?,
                    release_year: row.get(13)?,
                    releasedate_raw: row.get(14)?,
                    source_id: row.get(15)?,
                    scrap_source: row.get(16)?,
                    scrap_date: row.get(17)?,
                    md5: row.get(18)?,
                    crc32: row.get(19)?,
                    hidden: row.get::<_, i64>(20)? != 0,
                    kidgame: row.get::<_, i64>(21)? != 0,
                    gametime_secs: row.get(22)?,
                    image_path: row.get(23)?,
                    marquee_path: row.get(24)?,
                    thumbnail_path: row.get(25)?,
                    fanart_path: row.get(26)?,
                    manual_path: row.get(27)?,
                    imported_at: row.get(28)?,
                })
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        match rows.next() {
            Some(r) => Ok(Some(r.map_err(|e| UfmError::Database(e.to_string()))?)),
            None => Ok(None),
        }
    }

    /// Write the gamelist's identity onto an item the precedence
    /// rules say the import may title (P2). Evidence mirrors the
    /// identify pass's shape, with source "gamelist" and the
    /// item's freshness facts, so the pass treats the row as
    /// fresh until the file itself changes.
    fn write_gamelist_identity(&self, input: &EsApplyInput) -> Result<()> {
        let ev = serde_json::json!({
            "matched": false,
            "source": "gamelist",
            "size": input.item_size,
            "modified": input.item_modified_rfc3339,
            "esSystem": input.es_system,
            "gamelist": input.gamelist_path,
            "also": [],
        });
        self.conn
            .execute(
                "INSERT OR REPLACE INTO media_identities
                 (item_id, media_kind, title, year, tmdb_id, platform, sha1, match_evidence_json)
                 VALUES (?1, 'rom', ?2, ?3, NULL, ?4, NULL, ?5)",
                params![
                    input.item_id.to_string(),
                    input.name,
                    input.release_year,
                    input.platform,
                    ev.to_string()
                ],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// Apply one matched gamelist entry to the catalog under the
    /// precedence contract at this section's head. Idempotent: a
    /// second identical apply changes no state (provenance's
    /// imported_at stamp aside) and reports no fills.
    pub fn es_apply(&self, input: &EsApplyInput) -> Result<EsApplyOutcome> {
        let now = ufm_core::now_utc().to_rfc3339();
        // -- Descriptive provenance: import-owned, refreshed wholesale.
        self.conn
            .execute(
                "INSERT INTO es_metadata
                 (provider_key, es_system, platform, gamelist_path, name, description,
                  developer, publisher, genre, family, players, lang, region,
                  release_year, releasedate_raw, source_id, scrap_source, scrap_date,
                  md5, crc32, hidden, kidgame, gametime_secs, image_path, marquee_path,
                  thumbnail_path, fanart_path, manual_path, imported_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                         ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26,
                         ?27, ?28, ?29)
                 ON CONFLICT(provider_key) DO UPDATE SET
                    es_system = excluded.es_system,
                    platform = excluded.platform,
                    gamelist_path = excluded.gamelist_path,
                    name = excluded.name,
                    description = excluded.description,
                    developer = excluded.developer,
                    publisher = excluded.publisher,
                    genre = excluded.genre,
                    family = excluded.family,
                    players = excluded.players,
                    lang = excluded.lang,
                    region = excluded.region,
                    release_year = excluded.release_year,
                    releasedate_raw = excluded.releasedate_raw,
                    source_id = excluded.source_id,
                    scrap_source = excluded.scrap_source,
                    scrap_date = excluded.scrap_date,
                    md5 = excluded.md5,
                    crc32 = excluded.crc32,
                    hidden = excluded.hidden,
                    kidgame = excluded.kidgame,
                    gametime_secs = excluded.gametime_secs,
                    image_path = excluded.image_path,
                    marquee_path = excluded.marquee_path,
                    thumbnail_path = excluded.thumbnail_path,
                    fanart_path = excluded.fanart_path,
                    manual_path = excluded.manual_path,
                    imported_at = excluded.imported_at",
                params![
                    input.provider_key,
                    input.es_system,
                    input.platform,
                    input.gamelist_path,
                    input.name,
                    input.desc,
                    input.developer,
                    input.publisher,
                    input.genre,
                    input.family,
                    input.players,
                    input.lang,
                    input.region,
                    input.release_year,
                    input.releasedate_raw,
                    input.source_id,
                    input.scrap_source,
                    input.scrap_date,
                    input.md5,
                    input.crc32,
                    input.hidden as i64,
                    input.kidgame as i64,
                    input.gametime_secs,
                    input.image_path,
                    input.marquee_path,
                    input.thumbnail_path,
                    input.fanart_path,
                    input.manual_path,
                    now
                ],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;

        // -- Identity (P1/P2). A name is required to title anything.
        let identity = match self.media_identity_for(&input.item_id)? {
            _ if input.name.is_none() => EsIdentityOutcome::NoName,
            None => {
                self.write_gamelist_identity(input)?;
                EsIdentityOutcome::GamelistWritten
            }
            Some(row) if row.media_kind != "rom" => EsIdentityOutcome::NotRom,
            Some(row) => {
                let ev: serde_json::Value =
                    serde_json::from_str(&row.evidence_json).unwrap_or(serde_json::Value::Null);
                let source = ev.get("source").and_then(|v| v.as_str()).unwrap_or("");
                let matched = ev
                    .get("matched")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if source == "gamelist" {
                    self.write_gamelist_identity(input)?;
                    EsIdentityOutcome::GamelistRefreshed
                } else if matched || row.title.is_some() {
                    EsIdentityOutcome::DatKept
                } else {
                    self.write_gamelist_identity(input)?;
                    EsIdentityOutcome::FilledUnmatched
                }
            }
        };

        // -- Artwork (P5): fill only; a found row from any source wins.
        let mut art_filled: Vec<String> = Vec::new();
        let mut art_kept: Vec<String> = Vec::new();
        for (media_type, path, kind) in [
            ("boxart", &input.boxart_path, &input.boxart_kind),
            ("trailer", &input.trailer_path, &Some("video".to_string())),
            ("screenshot", &input.screenshot_path, &Some("screenshot".to_string())),
        ] {
            let Some(path) = path else { continue };
            let existing: Option<String> = self
                .conn
                .query_row(
                    "SELECT status FROM rom_media WHERE item_id = ?1 AND media_type = ?2",
                    params![input.item_id.to_string(), media_type],
                    |r| r.get(0),
                )
                .ok();
            if existing.as_deref() == Some("found") {
                art_kept.push(media_type.to_string());
                continue;
            }
            let detail = serde_json::json!({
                "esKind": kind,
                "gamelist": input.gamelist_path,
            });
            self.conn
                .execute(
                    "INSERT OR REPLACE INTO rom_media
                     (item_id, media_type, status, source, local_path, detail_json, checked_at)
                     VALUES (?1, ?2, 'found', 'gamelist', ?3, ?4, ?5)",
                    params![input.item_id.to_string(), media_type, path, detail.to_string(), now],
                )
                .map_err(|e| UfmError::Database(e.to_string()))?;
            art_filled.push(media_type.to_string());
        }

        // -- Personal state (P4): merge, never replace.
        let mut state_fields: Vec<String> = Vec::new();
        let current = self.rom_game_state_for(&input.provider_key)?;
        let mut scoped_sets: Vec<(&str, rusqlite::types::Value)> = Vec::new();
        if let Some(stars) = input.rating_stars {
            if current.rating.is_none() {
                scoped_sets.push(("rating", rusqlite::types::Value::Integer(stars)));
            }
        }
        if input.favorite && !current.favorite {
            scoped_sets.push(("favorite", rusqlite::types::Value::Integer(1)));
        }
        if let Some(g) = input.genre.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            if current.genre.as_deref().unwrap_or("").trim().is_empty() {
                scoped_sets.push(("genre", rusqlite::types::Value::Text(g.to_string())));
            }
        }
        if let Some(p) = input.players.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            if current.players.as_deref().unwrap_or("").trim().is_empty() {
                scoped_sets.push(("players", rusqlite::types::Value::Text(p.to_string())));
            }
        }
        if !scoped_sets.is_empty() {
            let apply = scoped_sets
                .iter()
                .map(|(c, _)| format!("{c} = ?"))
                .collect::<Vec<_>>()
                .join(", ");
            let args: Vec<rusqlite::types::Value> =
                scoped_sets.iter().map(|(_, v)| v.clone()).collect();
            state_fields.extend(scoped_sets.iter().map(|(c, _)| c.to_string()));
            match self.rom_group_key_for_path(&input.provider_key)? {
                Some(gk) => self.rom_group_state_touch(&gk, &apply, &args)?,
                None => self.rom_game_state_touch(&input.provider_key, &apply, &args)?,
            }
        }
        // Play history lives on the path row; max-merge both fields.
        let path_row = self.rom_path_state_row(&input.provider_key)?;
        let cur_count = path_row.as_ref().map(|r| r.play_count).unwrap_or(0);
        let cur_last = path_row.as_ref().and_then(|r| r.last_played.clone());
        let mut hist_sets: Vec<(&str, rusqlite::types::Value)> = Vec::new();
        if let Some(pc) = input.playcount {
            if pc > cur_count {
                hist_sets.push(("play_count", rusqlite::types::Value::Integer(pc)));
            }
        }
        if let Some(lp) = &input.last_played_rfc3339 {
            let newer = match &cur_last {
                None => true,
                Some(cur) => match (rfc3339_instant(lp), rfc3339_instant(cur)) {
                    (Some(n), Some(c)) => n > c,
                    _ => lp.as_str() > cur.as_str(),
                },
            };
            if newer {
                hist_sets.push((
                    "last_played",
                    rusqlite::types::Value::Text(lp.clone()),
                ));
            }
        }
        if !hist_sets.is_empty() {
            let apply = hist_sets
                .iter()
                .map(|(c, _)| format!("{c} = ?"))
                .collect::<Vec<_>>()
                .join(", ");
            let args: Vec<rusqlite::types::Value> =
                hist_sets.iter().map(|(_, v)| v.clone()).collect();
            state_fields.extend(hist_sets.iter().map(|(c, _)| c.to_string()));
            self.rom_game_state_touch(&input.provider_key, &apply, &args)?;
        }

        Ok(EsApplyOutcome {
            identity,
            art_filled,
            art_kept,
            state_fields,
        })
    }

    /// Import one system's gamelist into the catalog: parse it,
    /// match its entries against the items indexed under the
    /// system folder BY PATH, and apply each match under the
    /// precedence contract. Filesystem contact is metadata only
    /// (existence checks) — no game file is ever opened.
    pub fn import_es_system(
        &self,
        sys: &ufm_media::esgamelist::EsSystemDir,
    ) -> Result<EsSystemReport> {
        use ufm_media::esgamelist::{fable_platform, parse_gamelist, resolve_es_ref};
        let mut rep = EsSystemReport {
            es_system: sys.es_name.clone(),
            platform: fable_platform(&sys.es_name),
            gamelist_path: sys.gamelist_path.to_string_lossy().into_owned(),
            parse_error: None,
            counts: EsCounters::default(),
        };
        let xml = match std::fs::read_to_string(&sys.gamelist_path) {
            Ok(x) => x,
            Err(e) => {
                rep.parse_error = Some(format!("couldn't read the gamelist: {e}"));
                return Ok(rep);
            }
        };
        let list = match parse_gamelist(&xml) {
            Ok(l) => l,
            Err(e) => {
                rep.parse_error = Some(e.to_string());
                return Ok(rep);
            }
        };
        rep.counts.entries = list.games.len() as u64;
        rep.counts.folder_count = list.folder_count as u64;

        let items = self.items_under_prefix(&sys.dir.to_string_lossy())?;
        let mut by_key: std::collections::HashMap<String, ufm_vfs::Item> =
            std::collections::HashMap::new();
        let mut by_fold: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for it in items {
            if let Some(pk) = &it.provider_key {
                by_fold
                    .entry(pk.to_lowercase())
                    .or_insert_with(|| pk.clone());
                by_key.insert(pk.clone(), it);
            }
        }

        let resolve_str = |raw: Option<&str>| -> Option<String> {
            raw.and_then(|r| resolve_es_ref(&sys.dir, r))
                .map(|p| p.to_string_lossy().into_owned())
        };
        let resolve_existing = |raw: Option<&str>| -> Option<String> {
            let p = resolve_es_ref(&sys.dir, raw?)?;
            if p.is_file() {
                Some(p.to_string_lossy().into_owned())
            } else {
                None
            }
        };

        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for game in &list.games {
            let Some(raw_path) = game.path.as_deref() else {
                rep.counts.no_path += 1;
                continue;
            };
            let Some(resolved) = resolve_es_ref(&sys.dir, raw_path) else {
                rep.counts.unresolvable += 1;
                continue;
            };
            let key = resolved.to_string_lossy().into_owned();
            if !seen.insert(key.clone()) {
                rep.counts.duplicates += 1;
                continue;
            }
            let on_disk = resolved.is_file();
            let (item, casefold) = match by_key.get(&key) {
                Some(it) => (it, false),
                None => match by_fold.get(&key.to_lowercase()).and_then(|k| by_key.get(k)) {
                    Some(it) => (it, true),
                    None => {
                        if on_disk {
                            rep.counts.unmatched_not_indexed += 1;
                        } else {
                            rep.counts.unmatched_missing_file += 1;
                        }
                        continue;
                    }
                },
            };
            if item.kind == ufm_vfs::ItemKind::Folder {
                // A gamelist path that resolves to a folder row is
                // not the game file's item; say so in the honest
                // bucket rather than titling a folder.
                rep.counts.unmatched_not_indexed += 1;
                continue;
            }
            if casefold {
                rep.counts.matched_casefold += 1;
            } else {
                rep.counts.matched_exact += 1;
            }
            if !on_disk {
                rep.counts.matched_offline += 1;
            }

            // Art slots: the boxart slot takes the first of the
            // cover-ish tags whose file actually exists.
            let mut boxart_path: Option<String> = None;
            let mut boxart_kind: Option<String> = None;
            for (tag, raw) in [
                ("image", game.image.as_deref()),
                ("boxart", game.boxart.as_deref()),
                ("cover", game.cover.as_deref()),
                ("mix", game.mix.as_deref()),
                ("thumbnail", game.thumbnail.as_deref()),
            ] {
                if let Some(p) = resolve_existing(raw) {
                    boxart_path = Some(p);
                    boxart_kind = Some(tag.to_string());
                    break;
                }
            }

            let input = EsApplyInput {
                item_id: item.id,
                provider_key: key.clone(),
                es_system: sys.es_name.clone(),
                platform: rep.platform.clone(),
                gamelist_path: rep.gamelist_path.clone(),
                name: game.name.clone(),
                desc: game.desc.clone(),
                developer: game.developer.clone(),
                publisher: game.publisher.clone(),
                genre: game.genre.clone(),
                family: game.family.clone(),
                players: game.players.clone(),
                lang: game.lang.clone(),
                region: game.region.clone(),
                release_year: game
                    .releasedate
                    .as_deref()
                    .and_then(ufm_media::esgamelist::es_release_year),
                releasedate_raw: game.releasedate.clone(),
                source_id: game.source_id.clone(),
                scrap_source: game.scrap_source.clone(),
                scrap_date: game.scrap_date.clone(),
                md5: game.md5.clone(),
                crc32: game.crc32.clone(),
                hidden: game.hidden,
                kidgame: game.kidgame,
                gametime_secs: game.gametime.map(|s| s as i64),
                rating_stars: game
                    .rating
                    .and_then(ufm_media::esgamelist::es_rating_to_stars),
                favorite: game.favorite,
                playcount: game.playcount.map(|c| c as i64),
                last_played_rfc3339: game
                    .lastplayed
                    .as_deref()
                    .and_then(ufm_media::esgamelist::es_datetime_to_rfc3339),
                boxart_path,
                boxart_kind,
                trailer_path: resolve_existing(game.video.as_deref()),
                screenshot_path: resolve_existing(game.screenshot.as_deref()),
                image_path: resolve_str(game.image.as_deref()),
                marquee_path: resolve_str(game.marquee.as_deref()),
                thumbnail_path: resolve_str(game.thumbnail.as_deref()),
                fanart_path: resolve_str(game.fanart.as_deref()),
                manual_path: resolve_str(game.manual.as_deref()),
                item_size: item.size_bytes.unwrap_or(0),
                item_modified_rfc3339: item.modified.map(|m| m.to_rfc3339()),
            };
            let outcome = self.es_apply(&input)?;
            match outcome.identity {
                EsIdentityOutcome::DatKept => rep.counts.dat_kept += 1,
                EsIdentityOutcome::GamelistWritten => rep.counts.identity_written += 1,
                EsIdentityOutcome::GamelistRefreshed => rep.counts.identity_refreshed += 1,
                EsIdentityOutcome::FilledUnmatched => rep.counts.identity_filled += 1,
                EsIdentityOutcome::NoName => rep.counts.identity_no_name += 1,
                EsIdentityOutcome::NotRom => rep.counts.not_rom += 1,
            }
            rep.counts.art_filled += outcome.art_filled.len() as u64;
            rep.counts.art_kept += outcome.art_kept.len() as u64;
            if !outcome.state_fields.is_empty() {
                rep.counts.state_updates += 1;
            }
        }
        Ok(rep)
    }
}

// ---------------------------------------------------------------------------
// Indexer v2 — Library Memory: volumes, manifests, purge, job hygiene.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// ROM Home (Dickles, 2026-10-15): per-game personal state + the shelf
// queries the room renders from. Everything here reads the catalog
// alone — no disk scans — so shelves paint instantly and offline
// volumes browse with Library Memory honesty.
// ---------------------------------------------------------------------------

/// Personal state for one game, keyed by provider_key (the file's
/// path) exactly like `path_stars` / `path_labels`, so it survives
/// re-indexing, moves within a volume's history, and item-id churn.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RomGameState {
    /// The user's star rating, 1..=5 (None = unrated).
    pub rating: Option<i64>,
    pub favorite: bool,
    pub completed: bool,
    pub play_count: i64,
    /// RFC 3339 — the last time Play was pressed for this game.
    pub last_played: Option<String>,
    /// The user's own title, which wins over the DAT title in display.
    pub title_override: Option<String>,
    /// User-supplied facts (the Edit sheet): no metadata source in the
    /// enrichment machinery carries these, so they are the user's
    /// words or absent — never invented.
    pub genre: Option<String>,
    pub players: Option<String>,
    pub notes: Option<String>,
}

/// Per-track listening state (Music Glow-Up Phase 2, crew Diva):
/// the audio mirror of RomGameState's play history, keyed by
/// provider_key so it survives re-indexing.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioPlayState {
    pub play_count: i64,
    /// RFC 3339 — the last time Play was pressed for this track.
    pub last_played_at: Option<String>,
    /// Where the listener left off (the resume offer), seconds.
    pub last_position_secs: f64,
    /// Listens that reached the track's natural end.
    pub completed_count: i64,
}

/// One row of the Recently Played rail.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentAudio {
    pub provider_key: String,
    pub state: AudioPlayState,
}

/// One game's media presence for one media type (ROM Home inspector
/// chips): whether the enrichment cascade (or the user, by hand)
/// landed a file, and where it lives.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RomGameMedia {
    pub media_type: String,
    pub found: bool,
    pub local_path: Option<String>,
    pub source: Option<String>,
}

/// The PC facts a Games Home card shows for a pc-game identity
/// (PC Games P1, 2026-10-17): where the record came from, which
/// library and storefronts claim it, whether its install was
/// verified on a mounted volume, and the launch badge — which is
/// 'not-checked' until an observed launch or Dickles' own mark
/// moves it (P2 owns the ladder; P1 never guesses).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PcGameCard {
    /// "playnite" | "folder-scan"
    pub source_kind: String,
    /// The library it came from ("Playnite" or "Folder finds").
    pub library_name: String,
    /// Storefront labels derived from the record's links
    /// (Steam / GOG / Epic / itch.io / …), display order.
    pub storefronts: Vec<String>,
    /// "present" — install dir + play exe verified on disk at
    /// import; "recorded" — the record exists but the install
    /// could not be verified here (another drive letter / the
    /// volume is not mounted). Never a guess either way.
    pub install_state: String,
    /// Raw badge: not-checked | boots | boots-with-notes |
    /// wont-boot | windows-only (the last four are P2 / manual).
    pub launch_badge: String,
    pub install_size: Option<i64>,
    pub version: Option<String>,
    pub exe_name: Option<String>,
    /// Set when the folder/exe naming contradicts the record's
    /// title (a vendor mislabel, e.g. Innocence pointing at the
    /// Requiem folder) — shown, never silently "fixed".
    pub mislabel_note: Option<String>,
}

/// One outbound link from an imported record (name + URL).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PcLink {
    pub name: String,
    pub url: String,
}

/// One PC install to write — the importer's unit of work (PC
/// Games P1). The item it lands on is the install FOLDER: shallow
/// (contents are never walked), keyed by its absolute path under
/// the given location, exactly like every Library Memory item.
#[derive(Debug, Clone)]
pub struct PcInstallRecord {
    pub location_id: String,
    /// Absolute install-folder path (the item's provider key).
    pub provider_key: String,
    pub folder_name: String,
    pub folder_modified: Option<chrono::DateTime<chrono::Utc>>,
    pub title: String,
    pub year: Option<i64>,
    /// Identity evidence JSON (source, library, ids, volume
    /// facts) — the §U foreign-writer contract, stored verbatim.
    pub evidence_json: String,
    /// "playnite" | "folder-scan"
    pub source_kind: String,
    pub library_name: String,
    pub playnite_id: Option<String>,
    pub game_guid: Option<String>,
    pub description: Option<String>,
    pub genres: Vec<String>,
    pub developers: Vec<String>,
    pub publishers: Vec<String>,
    pub series: Vec<String>,
    pub features: Vec<String>,
    pub age_ratings: Vec<String>,
    pub links: Vec<PcLink>,
    pub storefronts: Vec<String>,
    pub critic_score: Option<i64>,
    pub community_score: Option<i64>,
    pub install_size: Option<i64>,
    pub version: Option<String>,
    pub exe_name: Option<String>,
    pub exe_rel: Option<String>,
    pub release_raw: Option<String>,
    pub mislabel_note: Option<String>,
    /// The import verified the install on disk (folder + play exe).
    pub present: bool,
    pub imported_at: String,
}

/// What one upsert did, so the import report can bucket honestly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PcUpsertOutcome {
    pub item_id: String,
    /// A pc-game identity was created by this write.
    pub created: bool,
    /// A folder-scan identity was upgraded to a richer source.
    pub upgraded: bool,
    /// The write was refused: an equal-or-richer source (or a
    /// non-PC identity) already owns the folder.
    pub skipped_precedence: bool,
}

/// One install of a game (a shelf group's member), for the sheet.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PcInstallRow {
    pub provider_key: String,
    pub title: String,
    pub library_name: String,
    pub source_kind: String,
    pub present: bool,
    pub launch_badge: String,
    pub exe_name: Option<String>,
    pub install_size: Option<i64>,
}

/// The game sheet's PC sections: the full imported record plus
/// every install of the same shelf group.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PcGameSheet {
    pub provider_key: String,
    pub title: String,
    pub year: Option<i64>,
    pub source_kind: String,
    pub library_name: String,
    pub playnite_id: Option<String>,
    pub game_guid: Option<String>,
    pub description: Option<String>,
    pub genres: Vec<String>,
    pub developers: Vec<String>,
    pub publishers: Vec<String>,
    pub series: Vec<String>,
    pub features: Vec<String>,
    pub age_ratings: Vec<String>,
    pub links: Vec<PcLink>,
    pub storefronts: Vec<String>,
    pub critic_score: Option<i64>,
    pub community_score: Option<i64>,
    pub install_size: Option<i64>,
    pub version: Option<String>,
    pub exe_name: Option<String>,
    pub exe_rel: Option<String>,
    pub release_raw: Option<String>,
    pub mislabel_note: Option<String>,
    pub imported_at: String,
    pub present: bool,
    pub launch_badge: String,
    pub launch_note: Option<String>,
    pub checked_at: Option<String>,
    pub installs: Vec<PcInstallRow>,
}

/// One row of the ROM Home shelf queries: everything the room
/// renders for a game.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RomHomeGame {
    pub item: ufm_vfs::Item,
    /// The DAT-verified title (display title = state.title_override
    /// when set, else this).
    pub title: String,
    /// The identity's primary platform (dataset key).
    pub platform: String,
    /// Further platforms the identity cross-lists under (the DAT
    /// evidence's `also` sets — a MAME set that is also Neo Geo
    /// belongs on both shelves, like the collections before it).
    pub also_platforms: Vec<String>,
    pub year: Option<i64>,
    pub sha1: Option<String>,
    /// The content CRC from the identity evidence (hex) — zip
    /// identities carry zeroed sha1, so (title, crc) is the variant
    /// discriminator there (AUDIT §8).
    pub crc: Option<String>,
    /// Phase 3: the group this game folds into (platform +
    /// normalized base title) and its parsed variant descriptor.
    /// One group = one shelf card; the room collapses by this key.
    pub group_key: String,
    pub variant: ufm_media::romdat::RomVariant,
    /// Region parsed from the DAT title ("USA", "USA, Europe", …).
    pub region: Option<String>,
    pub media: Vec<RomGameMedia>,
    pub state: RomGameState,
    /// An open File Auditor flag names this game's path.
    pub auditor_flagged: bool,
    /// PC facts for pc-game identities (None for ROMs): the card
    /// renders its library / install-state / launch badges from
    /// this, and the sheet opens with the same key.
    #[serde(default)]
    pub pc: Option<PcGameCard>,
}

/// Catalog-only collection tallies for the room: total identified
/// games, how many lack box art, and the same pair per system.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RomHomeCounts {
    pub total: i64,
    pub missing_media: i64,
    /// (platform, games, missing box art) — membership counts a game
    /// on every shelf it belongs to (primary + `also` platforms);
    /// `total` / `missing_media` count each game once.
    pub per_system: Vec<(String, i64, i64)>,
    /// Hyperbase #4: group counts for the rail's Favorites and
    /// Recently Played shelves (same group-level definitions the
    /// shelf window query uses), so the rail never needs the full
    /// game set in memory to know its numbers.
    pub favorite_groups: i64,
    pub recent_groups: i64,
    /// PC Games P1: (library name, group count) for the PC
    /// section's library sub-shelves — a group claimed by two
    /// libraries counts under both, like shelf membership itself.
    #[serde(default)]
    pub pc_libraries: Vec<(String, i64)>,
}

/// The lean row every ROM Home query shares: identity + item
/// facts, before enrichment (Hyperbase #4 refactor — the full
/// fetch and the shelf window enrich through the same code).
type RomHomeRawRow = (
    String,
    String,
    String,
    String,
    Option<i64>,
    Option<String>,
    Option<String>,
    String,
    String,
    Option<i64>,
    Option<String>,
    String,
);

/// Found-media bits in the shelf scan (boxart, screenshot,
/// titleshot, trailer) — one map read by the window query, the
/// tallies and the pair builder so they can never disagree.
const ROM_MEDIA_BIT: [(&str, u8); 4] = [
    ("boxart", 1),
    ("screenshot", 2),
    ("titleshot", 4),
    ("trailer", 8),
];

/// The room's filter vocabulary, mirrored 1:1 from the UI's
/// RoomFilters so the window spec sends what the room holds.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RomHomeShelfFilters {
    pub systems: Vec<String>,
    pub genres: Vec<String>,
    pub players: Vec<String>,
    pub media: Vec<String>,
    pub flags: Vec<String>,
    /// Smart List rules judge with their system facet ignored
    /// (the room's standing rule); the UI sets this for lists.
    #[serde(default)]
    pub ignore_systems: bool,
}

/// One shelf-window request (Hyperbase #4). `kind` names the
/// shelf: all | favorites | recent | missing | system | search |
/// smart | list. For smart shelves, `filters` IS the list's rule;
/// for the rest it is the room's toolbar filter set, applied on
/// top of shelf membership — always over the full set, in Rust.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RomHomeShelfSpec {
    pub kind: String,
    /// System shelf: the dataset key of the platform.
    #[serde(default)]
    pub platform: Option<String>,
    /// Search shelf: the query text.
    #[serde(default)]
    pub query: Option<String>,
    /// Platform keys whose DISPLAY name matches the search text —
    /// resolved by the UI, which owns the system-name table, so
    /// "nintendo" still finds the NES shelf's games.
    #[serde(default)]
    pub search_platforms: Vec<String>,
    /// List shelf: the list items' provider keys / item ids.
    #[serde(default)]
    pub member_keys: Option<Vec<String>>,
    /// PC section sub-shelf: keep only groups claimed by this
    /// library (any member — the same ANY rule as also-platforms).
    #[serde(default)]
    pub pc_source: Option<String>,
    #[serde(default)]
    pub filters: RomHomeShelfFilters,
    /// title | year | recent-played | most-played | recent-added
    pub sort: String,
    /// Keyset cursor from the previous window (opaque; minted
    /// only by this query). Never an offset: rows written by a
    /// concurrent identification pass cannot shift the chain.
    #[serde(default)]
    pub cursor: Option<String>,
    pub limit: u32,
}

/// One bounded window of a shelf (Hyperbase #4).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RomHomeShelfPage {
    /// Groups matching the spec across the FULL set — the honest
    /// number the shelf header shows, never the window size.
    pub total: i64,
    /// Member rows of this window's groups (whole groups only —
    /// a group never splits across windows), in window order.
    pub games: Vec<RomHomeGame>,
    /// Cursor for the next window, when more groups remain.
    pub next_cursor: Option<String>,
}

/// One missing-media fetch pair (item, media type) for the
/// room's fetch actions, judged over the full set in Rust.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RomHomeMissingPair {
    pub item_id: String,
    pub media_type: String,
}

/// Lean per-file row for the per-system tallies the command
/// layer finishes with session volume badges.
#[derive(Debug, Clone)]
pub struct RomHomeSystemFileRow {
    pub item_id: String,
    pub location_id: String,
    pub provider_key: Option<String>,
    pub platform: String,
    pub title: String,
    pub size_bytes: Option<i64>,
}

/// Per-path personal facts the group scan folds (play history
/// lives per path; scoped fields fall back here only when the
/// group has no state row of its own).
#[derive(Debug, Clone, Default)]
struct ShelfPathFacts {
    rating: Option<i64>,
    favorite: bool,
    completed: bool,
    play_count: i64,
    last_played: Option<String>,
}

/// One member file inside a group aggregate (lean — titles and
/// identity facts only; enrichment happens per window).
#[derive(Debug, Clone)]
struct ShelfMemberFacts {
    item_id: String,
    provider_key: Option<String>,
    title: String,
    sha1: Option<String>,
    crc: Option<String>,
    year: Option<i64>,
    modified: Option<String>,
}

/// Everything the shelf queries need to know about one group,
/// folded from one lean scan (Hyperbase #4).
#[derive(Debug, Clone, Default)]
struct ShelfGroupFacts {
    group_key: String,
    platform: String,
    members: Vec<ShelfMemberFacts>,
    /// Cross-listed platforms, union over members (self removed).
    also: Vec<String>,
    /// PC Games P1: libraries claiming this group (union over
    /// members' imported sources), and the imported genres union —
    /// pc-game groups carry their facets in pc_metadata, not in
    /// rom_group_state, so the scan folds them in from there.
    pc_sources: Vec<String>,
    pc_genres: Vec<String>,
    has_boxart: bool,
    has_trailer: bool,
    group_state: Option<RomGameState>,
    any_path_favorite: bool,
    any_path_completed: bool,
    max_path_rating: Option<i64>,
    play_total: i64,
    play_last: Option<String>,
    any_starred: bool,
    display_title: String,
    sort_title: String,
    /// The DEFAULT version's year / newest member stamp (group
    /// sort keys — see shelf_default_version_span).
    def_year: Option<i64>,
    def_modified: Option<String>,
}

/// The scan's full yield: group facts + the found-media map +
/// (search only) labels by path.
struct ShelfScan {
    groups: Vec<ShelfGroupFacts>,
    media_found: std::collections::HashMap<String, u8>,
    labels_by_path: std::collections::HashMap<String, Vec<String>>,
}

/// The total order key of one group under a sort — the cursor's
/// payload and the comparator's whole input, so a window chain
/// compares exactly what it sorted by.
#[derive(Debug, Clone)]
struct ShelfSortKey {
    sort_title: String,
    def_year: Option<i64>,
    play_last: Option<String>,
    play_total: i64,
    def_modified: Option<String>,
    group_key: String,
}

/// The opaque cursor string's contents (JSON). Carries the sort
/// it was minted under; a cursor from another sort is refused
/// (the chain restarts rather than mixing orders).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ShelfCursor {
    sort: String,
    sort_title: String,
    def_year: Option<i64>,
    play_last: Option<String>,
    play_total: i64,
    def_modified: Option<String>,
    group_key: String,
}


/// The two shelf facts an identity's evidence JSON carries —
/// `also` (other systems claiming the game) and `crc` (the DAT
/// checksum) — extracted WITHOUT a full JSON parse on the hot
/// path: a small scanner handles the app-owned evidence shape,
/// and ANY anomaly defers to the real parser, which also
/// defines the semantics (string-form crc reads as none;
/// a non-string `also` element voids the array).
fn shelf_evidence(evidence: &str) -> (Vec<String>, Option<String>) {
    match shelf_evidence_fast(evidence) {
        Some(v) => v,
        None => {
            let ev = serde_json::from_str::<serde_json::Value>(evidence).ok();
            let also: Vec<String> = ev
                .as_ref()
                .and_then(|v| v.get("also"))
                .and_then(|a| serde_json::from_value(a.clone()).ok())
                .unwrap_or_default();
            let crc: Option<String> = ev
                .as_ref()
                .and_then(|v| v.get("crc"))
                .and_then(|c| c.as_u64())
                .map(|n| format!("{n:08x}"));
            (also, crc)
        }
    }
}

fn shelf_evidence_fast(evidence: &str) -> Option<(Vec<String>, Option<String>)> {
    let mut also: Vec<String> = Vec::new();
    if let Some(pos) = evidence.find("\"also\"") {
        let rest = evidence[pos + 6..].trim_start();
        let rest = rest.strip_prefix(':')?.trim_start();
        let mut s = rest.strip_prefix('[')?;
        loop {
            s = s.trim_start();
            if let Some(r) = s.strip_prefix(']') {
                s = r;
                break;
            }
            if let Some(r) = s.strip_prefix(',') {
                s = r;
                continue;
            }
            let r = s.strip_prefix('"')?;
            let mut val = String::new();
            let mut end: Option<usize> = None;
            let mut chars = r.char_indices();
            while let Some((i, c)) = chars.next() {
                match c {
                    '"' => {
                        end = Some(i);
                        break;
                    }
                    '\\' => {
                        let (_, e) = chars.next()?;
                        match e {
                            '"' => val.push('"'),
                            '\\' => val.push('\\'),
                            '/' => val.push('/'),
                            'n' => val.push('\n'),
                            't' => val.push('\t'),
                            'r' => val.push('\r'),
                            _ => return None,
                        }
                    }
                    _ => val.push(c),
                }
            }
            let end = end?;
            also.push(val);
            s = &r[end + 1..];
        }
        let _ = s;
    }
    let mut crc: Option<String> = None;
    if let Some(pos) = evidence.find("\"crc\"") {
        let rest = evidence[pos + 5..].trim_start();
        let rest = rest.strip_prefix(':')?.trim_start();
        if rest.starts_with('"') {
            crc = None;
        } else {
            let digits: String =
                rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                return None;
            }
            match rest[digits.len()..].chars().next() {
                Some(',') | Some('}') | Some(' ') | Some(']') | None => {
                    crc = Some(format!("{:08x}", digits.parse::<u64>().ok()?));
                }
                _ => return None,
            }
        }
    }
    Some((also, crc))
}

fn shelf_sort_key_of(g: &ShelfGroupFacts) -> ShelfSortKey {
    ShelfSortKey {
        sort_title: g.sort_title.clone(),
        def_year: g.def_year,
        play_last: g.play_last.clone(),
        play_total: g.play_total,
        def_modified: g.def_modified.clone(),
        group_key: g.group_key.clone(),
    }
}

/// The room's sort orders, at group level (Hyperbase #4):
/// title = display title; year = the DEFAULT version's year,
/// newest first, yearless last; recent-played = the group's
/// newest play; most-played = the group's total plays;
/// recent-added = the default version's newest file stamp.
/// Ties fall to title, then group key — a total order, which is
/// what makes keyset windows exact.
fn shelf_key_cmp(a: &ShelfSortKey, b: &ShelfSortKey, sort: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let primary = match sort {
        "year" => b.def_year.unwrap_or(-1).cmp(&a.def_year.unwrap_or(-1)),
        "recent-played" => b.play_last.cmp(&a.play_last),
        "most-played" => b.play_total.cmp(&a.play_total),
        "recent-added" => b.def_modified.cmp(&a.def_modified),
        _ => Ordering::Equal,
    };
    primary
        .then_with(|| a.sort_title.cmp(&b.sort_title))
        .then_with(|| a.group_key.cmp(&b.group_key))
}

/// Effective favorite: the group row rules when it exists (the
/// compose rule); else any member path's mark (the backfill
/// invariant makes the two agree wherever state was written
/// through the app's own doors).
fn shelf_group_favorite(g: &ShelfGroupFacts) -> bool {
    g.group_state
        .as_ref()
        .map(|s| s.favorite)
        .unwrap_or(g.any_path_favorite)
}

fn shelf_group_completed(g: &ShelfGroupFacts) -> bool {
    g.group_state
        .as_ref()
        .map(|s| s.completed)
        .unwrap_or(g.any_path_completed)
}

fn shelf_group_rating(g: &ShelfGroupFacts) -> Option<i64> {
    match &g.group_state {
        Some(s) => s.rating,
        None => g.max_path_rating,
    }
}

/// The players bucket of a user-supplied players string ("2",
/// "1-4", "2+"): the bucket names the most players it seats.
/// Mirrors the UI's playersBucket exactly.
fn players_bucket(players: &str) -> Option<&'static str> {
    let mut max: Option<u32> = None;
    let mut cur = String::new();
    for ch in players.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_digit() {
            cur.push(ch);
        } else if !cur.is_empty() {
            if let Ok(n) = cur.parse::<u32>() {
                max = Some(match max {
                    Some(m) => m.max(n),
                    None => n,
                });
            }
            cur.clear();
        }
    }
    match max? {
        0 | 1 => Some("1"),
        2 => Some("2"),
        3 | 4 => Some("3-4"),
        _ => Some("5+"),
    }
}

/// The group's DEFAULT version span (year + newest member
/// stamp), where "default" is the room's own rule ported: rank
/// versions by region, status, newest revision, descriptor;
/// never-default variants (bad dumps, trainers, hacks) yield
/// while any clean version exists. A version = one distinct
/// (descriptor, content) pair — content is the real sha1, else
/// the evidence CRC, else the DAT title (AUDIT §8).
fn shelf_default_version_span(
    g: &ShelfGroupFacts,
) -> (Option<i64>, Option<String>) {
    use std::collections::HashMap;
    // Fast path (the common case by far): one member is one
    // version, and it is the default — its year/mtime ARE the
    // span. Skipping the version fold here is what keeps a
    // 100k-group scan cheap; the full path below is exact for
    // the multi-dump minority and the two agree by construction
    // (a single version's span is its only member's).
    if g.members.len() == 1 {
        let m = &g.members[0];
        return (m.year, m.modified.clone());
    }
    let mut versions: HashMap<
        String,
        (ufm_media::romdat::RomVariant, Option<i64>, Option<String>),
    > = HashMap::new();
    for m in &g.members {
        let variant = ufm_media::romdat::variant_of_title(&m.title);
        let content = match &m.sha1 {
            Some(s) if s.chars().any(|c| c != '0') => s.clone(),
            _ => match &m.crc {
                Some(c) => c.clone(),
                None => m.title.clone(),
            },
        };
        let key = format!("{}|{}", variant.descriptor, content);
        let e = versions
            .entry(key)
            .or_insert_with(|| (variant, None, None));
        e.1 = match (e.1, m.year) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        if m.modified.as_deref() > e.2.as_deref() {
            e.2 = m.modified.clone();
        }
    }
    let mut vs: Vec<(
        &String,
        &(ufm_media::romdat::RomVariant, Option<i64>, Option<String>),
    )> = versions.iter().collect();
    vs.sort_by(|a, b| {
        (a.1 .0.region_rank, a.1 .0.status_rank, std::cmp::Reverse(a.1 .0.revision_key))
            .cmp(&(b.1 .0.region_rank, b.1 .0.status_rank, std::cmp::Reverse(b.1 .0.revision_key)))
            .then_with(|| a.1 .0.descriptor.cmp(&b.1 .0.descriptor))
            // Full ties (same descriptor, different content) break
            // on the version key — HashMap order must never leak
            // into a sort key two windows compare against.
            .then_with(|| a.0.cmp(b.0))
    });
    let def = vs.iter().find(|v| !v.1 .0.never_default).or_else(|| vs.first());
    match def {
        Some(v) => (v.1 .1, v.1 .2.clone()),
        None => (None, None),
    }
}

/// Search membership for one group (Hyperbase #4): any member's
/// DAT title, the group's display title or genre, any member's
/// labels, or the primary platform being one whose display name
/// matched (resolved by the UI into `search_platforms`).
fn shelf_group_matches_search(
    g: &ShelfGroupFacts,
    spec: &RomHomeShelfSpec,
    scan: &ShelfScan,
) -> bool {
    let q = spec
        .query
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_lowercase();
    if q.is_empty() {
        return true;
    }
    if spec.search_platforms.iter().any(|p| p == &g.platform) {
        return true;
    }
    if g.display_title.to_lowercase().contains(&q) {
        return true;
    }
    if let Some(genre) = g.group_state.as_ref().and_then(|s| s.genre.as_ref()) {
        if genre.to_lowercase().contains(&q) {
            return true;
        }
    }
    if g.pc_genres
        .iter()
        .any(|genre| genre.to_lowercase().contains(&q))
    {
        return true;
    }
    if g.pc_sources
        .iter()
        .any(|s| s.to_lowercase().contains(&q))
    {
        return true;
    }
    for m in &g.members {
        if m.title.to_lowercase().contains(&q) {
            return true;
        }
        if let Some(pk) = &m.provider_key {
            if let Some(labels) = scan.labels_by_path.get(pk) {
                if labels.iter().any(|l| l.to_lowercase().contains(&q)) {
                    return true;
                }
            }
        }
    }
    false
}

/// Does one group belong to the spec's shelf, under its filters?
/// Judged at GROUP level over the full set (the Ferret lesson:
/// never over a loaded window). Where the pre-window room judged
/// a collapsed REPRESENTATIVE and the readings differ, the group
/// reading is the pinned definition (see the receipt): systems /
/// starred judge ANY member; unplayed means no copy was played.
fn shelf_group_matches(
    g: &ShelfGroupFacts,
    spec: &RomHomeShelfSpec,
    scan: &ShelfScan,
) -> bool {
    match spec.kind.as_str() {
        "favorites" => {
            if !shelf_group_favorite(g) {
                return false;
            }
        }
        "recent" => {
            if g.play_last.is_none() {
                return false;
            }
        }
        "missing" => {
            if g.has_boxart {
                return false;
            }
        }
        "system" => {
            let p = spec.platform.as_deref().unwrap_or("");
            if g.platform != p && !g.also.iter().any(|a| a == p) {
                return false;
            }
        }
        "search" => {
            if !shelf_group_matches_search(g, spec, scan) {
                return false;
            }
        }
        "list" => {
            let keys: std::collections::HashSet<&str> = spec
                .member_keys
                .as_ref()
                .map(|v| v.iter().map(|s| s.as_str()).collect())
                .unwrap_or_default();
            let hit = g.members.iter().any(|m| {
                keys.contains(m.item_id.as_str())
                    || m.provider_key
                        .as_deref()
                        .map(|pk| keys.contains(pk))
                        .unwrap_or(false)
            });
            if !hit {
                return false;
            }
        }
        _ => {}
    }
    // PC library sub-shelf: ANY member's library claims the group
    // (the also-platforms rule), regardless of shelf kind.
    if let Some(source) = spec.pc_source.as_deref() {
        if !g.pc_sources.iter().any(|s| s == source) {
            return false;
        }
    }
    let f = &spec.filters;
    if !f.ignore_systems && !f.systems.is_empty() {
        let hit = f.systems.iter().any(|s| s == &g.platform)
            || g.also.iter().any(|a| f.systems.contains(a));
        if !hit {
            return false;
        }
    }
    if !f.genres.is_empty() {
        // ROM groups carry genre in group state; pc-game groups
        // carry the imported genre UNION — either satisfies.
        let state_hit = g
            .group_state
            .as_ref()
            .and_then(|s| s.genre.as_ref())
            .map(|genre| f.genres.iter().any(|x| x == genre))
            .unwrap_or(false);
        let pc_hit = g.pc_genres.iter().any(|x| f.genres.contains(x));
        if !state_hit && !pc_hit {
            return false;
        }
    }
    if !f.players.is_empty() {
        let bucket = g
            .group_state
            .as_ref()
            .and_then(|s| s.players.as_ref())
            .and_then(|p| players_bucket(p));
        match bucket {
            Some(b) if f.players.iter().any(|x| x == b) => {}
            _ => return false,
        }
    }
    for m in &f.media {
        match m.as_str() {
            "has-art" => {
                if !g.has_boxart {
                    return false;
                }
            }
            "missing-art" => {
                if g.has_boxart {
                    return false;
                }
            }
            "has-video" => {
                if !g.has_trailer {
                    return false;
                }
            }
            _ => {}
        }
    }
    for fl in &f.flags {
        match fl.as_str() {
            "starred" => {
                if !g.any_starred && shelf_group_rating(g).map(|r| r < 1).unwrap_or(true) {
                    return false;
                }
            }
            "favorites" => {
                if !shelf_group_favorite(g) {
                    return false;
                }
            }
            "completed" => {
                if !shelf_group_completed(g) {
                    return false;
                }
            }
            "unplayed" => {
                if g.play_total > 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

/// The region named by a DAT title's parenthesized region group —
/// "1943 - The Battle of Midway (USA)" → "USA", "(USA, Europe)" →
/// "USA, Europe". Groups naming anything else (Rev 1, En, Pirate…)
/// are not regions. Pure and testable.
pub fn region_of_title(title: &str) -> Option<String> {
    const REGIONS: &[&str] = &[
        "USA", "Europe", "Japan", "World", "UK", "France", "Germany", "Spain",
        "Italy", "Australia", "Canada", "Korea", "Brazil", "Asia", "Netherlands",
        "Sweden", "Denmark", "Finland", "Norway", "Russia", "China", "Taiwan",
        "Hong Kong", "Latin America", "Mexico", "Portugal", "Greece", "Turkey",
        "India", "New Zealand", "South Africa", "Argentina", "Chile", "Peru",
        "Colombia", "Venezuela", "Belgium", "Austria", "Switzerland", "Poland",
        "Czech Republic", "Hungary", "Romania", "Bulgaria", "Croatia", "Ireland",
        "Scandinavia", "Middle East", "Africa", "Oceania", "Global",
    ];
    let mut rest = title;
    while let Some(open) = rest.find('(') {
        let after = &rest[open + 1..];
        let Some(close) = after.find(')') else { break };
        let group = after[..close].trim();
        let parts: Vec<&str> = group.split(',').map(|p| p.trim()).collect();
        if !parts.is_empty()
            && parts.iter().all(|p| REGIONS.contains(p))
        {
            return Some(group.to_string());
        }
        rest = &after[close + 1..];
    }
    None
}

impl Catalog {
    /// Every DAT-identified game (title + platform known), or only
    /// one platform's shelf when `platform` is set — membership is
    /// the ROM Lists rule: the primary platform, or a cross-listed
    /// `also` platform. Each game arrives with its media presence,
    /// its personal state, and its Auditor flag state, all from the
    /// catalog. Ordered by platform, then title.
    pub fn rom_home_games(&self, platform: Option<&str>) -> Result<Vec<RomHomeGame>> {
        let raw = self.rom_home_raw_rows(platform)?;
        self.enrich_rom_home_rows(raw)
    }

    /// The lean identity+item rows behind every ROM Home query —
    /// one shape shared by the full fetch (above) and the shelf
    /// window's member enrichment (Hyperbase #4), so both read the
    /// same rows the same way.
    fn rom_home_raw_rows(&self, platform: Option<&str>) -> Result<Vec<RomHomeRawRow>> {
        let mut sql = String::from(
            "SELECT i.id, i.location_id, i.name, i.kind, i.size_bytes,
                    i.modified, i.provider_key, mi.title, mi.platform,
                    mi.year, mi.sha1, mi.match_evidence_json
             FROM media_identities AS mi
             JOIN items AS i ON i.id = mi.item_id
             WHERE mi.media_kind IN ('rom', 'pc-game')
               AND mi.title IS NOT NULL
               AND mi.platform IS NOT NULL",
        );
        if platform.is_some() {
            sql.push_str(
                " AND (mi.platform = ?1
                    OR EXISTS (SELECT 1 FROM json_each(mi.match_evidence_json, '$.also')
                               WHERE value = ?1))",
            );
        }
        sql.push_str(" ORDER BY mi.platform, mi.title COLLATE NOCASE");
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let raw: Vec<RomHomeRawRow> = match platform {
            Some(p) => stmt
                .query_map(params![p], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                        row.get(10)?,
                        row.get(11)?,
                    ))
                })
                .map_err(|e| UfmError::Database(e.to_string()))?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| UfmError::Database(e.to_string()))?,
            None => stmt
                .query_map([], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                        row.get(10)?,
                        row.get(11)?,
                    ))
                })
                .map_err(|e| UfmError::Database(e.to_string()))?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| UfmError::Database(e.to_string()))?,
        };
        drop(stmt);
        Ok(raw)
    }

    /// Enrich lean rows into shelf rows: media presence, personal
    /// state (group composed over path), the starred set and the
    /// open Auditor flags, all batched — never per-row queries
    /// beyond the label read each row already carried.
    fn enrich_rom_home_rows(&self, raw: Vec<RomHomeRawRow>) -> Result<Vec<RomHomeGame>> {
        // Batch the side data: media rows per item, personal state
        // per path, the starred set, the open Auditor flag paths.
        let item_ids: Vec<String> = raw.iter().map(|r| r.0.clone()).collect();
        let paths: Vec<String> = raw.iter().filter_map(|r| r.6.clone()).collect();
        let mut media_by_item: std::collections::HashMap<String, Vec<RomGameMedia>> =
            std::collections::HashMap::new();
        for chunk in item_ids.chunks(400) {
            let placeholders: Vec<String> =
                chunk.iter().enumerate().map(|(i, _)| format!("?{}", i + 1)).collect();
            let sql = format!(
                "SELECT item_id, media_type, status, source, local_path FROM rom_media
                 WHERE item_id IN ({})",
                placeholders.join(",")
            );
            let mut stmt = self
                .conn
                .prepare(&sql)
                .map_err(|e| UfmError::Database(e.to_string()))?;
            let values: Vec<rusqlite::types::Value> =
                chunk.iter().map(|s| rusqlite::types::Value::Text(s.clone())).collect();
            let rows = stmt
                .query_map(rusqlite::params_from_iter(values.iter()), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                })
                .map_err(|e| UfmError::Database(e.to_string()))?;
            for r in rows {
                let (item_id, media_type, status, source, local_path) =
                    r.map_err(|e| UfmError::Database(e.to_string()))?;
                media_by_item.entry(item_id).or_default().push(RomGameMedia {
                    media_type,
                    found: status == "found" && local_path.is_some(),
                    local_path,
                    source,
                });
            }
        }
        let mut state_by_path: std::collections::HashMap<String, RomGameState> =
            std::collections::HashMap::new();
        for chunk in paths.chunks(400) {
            let placeholders: Vec<String> =
                chunk.iter().enumerate().map(|(i, _)| format!("?{}", i + 1)).collect();
            let sql = format!(
                "SELECT provider_key, rating, favorite, completed, play_count,
                        last_played, title_override, genre, players, notes
                 FROM rom_game_state WHERE provider_key IN ({})",
                placeholders.join(",")
            );
            let mut stmt = self
                .conn
                .prepare(&sql)
                .map_err(|e| UfmError::Database(e.to_string()))?;
            let values: Vec<rusqlite::types::Value> =
                chunk.iter().map(|s| rusqlite::types::Value::Text(s.clone())).collect();
            let rows = stmt
                .query_map(rusqlite::params_from_iter(values.iter()), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        RomGameState {
                            rating: row.get(1)?,
                            favorite: row.get::<_, i64>(2)? != 0,
                            completed: row.get::<_, i64>(3)? != 0,
                            play_count: row.get(4)?,
                            last_played: row.get(5)?,
                            title_override: row.get(6)?,
                            genre: row.get(7)?,
                            players: row.get(8)?,
                            notes: row.get(9)?,
                        },
                    ))
                })
                .map_err(|e| UfmError::Database(e.to_string()))?;
            for r in rows {
                let (pk, st) = r.map_err(|e| UfmError::Database(e.to_string()))?;
                state_by_path.insert(pk, st);
            }
        }
        // Phase 3: group state for every group these games belong
        // to (scoped fields are the group's; play stays per-path).
        let group_keys: Vec<String> = raw
            .iter()
            .map(|r| ufm_media::romdat::rom_group_key(&r.8, &r.7))
            .collect();
        let mut state_by_group: std::collections::HashMap<String, RomGameState> =
            std::collections::HashMap::new();
        {
            let mut distinct: Vec<String> = group_keys.clone();
            distinct.sort();
            distinct.dedup();
            for chunk in distinct.chunks(400) {
                let placeholders: Vec<String> = chunk
                    .iter()
                    .enumerate()
                    .map(|(i, _)| format!("?{}", i + 1))
                    .collect();
                let sql = format!(
                    "SELECT group_key, rating, favorite, completed,
                            title_override, genre, players, notes
                     FROM rom_group_state WHERE group_key IN ({})",
                    placeholders.join(",")
                );
                let mut stmt = self
                    .conn
                    .prepare(&sql)
                    .map_err(|e| UfmError::Database(e.to_string()))?;
                let values: Vec<rusqlite::types::Value> = chunk
                    .iter()
                    .map(|s| rusqlite::types::Value::Text(s.clone()))
                    .collect();
                let rows = stmt
                    .query_map(rusqlite::params_from_iter(values.iter()), |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            RomGameState {
                                rating: row.get(1)?,
                                favorite: row.get::<_, i64>(2)? != 0,
                                completed: row.get::<_, i64>(3)? != 0,
                                play_count: 0,
                                last_played: None,
                                title_override: row.get(4)?,
                                genre: row.get(5)?,
                                players: row.get(6)?,
                                notes: row.get(7)?,
                            },
                        ))
                    })
                    .map_err(|e| UfmError::Database(e.to_string()))?;
                for r in rows {
                    let (gk, st) = r.map_err(|e| UfmError::Database(e.to_string()))?;
                    state_by_group.insert(gk, st);
                }
            }
        }
        // Labels batch too — the per-row labels_for_path call
        // this replaces was the enrich path's last N+1.
        let mut labels_by_path: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for chunk in paths.chunks(400) {
            let placeholders: Vec<String> =
                chunk.iter().enumerate().map(|(i, _)| format!("?{}", i + 1)).collect();
            let sql = format!(
                "SELECT provider_key, label FROM path_labels
                 WHERE provider_key IN ({})",
                placeholders.join(",")
            );
            let mut stmt = self
                .conn
                .prepare(&sql)
                .map_err(|e| UfmError::Database(e.to_string()))?;
            let values: Vec<rusqlite::types::Value> =
                chunk.iter().map(|s| rusqlite::types::Value::Text(s.clone())).collect();
            let rows = stmt
                .query_map(rusqlite::params_from_iter(values.iter()), |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| UfmError::Database(e.to_string()))?;
            for r in rows {
                let (pk, label) = r.map_err(|e| UfmError::Database(e.to_string()))?;
                labels_by_path.entry(pk).or_default().push(label);
            }
        }
        let starred = self.starred_paths(&paths).unwrap_or_default();
        let mut flagged: std::collections::HashSet<String> = std::collections::HashSet::new();
        for chunk in paths.chunks(400) {
            let placeholders: Vec<String> =
                chunk.iter().enumerate().map(|(i, _)| format!("?{}", i + 1)).collect();
            let sql = format!(
                "SELECT DISTINCT path FROM auditor_flags
                 WHERE state = 'open' AND path IN ({})",
                placeholders.join(",")
            );
            let mut stmt = self
                .conn
                .prepare(&sql)
                .map_err(|e| UfmError::Database(e.to_string()))?;
            let values: Vec<rusqlite::types::Value> =
                chunk.iter().map(|s| rusqlite::types::Value::Text(s.clone())).collect();
            let rows = stmt
                .query_map(rusqlite::params_from_iter(values.iter()), |row| {
                    row.get::<_, String>(0)
                })
                .map_err(|e| UfmError::Database(e.to_string()))?;
            for r in rows {
                flagged.insert(r.map_err(|e| UfmError::Database(e.to_string()))?);
            }
        }
        // PC cards for the window's pc-game rows (PC Games P1) —
        // batched like every other side read here.
        let mut pc_by_path: std::collections::HashMap<String, PcGameCard> =
            std::collections::HashMap::new();
        for chunk in paths.chunks(400) {
            let placeholders: Vec<String> =
                chunk.iter().enumerate().map(|(i, _)| format!("?{}", i + 1)).collect();
            let sql = format!(
                "SELECT pm.provider_key, pm.source_kind, pm.library_name,
                        pm.storefronts_json, pm.install_size, pm.version,
                        pm.exe_name, pm.mislabel_note,
                        COALESCE(pgs.present, 0),
                        COALESCE(pgs.launch_badge, 'not-checked')
                 FROM pc_metadata AS pm
                 LEFT JOIN pc_game_state AS pgs
                   ON pgs.provider_key = pm.provider_key
                 WHERE pm.provider_key IN ({})",
                placeholders.join(",")
            );
            let mut stmt = self
                .conn
                .prepare(&sql)
                .map_err(|e| UfmError::Database(e.to_string()))?;
            let values: Vec<rusqlite::types::Value> =
                chunk.iter().map(|s| rusqlite::types::Value::Text(s.clone())).collect();
            let rows = stmt
                .query_map(rusqlite::params_from_iter(values.iter()), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, i64>(8)?,
                        row.get::<_, String>(9)?,
                    ))
                })
                .map_err(|e| UfmError::Database(e.to_string()))?;
            for r in rows {
                let (pk, source_kind, library_name, fronts_json, size, version, exe, note, present, badge) =
                    r.map_err(|e| UfmError::Database(e.to_string()))?;
                pc_by_path.insert(
                    pk,
                    PcGameCard {
                        source_kind,
                        library_name,
                        storefronts: serde_json::from_str(&fronts_json)
                            .unwrap_or_default(),
                        install_state: if present != 0 {
                            "present".to_string()
                        } else {
                            "recorded".to_string()
                        },
                        launch_badge: badge,
                        install_size: size,
                        version,
                        exe_name: exe,
                        mislabel_note: note,
                    },
                );
            }
        }
        let parse_id = |s: &str, what: &str| {
            uuid::Uuid::parse_str(s)
                .map_err(|e| UfmError::Database(format!("bad {what} id {s:?}: {e}")))
        };
        let mut out = Vec::with_capacity(raw.len());
        for (
            id,
            location_id,
            name,
            kind_s,
            size_bytes,
            modified_s,
            provider_key,
            title,
            platform_s,
            year,
            sha1,
            evidence,
        ) in raw
        {
            let (also_platforms, crc) = shelf_evidence(&evidence);
            let labels = provider_key
                .as_ref()
                .and_then(|pk| labels_by_path.get(pk).cloned())
                .unwrap_or_default();
            let is_starred = provider_key
                .as_ref()
                .map(|pk| starred.contains(pk))
                .unwrap_or(false);
            let group_key = ufm_media::romdat::rom_group_key(&platform_s, &title);
            let variant = ufm_media::romdat::variant_of_title(&title);
            let state = Self::compose_rom_state(
                state_by_group.get(&group_key),
                provider_key
                    .as_ref()
                    .and_then(|pk| state_by_path.get(pk)),
            );
            let auditor_flagged = provider_key
                .as_ref()
                .map(|pk| flagged.contains(pk))
                .unwrap_or(false);
            out.push(RomHomeGame {
                item: ufm_vfs::Item {
                    id: ufm_core::ItemId(parse_id(&id, "item")?),
                    location: ufm_core::LocationId(parse_id(&location_id, "location")?),
                    name,
                    kind: parse_kind(&kind_s),
                    size_bytes: size_bytes.map(|s| s as u64),
                    modified: modified_s.and_then(|s| {
                        chrono::DateTime::parse_from_rfc3339(&s)
                            .ok()
                            .map(|dt| dt.with_timezone(&chrono::Utc))
                    }),
                    provider_key: provider_key.clone(),
                    labels,
                    starred: is_starred,
                },
                region: region_of_title(&title),
                title,
                platform: platform_s,
                also_platforms,
                year,
                sha1,
                crc,
                group_key,
                variant,
                media: media_by_item.get(&id).cloned().unwrap_or_default(),
                state,
                auditor_flagged,
                pc: provider_key
                    .as_ref()
                    .and_then(|pk| pc_by_path.get(pk).cloned()),
            });
        }
        Ok(out)
    }

    // --------------------------------------------------------
    // Hyperbase #4 (2026-10-17): windowed shelf queries. The old
    // room fetch shipped EVERY identified game to the webview and
    // let the client sort/filter/collapse the whole set — at
    // Hyperbase scale (100k+ games) that payload is the freeze.
    // Now Rust owns the set: one lean scan folds the identified
    // ROMs into per-GROUP facts (a group = one shelf card), the
    // spec's shelf + filters + sort are judged over the full set
    // here, and only a bounded window of whole groups is enriched
    // and shipped. Windows chain by keyset cursor, so a background
    // identification pass writing rows mid-browse can neither
    // duplicate nor skip groups in an open shelf.
    // --------------------------------------------------------

    /// One window request from the room. `kind` names the shelf
    /// (all | favorites | recent | missing | system | search |
    /// smart | list); filters/sort speak the room's own vocabulary
    /// (SortKey / RoomFilters field values) so the UI sends what it
    /// already holds.
    // (types live below the impl, beside RomHomeCounts)

    /// The group-facts scan shared by the window query, the room
    /// tallies and the missing-media pair builder: they must never
    /// disagree about what a group is or whether it qualifies.
    fn rom_home_scan(&self, need_labels: bool) -> Result<ShelfScan> {
        self.rom_home_scan_mode(need_labels, false)
    }

    /// `light`: skip per-member storage and the display/span
    /// pass — the tallies read group aggregates only, and at
    /// 100k+ groups the member structs are the scan's ballast.
    fn rom_home_scan_mode(&self, need_labels: bool, light: bool) -> Result<ShelfScan> {
        use std::collections::{HashMap, HashSet};
        // Group-scoped personal state (the source of truth for
        // scoped fields once a group row exists — compose rule).
        let mut group_state: HashMap<String, RomGameState> = HashMap::new();
        {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT group_key, rating, favorite, completed,
                            title_override, genre, players, notes
                     FROM rom_group_state",
                )
                .map_err(|e| UfmError::Database(e.to_string()))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        RomGameState {
                            rating: row.get(1)?,
                            favorite: row.get::<_, i64>(2)? != 0,
                            completed: row.get::<_, i64>(3)? != 0,
                            play_count: 0,
                            last_played: None,
                            title_override: row.get(4)?,
                            genre: row.get(5)?,
                            players: row.get(6)?,
                            notes: row.get(7)?,
                        },
                    ))
                })
                .map_err(|e| UfmError::Database(e.to_string()))?;
            for r in rows {
                let (k, s) = r.map_err(|e| UfmError::Database(e.to_string()))?;
                group_state.insert(k, s);
            }
        }
        // Per-path facts (play history + the pre-group scoped
        // residue the favorite/completed formulas fall back to).
        let mut path_facts: HashMap<String, ShelfPathFacts> = HashMap::new();
        {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT provider_key, rating, favorite, completed,
                            play_count, last_played FROM rom_game_state",
                )
                .map_err(|e| UfmError::Database(e.to_string()))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        ShelfPathFacts {
                            rating: row.get(1)?,
                            favorite: row.get::<_, i64>(2)? != 0,
                            completed: row.get::<_, i64>(3)? != 0,
                            play_count: row.get(4)?,
                            last_played: row.get(5)?,
                        },
                    ))
                })
                .map_err(|e| UfmError::Database(e.to_string()))?;
            for r in rows {
                let (k, s) = r.map_err(|e| UfmError::Database(e.to_string()))?;
                path_facts.insert(k, s);
            }
        }
        let mut starred: HashSet<String> = HashSet::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT provider_key FROM path_stars")
                .map_err(|e| UfmError::Database(e.to_string()))?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|e| UfmError::Database(e.to_string()))?;
            for r in rows {
                starred.insert(r.map_err(|e| UfmError::Database(e.to_string()))?);
            }
        }
        // Found-media bits per item (the fetch pair builder reads
        // the same map, so "missing" can never drift between the
        // shelf and the fetch it launches).
        let mut media_found: HashMap<String, u8> = HashMap::new();
        {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT item_id, media_type FROM rom_media
                     WHERE status = 'found' AND local_path IS NOT NULL",
                )
                .map_err(|e| UfmError::Database(e.to_string()))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| UfmError::Database(e.to_string()))?;
            for r in rows {
                let (item_id, media_type) =
                    r.map_err(|e| UfmError::Database(e.to_string()))?;
                // PC games carry their box art as 'cover' (adopted
                // from the source library at import); it IS the
                // group's box art for every missing/has-art rule.
                // ROM items never carry 'cover', so ROM readings
                // are untouched.
                let bit = ROM_MEDIA_BIT
                    .iter()
                    .find(|(n, _)| *n == media_type)
                    .map(|(_, b)| *b)
                    .or(if media_type == "cover" { Some(1) } else { None });
                if let Some(bit) = bit {
                    *media_found.entry(item_id).or_default() |= bit;
                }
            }
        }
        let mut labels_by_path: HashMap<String, Vec<String>> = HashMap::new();
        if need_labels {
            let mut stmt = self
                .conn
                .prepare("SELECT provider_key, label FROM path_labels")
                .map_err(|e| UfmError::Database(e.to_string()))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| UfmError::Database(e.to_string()))?;
            for r in rows {
                let (pk, label) = r.map_err(|e| UfmError::Database(e.to_string()))?;
                labels_by_path.entry(pk).or_default().push(label);
            }
        }
        // PC Games P1: the imported facet facts for pc-game
        // groups, keyed by install path — libraries claiming the
        // group and its genre union. Read once per scan like every
        // other side map; PC libraries are hundreds of rows, not a
        // scale concern beside the ROM stream.
        let mut pc_facts: HashMap<String, (String, Vec<String>)> = HashMap::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT provider_key, library_name, genres_json FROM pc_metadata")
                .map_err(|e| UfmError::Database(e.to_string()))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .map_err(|e| UfmError::Database(e.to_string()))?;
            for r in rows {
                let (pk, library, genres_json) =
                    r.map_err(|e| UfmError::Database(e.to_string()))?;
                let genres: Vec<String> =
                    serde_json::from_str(&genres_json).unwrap_or_default();
                pc_facts.insert(pk, (library, genres));
            }
        }
        // The lean identity stream: no enrichment, no per-row side
        // queries — fold straight into group facts.
        let mut groups: HashMap<String, ShelfGroupFacts> = HashMap::new();
        {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT i.id, i.provider_key, mi.title, mi.platform,
                            mi.year, mi.sha1, i.modified, mi.match_evidence_json
                     FROM media_identities AS mi
                     JOIN items AS i ON i.id = mi.item_id
                     WHERE mi.media_kind IN ('rom', 'pc-game')
                       AND mi.title IS NOT NULL
                       AND mi.platform IS NOT NULL",
                )
                .map_err(|e| UfmError::Database(e.to_string()))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, String>(7)?,
                    ))
                })
                .map_err(|e| UfmError::Database(e.to_string()))?;
            for r in rows {
                let (item_id, provider_key, title, platform, year, sha1, modified, evidence) =
                    r.map_err(|e| UfmError::Database(e.to_string()))?;
                let group_key = ufm_media::romdat::rom_group_key(&platform, &title);
                let (also, crc) = shelf_evidence(&evidence);
                let bits = media_found.get(&item_id).copied().unwrap_or(0);
                // Borrowed lookup on the hot path — the owned
                // key is only built for a group's first member.
                let g = match groups.get_mut(&group_key) {
                    Some(g) => g,
                    None => groups
                        .entry(group_key.clone())
                        .or_insert_with(|| ShelfGroupFacts {
                            group_key: group_key.clone(),
                            platform: platform.clone(),
                            ..Default::default()
                        }),
                };
                if let Some(pk) = &provider_key {
                    if let Some(pf) = path_facts.get(pk) {
                        g.any_path_favorite |= pf.favorite;
                        g.any_path_completed |= pf.completed;
                        g.max_path_rating = match (g.max_path_rating, pf.rating) {
                            (Some(a), Some(b)) => Some(a.max(b)),
                            (a, b) => a.or(b),
                        };
                        g.play_total += pf.play_count;
                        if pf.last_played.as_deref() > g.play_last.as_deref() {
                            g.play_last = pf.last_played.clone();
                        }
                    }
                    if starred.contains(pk) {
                        g.any_starred = true;
                    }
                    if let Some((library, genres)) = pc_facts.get(pk) {
                        if !g.pc_sources.contains(library) {
                            g.pc_sources.push(library.clone());
                        }
                        for genre in genres {
                            if !g.pc_genres.contains(genre) {
                                g.pc_genres.push(genre.clone());
                            }
                        }
                    }
                }
                for a in also {
                    if !g.also.contains(&a) {
                        g.also.push(a);
                    }
                }
                if bits & 1 != 0 {
                    g.has_boxart = true;
                }
                if bits & 8 != 0 {
                    g.has_trailer = true;
                }
                if !light {
                    g.members.push(ShelfMemberFacts {
                        item_id,
                        provider_key,
                        title,
                        sha1,
                        crc,
                        year,
                        modified,
                    });
                }
            }
        }
        let mut out: Vec<ShelfGroupFacts> = groups.into_values().collect();
        for g in &mut out {
            g.group_state = group_state.get(&g.group_key).cloned();
            g.also.sort();
            g.also.dedup();
            g.also.retain(|a| a != &g.platform);
            g.pc_sources.sort();
            g.pc_genres.sort();
            if light {
                continue;
            }
            // Display title: the group's own override wins; else the
            // parsed base title of the alphabetically-first member
            // (deterministic — every member shares the base modulo
            // case, by construction of the group key).
            let base = g
                .members
                .iter()
                .map(|m| m.title.as_str())
                .min()
                .map(ufm_media::romdat::parsed_base_title)
                .unwrap_or_default();
            g.display_title = g
                .group_state
                .as_ref()
                .and_then(|s| s.title_override.clone())
                .unwrap_or(base);
            g.sort_title = if g.display_title.is_ascii() {
                g.display_title.to_ascii_lowercase()
            } else {
                g.display_title.to_lowercase()
            };
            let (dy, dm) = shelf_default_version_span(g);
            g.def_year = dy;
            g.def_modified = dm;
        }
        Ok(ShelfScan {
            groups: out,
            media_found,
            labels_by_path,
        })
    }

    /// One bounded window of a shelf, judged over the full set.
    /// The window holds WHOLE groups; `total` is the true count of
    /// qualifying groups; `next_cursor` chains to the next window
    /// by sort key (never by offset), so concurrent indexing can
    /// neither duplicate nor skip groups between windows.
    pub fn rom_home_shelf_window(
        &self,
        spec: &RomHomeShelfSpec,
    ) -> Result<RomHomeShelfPage> {
        let scan = self.rom_home_scan(spec.kind == "search")?;
        let mut keyed: Vec<(ShelfSortKey, &ShelfGroupFacts)> = scan
            .groups
            .iter()
            .filter(|g| shelf_group_matches(g, spec, &scan))
            .map(|g| (shelf_sort_key_of(g), g))
            .collect();
        let total = keyed.len() as i64;
        keyed.sort_by(|a, b| shelf_key_cmp(&a.0, &b.0, &spec.sort));
        let mut start = 0usize;
        if let Some(raw_cursor) = &spec.cursor {
            if let Ok(c) = serde_json::from_str::<ShelfCursor>(raw_cursor) {
                if c.sort == spec.sort {
                    let ck = ShelfSortKey {
                        sort_title: c.sort_title,
                        def_year: c.def_year,
                        play_last: c.play_last,
                        play_total: c.play_total,
                        def_modified: c.def_modified,
                        group_key: c.group_key,
                    };
                    start = keyed
                        .iter()
                        .position(|(k, _)| {
                            shelf_key_cmp(k, &ck, &spec.sort) == std::cmp::Ordering::Greater
                        })
                        .unwrap_or(keyed.len());
                }
            }
        }
        let limit = spec.limit.clamp(1, 5000) as usize;
        let end = keyed.len().min(start + limit);
        let window = &keyed[start..end];
        let next_cursor = if end < keyed.len() {
            let (k, _) = &window[window.len() - 1];
            Some(serde_json::to_string(&ShelfCursor {
                sort: spec.sort.clone(),
                sort_title: k.sort_title.clone(),
                def_year: k.def_year,
                play_last: k.play_last.clone(),
                play_total: k.play_total,
                def_modified: k.def_modified.clone(),
                group_key: k.group_key.clone(),
            })
            .unwrap_or_default())
        } else {
            None
        };
        // Enrich exactly the window's members, in window order.
        let mut rank: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        let mut ordered_ids: Vec<String> = Vec::new();
        for (i, (_, g)) in window.iter().enumerate() {
            for m in &g.members {
                rank.insert(m.item_id.clone(), i);
                ordered_ids.push(m.item_id.clone());
            }
        }
        let mut raw = self.rom_home_raw_rows_for_ids(&ordered_ids)?;
        raw.sort_by(|a, b| {
            let ra = rank.get(&a.0).copied().unwrap_or(usize::MAX);
            let rb = rank.get(&b.0).copied().unwrap_or(usize::MAX);
            ra.cmp(&rb)
                .then_with(|| a.7.cmp(&b.7))
                .then_with(|| a.0.cmp(&b.0))
        });
        let games = self.enrich_rom_home_rows(raw)?;
        Ok(RomHomeShelfPage {
            total,
            games,
            next_cursor,
        })
    }

    /// The missing-media fetch pairs for a whole shelf spec (the
    /// fetch actions judge the full set here instead of building
    /// pair lists from a full in-memory game set in the webview).
    /// Cursor/limit in the spec are ignored — a fetch is total by
    /// definition; the room confirms the count before starting.
    pub fn rom_home_missing_pairs(
        &self,
        spec: &RomHomeShelfSpec,
    ) -> Result<Vec<RomHomeMissingPair>> {
        let scan = self.rom_home_scan(spec.kind == "search")?;
        let mut groups: Vec<&ShelfGroupFacts> = scan
            .groups
            .iter()
            .filter(|g| shelf_group_matches(g, spec, &scan))
            .collect();
        groups.sort_by(|a, b| {
            a.sort_title
                .cmp(&b.sort_title)
                .then_with(|| a.group_key.cmp(&b.group_key))
        });
        let mut out = Vec::new();
        for g in groups {
            // PC games never enter the ROM media-fetch cascade:
            // their art is adopted from the source library at
            // import (or honestly absent), and the fetch sources
            // serve emulated systems. Declared in the P1 design.
            if g.platform == "pc" {
                continue;
            }
            for m in &g.members {
                let bits = scan.media_found.get(&m.item_id).copied().unwrap_or(0);
                for (name, bit) in ROM_MEDIA_BIT {
                    if bits & bit == 0 {
                        out.push(RomHomeMissingPair {
                            item_id: m.item_id.clone(),
                            media_type: name.to_string(),
                        });
                    }
                }
            }
        }
        Ok(out)
    }

    /// Genre facet counts for the room's Filters popover, counted
    /// in GAMES (group rows carry genre — the backfill invariant
    /// makes scoped state group-owned). Ordered by count desc,
    /// then name, like the popover has always presented them.
    /// PC Games P1: pc-game groups carry imported genres instead,
    /// counted per GROUP from the same scan the shelves use and
    /// merged in — a genre living in both worlds sums.
    pub fn rom_home_genre_facets(&self) -> Result<Vec<(String, i64)>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT genre, COUNT(*) FROM rom_group_state
                 WHERE genre IS NOT NULL AND genre != ''
                 GROUP BY genre ORDER BY COUNT(*) DESC, genre ASC",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut merged: std::collections::HashMap<String, i64> =
            std::collections::HashMap::new();
        for r in rows {
            let (genre, n) = r.map_err(|e| UfmError::Database(e.to_string()))?;
            *merged.entry(genre).or_default() += n;
        }
        drop(stmt);
        let scan = self.rom_home_scan_mode(false, true)?;
        for g in &scan.groups {
            for genre in &g.pc_genres {
                *merged.entry(genre.clone()).or_default() += 1;
            }
        }
        let mut out: Vec<(String, i64)> = merged.into_iter().collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Ok(out)
    }

    /// Lean per-file rows for the per-system tallies the command
    /// layer finishes (byte totals are primary-platform sums over
    /// member files; offline truth needs volume badges, which live
    /// in the session layer — the catalog hands over the rows).
    pub fn rom_home_system_file_rows(&self) -> Result<Vec<RomHomeSystemFileRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT i.id, i.location_id, i.provider_key, mi.platform,
                        mi.title, i.size_bytes
                 FROM media_identities AS mi
                 JOIN items AS i ON i.id = mi.item_id
                 WHERE mi.media_kind = 'rom' AND mi.title IS NOT NULL
                   AND mi.platform IS NOT NULL
                 ORDER BY mi.platform, mi.title COLLATE NOCASE, i.id",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(RomHomeSystemFileRow {
                    item_id: row.get(0)?,
                    location_id: row.get(1)?,
                    provider_key: row.get(2)?,
                    platform: row.get(3)?,
                    title: row.get(4)?,
                    size_bytes: row.get(5)?,
                })
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut out: Vec<RomHomeSystemFileRow> =
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| UfmError::Database(e.to_string()))?;
        drop(stmt);
        // PC Games P1: pc-game folder items carry no content size
        // of their own (contents are never walked); the section's
        // byte total is the imported install sizes, which is the
        // number the source library itself reports.
        let mut stmt = self
            .conn
            .prepare(
                "SELECT i.id, i.location_id, i.provider_key, mi.platform,
                        mi.title, pm.install_size
                 FROM media_identities AS mi
                 JOIN items AS i ON i.id = mi.item_id
                 LEFT JOIN pc_metadata AS pm ON pm.provider_key = i.provider_key
                 WHERE mi.media_kind = 'pc-game' AND mi.title IS NOT NULL
                   AND mi.platform IS NOT NULL
                 ORDER BY mi.title COLLATE NOCASE, i.id",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(RomHomeSystemFileRow {
                    item_id: row.get(0)?,
                    location_id: row.get(1)?,
                    provider_key: row.get(2)?,
                    platform: row.get(3)?,
                    title: row.get(4)?,
                    size_bytes: row.get(5)?,
                })
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        for r in rows {
            out.push(r.map_err(|e| UfmError::Database(e.to_string()))?);
        }
        Ok(out)
    }

    /// Raw rows for a specific set of item ids (the window's
    /// members) — the same SELECT shape as `rom_home_raw_rows`,
    /// chunked for SQLite's parameter comfort.
    fn rom_home_raw_rows_for_ids(
        &self,
        item_ids: &[String],
    ) -> Result<Vec<RomHomeRawRow>> {
        let mut out = Vec::new();
        for chunk in item_ids.chunks(400) {
            let placeholders: Vec<String> =
                chunk.iter().enumerate().map(|(i, _)| format!("?{}", i + 1)).collect();
            let sql = format!(
                "SELECT i.id, i.location_id, i.name, i.kind, i.size_bytes,
                        i.modified, i.provider_key, mi.title, mi.platform,
                        mi.year, mi.sha1, mi.match_evidence_json
                 FROM media_identities AS mi
                 JOIN items AS i ON i.id = mi.item_id
                 WHERE mi.media_kind IN ('rom', 'pc-game')
                   AND mi.title IS NOT NULL
                   AND mi.platform IS NOT NULL AND i.id IN ({})",
                placeholders.join(",")
            );
            let mut stmt = self
                .conn
                .prepare(&sql)
                .map_err(|e| UfmError::Database(e.to_string()))?;
            let values: Vec<rusqlite::types::Value> =
                chunk.iter().map(|s| rusqlite::types::Value::Text(s.clone())).collect();
            let rows = stmt
                .query_map(rusqlite::params_from_iter(values.iter()), |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                        row.get(10)?,
                        row.get(11)?,
                    ))
                })
                .map_err(|e| UfmError::Database(e.to_string()))?;
            for r in rows {
                out.push(r.map_err(|e| UfmError::Database(e.to_string()))?);
            }
        }
        Ok(out)
    }

    /// The room's tallies, from the catalog alone — counted in
    /// GAMES (Phase 3 groups), not files: total = distinct group
    /// keys; a group counts as missing media when NO member item
    /// has found box art; the per-system split uses the same
    /// membership rule as `rom_home_games` (primary + `also`
    /// platforms), counting each group once per shelf it sits on.
    /// Hyperbase #4: computed from the shared group scan, which
    /// also yields the rail's Favorites / Recently Played group
    /// counts under the same definitions the shelves use.
    pub fn rom_home_counts(&self) -> Result<RomHomeCounts> {
        let scan = self.rom_home_scan_mode(false, true)?;
        let mut counts = RomHomeCounts::default();
        counts.total = scan.groups.len() as i64;
        let mut per: std::collections::HashMap<String, (i64, i64)> =
            std::collections::HashMap::new();
        let mut pc_libs: std::collections::HashMap<String, i64> =
            std::collections::HashMap::new();
        for g in &scan.groups {
            if !g.has_boxart {
                counts.missing_media += 1;
            }
            if shelf_group_favorite(g) {
                counts.favorite_groups += 1;
            }
            if g.play_last.is_some() {
                counts.recent_groups += 1;
            }
            for lib in &g.pc_sources {
                *pc_libs.entry(lib.clone()).or_default() += 1;
            }
            for shelf in std::iter::once(&g.platform).chain(g.also.iter()) {
                let e = per.entry(shelf.clone()).or_default();
                e.0 += 1;
                if !g.has_boxart {
                    e.1 += 1;
                }
            }
        }
        let mut per_system: Vec<(String, i64, i64)> = per
            .into_iter()
            .map(|(p, (n, m))| (p, n, m))
            .collect();
        per_system.sort_by(|a, b| a.0.cmp(&b.0));
        counts.per_system = per_system;
        let mut pc_libraries: Vec<(String, i64)> = pc_libs.into_iter().collect();
        pc_libraries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        counts.pc_libraries = pc_libraries;
        Ok(counts)
    }

    // --------------------------------------------------------
    // PC Games (2026-10-17, PC Games Library P1): the import
    // write path + the sheet read. Identities ride the shared
    // Games Home shelf scan above (media_kind 'pc-game',
    // platform 'pc'); descriptive metadata and install/launch
    // facts live in pc_metadata / pc_game_state; personal state
    // deliberately shares rom_game_state / rom_group_state (the
    // design's one-game-state ruling — no forked favorites).
    // --------------------------------------------------------

    /// Insert a catalog location row ONLY when absent — the
    /// importers resolve locations that usually already exist
    /// (volume locations written by the indexer), and
    /// insert_location's INSERT OR REPLACE would clobber the
    /// stored capabilities of a live row.
    pub fn pc_ensure_location(
        &self,
        id: &str,
        provider: &str,
        display_name: &str,
        capabilities_json: &str,
    ) -> Result<()> {
        let exists: bool = self
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM locations WHERE id = ?1)",
                params![id],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n != 0)
            .map_err(|e| UfmError::Database(e.to_string()))?;
        if exists {
            return Ok(());
        }
        let caps: serde_json::Value =
            serde_json::from_str(capabilities_json).unwrap_or(serde_json::json!({}));
        self.insert_location(&ufm_vfs::Location {
            id: ufm_core::LocationId(
                uuid::Uuid::parse_str(id)
                    .map_err(|e| UfmError::InvalidInput(format!("location id: {e}")))?,
            ),
            provider: provider.to_string(),
            display_name: display_name.to_string(),
            capabilities: serde_json::from_value::<ufm_vfs::Capabilities>(caps)
                .map_err(|e| UfmError::InvalidInput(format!("capabilities: {e}")))?,
            protected: ufm_vfs::ProtectionTier::None,
        })
    }

    /// Write one PC install: the shallow folder item, its
    /// pc-game identity, the imported metadata and the install
    /// facts — one transaction per game, so a crashed import
    /// leaves whole games, never halves, and a re-run finishes
    /// the job (idempotent by provider key throughout).
    ///
    /// Source precedence is playnite > folder-scan: a folder-scan
    /// write never touches a folder a Playnite import owns, and a
    /// Playnite write UPGRADES a folder-scan identity in place
    /// (personal state lives in other tables and is untouched
    /// either way). A folder already carrying a non-PC identity
    /// is never clobbered.
    pub fn upsert_pc_install(&self, rec: &PcInstallRecord) -> Result<PcUpsertOutcome> {
        let location_uuid = uuid::Uuid::parse_str(&rec.location_id)
            .map_err(|e| UfmError::InvalidInput(format!("location id: {e}")))?;
        // Read BEFORE writing: precedence is judged on what already
        // owns the folder, and an existing item is refreshed with a
        // plain UPDATE — insert_item's INSERT OR REPLACE would
        // cascade-delete the very identity this write maintains.
        let existing_row: Option<(String, Option<String>, Option<String>)> = {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT i.id, mi.media_kind, mi.match_evidence_json
                     FROM items AS i
                     LEFT JOIN media_identities AS mi ON mi.item_id = i.id
                     WHERE i.location_id = ?1 AND i.provider_key = ?2",
                )
                .map_err(|e| UfmError::Database(e.to_string()))?;
            let mut rows = stmt
                .query_map(params![rec.location_id, rec.provider_key], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })
                .map_err(|e| UfmError::Database(e.to_string()))?;
            match rows.next() {
                Some(r) => Some(r.map_err(|e| UfmError::Database(e.to_string()))?),
                None => None,
            }
        };
        let mut outcome = PcUpsertOutcome {
            item_id: String::new(),
            created: false,
            upgraded: false,
            skipped_precedence: false,
        };
        if let Some((_, kind, evidence)) = &existing_row {
            if let Some(kind) = kind {
                if kind != "pc-game" {
                    outcome.skipped_precedence = true;
                    outcome.item_id = existing_row.as_ref().unwrap().0.clone();
                    return Ok(outcome);
                }
                let existing_source = evidence
                    .as_deref()
                    .and_then(|e| serde_json::from_str::<serde_json::Value>(e).ok())
                    .and_then(|v| {
                        v.get("source")
                            .and_then(|s| s.as_str())
                            .map(|s| s.to_string())
                    });
                match (existing_source.as_deref(), rec.source_kind.as_str()) {
                    (Some("playnite"), "folder-scan") => {
                        outcome.skipped_precedence = true;
                        outcome.item_id = existing_row.as_ref().unwrap().0.clone();
                        return Ok(outcome);
                    }
                    (Some("folder-scan"), "playnite") => outcome.upgraded = true,
                    _ => {}
                }
            }
        }
        let had_pc_identity = existing_row
            .as_ref()
            .map(|(_, kind, _)| kind.as_deref() == Some("pc-game"))
            .unwrap_or(false);
        outcome.created = !had_pc_identity;
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let item_id = match &existing_row {
            Some((id, _, _)) => {
                self.conn
                    .execute(
                        "UPDATE items SET name = ?2, modified = ?3 WHERE id = ?1",
                        params![
                            id,
                            rec.folder_name,
                            rec.folder_modified.map(|m| m.to_rfc3339()),
                        ],
                    )
                    .map_err(|e| UfmError::Database(e.to_string()))?;
                id.clone()
            }
            None => {
                let item = ufm_vfs::Item {
                    id: ufm_core::ItemId::new(),
                    location: ufm_core::LocationId(location_uuid),
                    name: rec.folder_name.clone(),
                    kind: ufm_vfs::ItemKind::Folder,
                    size_bytes: None,
                    modified: rec.folder_modified,
                    provider_key: Some(rec.provider_key.clone()),
                    labels: vec![],
                    starred: false,
                };
                let id = item.id.to_string();
                self.insert_item(&item)?;
                id
            }
        };
        outcome.item_id = item_id.clone();
        self.conn
            .execute(
                "INSERT INTO media_identities
                    (item_id, media_kind, title, year, tmdb_id, platform,
                     sha1, match_evidence_json)
                 VALUES (?1, 'pc-game', ?2, ?3, NULL, 'pc', NULL, ?4)
                 ON CONFLICT(item_id) DO UPDATE SET
                    media_kind = 'pc-game', title = excluded.title,
                    year = excluded.year, platform = 'pc',
                    match_evidence_json = excluded.match_evidence_json",
                params![item_id, rec.title, rec.year, rec.evidence_json],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let json_list = |v: &Vec<String>| serde_json::to_string(v).unwrap_or_else(|_| "[]".into());
        self.conn
            .execute(
                "INSERT OR REPLACE INTO pc_metadata
                    (provider_key, source_kind, library_name, playnite_id,
                     game_guid, description, genres_json, developers_json,
                     publishers_json, series_json, features_json,
                     age_ratings_json, links_json, storefronts_json,
                     critic_score, community_score, install_size, version,
                     exe_name, exe_rel, release_raw, mislabel_note,
                     imported_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                         ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20,
                         ?21, ?22, ?23)",
                params![
                    rec.provider_key,
                    rec.source_kind,
                    rec.library_name,
                    rec.playnite_id,
                    rec.game_guid,
                    rec.description,
                    json_list(&rec.genres),
                    json_list(&rec.developers),
                    json_list(&rec.publishers),
                    json_list(&rec.series),
                    json_list(&rec.features),
                    json_list(&rec.age_ratings),
                    serde_json::to_string(&rec.links).unwrap_or_else(|_| "[]".into()),
                    json_list(&rec.storefronts),
                    rec.critic_score,
                    rec.community_score,
                    rec.install_size,
                    rec.version,
                    rec.exe_name,
                    rec.exe_rel,
                    rec.release_raw,
                    rec.mislabel_note,
                    rec.imported_at,
                ],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        // Install facts: presence is import-owned and refreshes;
        // the launch badge is NOT — it moves only on an observed
        // launch or a manual mark (pc_set_launch_badge).
        self.conn
            .execute(
                "INSERT INTO pc_game_state
                    (provider_key, present, launch_badge, launch_note,
                     checked_at, updated_at)
                 VALUES (?1, ?2, 'not-checked', NULL, ?3, ?3)
                 ON CONFLICT(provider_key) DO UPDATE SET
                    present = excluded.present,
                    checked_at = excluded.checked_at,
                    updated_at = excluded.updated_at",
                params![rec.provider_key, rec.present as i64, rec.imported_at],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        tx.commit().map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(outcome)
    }

    /// Move a game's launch badge — the ONLY door that writes it:
    /// an observed launch (P2's ladder) or Dickles marking a game
    /// by hand. Returns false when the path has no PC install.
    pub fn pc_set_launch_badge(
        &self,
        provider_key: &str,
        badge: &str,
        note: Option<&str>,
        at: &str,
    ) -> Result<bool> {
        let n = self
            .conn
            .execute(
                "UPDATE pc_game_state SET launch_badge = ?2,
                        launch_note = ?3, updated_at = ?4
                 WHERE provider_key = ?1",
                params![provider_key, badge, note, at],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(n > 0)
    }

    /// The game sheet's PC record for one install path, plus every
    /// install of the same shelf group. None when the path is not
    /// a PC install.
    /// All provider keys owned by Playnite imports — the
    /// folder scan defers to these (a candidate folder that IS
    /// one, or CONTAINS one as a descendant, is already
    /// covered by the richer record).
    pub fn pc_playnite_keys(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT provider_key FROM pc_metadata
                 WHERE source_kind = 'playnite'",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| UfmError::Database(e.to_string()))?);
        }
        Ok(out)
    }

    pub fn pc_game_sheet(&self, provider_key: &str) -> Result<Option<PcGameSheet>> {
        let row = {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT mi.title, mi.year, pm.source_kind,
                            pm.library_name, pm.playnite_id, pm.game_guid,
                            pm.description, pm.genres_json,
                            pm.developers_json, pm.publishers_json,
                            pm.series_json, pm.features_json,
                            pm.age_ratings_json, pm.links_json,
                            pm.storefronts_json, pm.critic_score,
                            pm.community_score, pm.install_size,
                            pm.version, pm.exe_name, pm.exe_rel,
                            pm.release_raw, pm.mislabel_note,
                            pm.imported_at, COALESCE(pgs.present, 0),
                            COALESCE(pgs.launch_badge, 'not-checked'),
                            pgs.launch_note, pgs.checked_at
                     FROM pc_metadata AS pm
                     JOIN items AS i ON i.provider_key = pm.provider_key
                     JOIN media_identities AS mi ON mi.item_id = i.id
                     LEFT JOIN pc_game_state AS pgs
                       ON pgs.provider_key = pm.provider_key
                     WHERE pm.provider_key = ?1
                       AND mi.media_kind = 'pc-game'",
                )
                .map_err(|e| UfmError::Database(e.to_string()))?;
            let mut rows = stmt
                .query_map(params![provider_key], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, String>(10)?,
                        row.get::<_, String>(11)?,
                        row.get::<_, String>(12)?,
                        row.get::<_, String>(13)?,
                        row.get::<_, String>(14)?,
                        row.get::<_, Option<i64>>(15)?,
                        row.get::<_, Option<i64>>(16)?,
                        row.get::<_, Option<i64>>(17)?,
                        row.get::<_, Option<String>>(18)?,
                        row.get::<_, Option<String>>(19)?,
                        row.get::<_, Option<String>>(20)?,
                        row.get::<_, Option<String>>(21)?,
                        row.get::<_, Option<String>>(22)?,
                        row.get::<_, String>(23)?,
                        row.get::<_, i64>(24)?,
                        row.get::<_, String>(25)?,
                        row.get::<_, Option<String>>(26)?,
                        row.get::<_, Option<String>>(27)?,
                    ))
                })
                .map_err(|e| UfmError::Database(e.to_string()))?;
            match rows.next() {
                Some(r) => Some(r.map_err(|e| UfmError::Database(e.to_string()))?),
                None => None,
            }
        };
        let Some((
            title,
            year,
            source_kind,
            library_name,
            playnite_id,
            game_guid,
            description,
            genres_json,
            developers_json,
            publishers_json,
            series_json,
            features_json,
            age_ratings_json,
            links_json,
            storefronts_json,
            critic_score,
            community_score,
            install_size,
            version,
            exe_name,
            exe_rel,
            release_raw,
            mislabel_note,
            imported_at,
            present,
            launch_badge,
            launch_note,
            checked_at,
        )) = row
        else {
            return Ok(None);
        };
        let parse_list = |s: &str| -> Vec<String> {
            serde_json::from_str(s).unwrap_or_default()
        };
        // Group siblings: every pc install whose identity folds
        // into the same shelf group (same title, other folders /
        // editions / libraries).
        let mut installs = Vec::new();
        if let Some(group_key) = self.rom_group_key_for_path(provider_key)? {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT i.provider_key, mi.title, mi.platform,
                            COALESCE(pm.library_name, ''),
                            COALESCE(pm.source_kind, ''),
                            COALESCE(pgs.present, 0),
                            COALESCE(pgs.launch_badge, 'not-checked'),
                            pm.exe_name, pm.install_size
                     FROM media_identities AS mi
                     JOIN items AS i ON i.id = mi.item_id
                     LEFT JOIN pc_metadata AS pm
                       ON pm.provider_key = i.provider_key
                     LEFT JOIN pc_game_state AS pgs
                       ON pgs.provider_key = i.provider_key
                     WHERE mi.media_kind = 'pc-game'
                       AND mi.title IS NOT NULL AND mi.platform IS NOT NULL",
                )
                .map_err(|e| UfmError::Database(e.to_string()))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, Option<i64>>(8)?,
                    ))
                })
                .map_err(|e| UfmError::Database(e.to_string()))?;
            for r in rows {
                let (pk, t, platform, lib, kind, pres, badge, exe, size) =
                    r.map_err(|e| UfmError::Database(e.to_string()))?;
                if ufm_media::romdat::rom_group_key(&platform, &t) == group_key {
                    installs.push(PcInstallRow {
                        provider_key: pk,
                        title: t,
                        library_name: lib,
                        source_kind: kind,
                        present: pres != 0,
                        launch_badge: badge,
                        exe_name: exe,
                        install_size: size,
                    });
                }
            }
            installs.sort_by(|a, b| a.provider_key.cmp(&b.provider_key));
        }
        Ok(Some(PcGameSheet {
            provider_key: provider_key.to_string(),
            title,
            year,
            source_kind,
            library_name,
            playnite_id,
            game_guid,
            description,
            genres: parse_list(&genres_json),
            developers: parse_list(&developers_json),
            publishers: parse_list(&publishers_json),
            series: parse_list(&series_json),
            features: parse_list(&features_json),
            age_ratings: parse_list(&age_ratings_json),
            links: serde_json::from_str(&links_json).unwrap_or_default(),
            storefronts: parse_list(&storefronts_json),
            critic_score,
            community_score,
            install_size,
            version,
            exe_name,
            exe_rel,
            release_raw,
            mislabel_note,
            imported_at,
            present: present != 0,
            launch_badge,
            launch_note,
            checked_at,
            installs,
        }))
    }

    // --------------------------------------------------------
    // Phase 3 grouping: game-scoped state (rom_group_state).
    // Resolution is path → item → identity → group key. Reads
    // compose group-scoped fields over the path's own play fields;
    // writes route to the group row whenever the path resolves.
    // Backfill COPIES (donor path rows keep their fields, so a
    // rollback build still sees them); once a group row exists it
    // is the source of truth for scoped fields.
    // --------------------------------------------------------

    /// The group key a path's game belongs to, when the path is an
    /// identified ROM item. None for unidentified / extension-rule
    /// files — their state stays path-keyed, orphan-safe.
    pub fn rom_group_key_for_path(&self, provider_key: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT mi.platform, mi.title FROM items AS i
                 JOIN media_identities AS mi ON mi.item_id = i.id
                 WHERE i.provider_key = ?1 AND mi.media_kind IN ('rom', 'pc-game')
                   AND mi.title IS NOT NULL AND mi.platform IS NOT NULL
                 LIMIT 1",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut rows = stmt
            .query_map(params![provider_key], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        match rows.next() {
            Some(r) => {
                let (platform, title) =
                    r.map_err(|e| UfmError::Database(e.to_string()))?;
                Ok(Some(ufm_media::romdat::rom_group_key(&platform, &title)))
            }
            None => Ok(None),
        }
    }

    /// The raw path row, when one exists (no group composition).
    fn rom_path_state_row(&self, provider_key: &str) -> Result<Option<RomGameState>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT rating, favorite, completed, play_count, last_played,
                        title_override, genre, players, notes
                 FROM rom_game_state WHERE provider_key = ?1",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut rows = stmt
            .query_map(params![provider_key], |row| {
                Ok(RomGameState {
                    rating: row.get(0)?,
                    favorite: row.get::<_, i64>(1)? != 0,
                    completed: row.get::<_, i64>(2)? != 0,
                    play_count: row.get(3)?,
                    last_played: row.get(4)?,
                    title_override: row.get(5)?,
                    genre: row.get(6)?,
                    players: row.get(7)?,
                    notes: row.get(8)?,
                })
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        match rows.next() {
            Some(r) => Ok(Some(r.map_err(|e| UfmError::Database(e.to_string()))?)),
            None => Ok(None),
        }
    }

    /// The group row's scoped fields as a state (play fields stay
    /// default — history is per-path, never the group's).
    fn rom_group_state_row(&self, group_key: &str) -> Result<Option<RomGameState>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT rating, favorite, completed, title_override,
                        genre, players, notes
                 FROM rom_group_state WHERE group_key = ?1",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut rows = stmt
            .query_map(params![group_key], |row| {
                Ok(RomGameState {
                    rating: row.get(0)?,
                    favorite: row.get::<_, i64>(1)? != 0,
                    completed: row.get::<_, i64>(2)? != 0,
                    play_count: 0,
                    last_played: None,
                    title_override: row.get(3)?,
                    genre: row.get(4)?,
                    players: row.get(5)?,
                    notes: row.get(6)?,
                })
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        match rows.next() {
            Some(r) => Ok(Some(r.map_err(|e| UfmError::Database(e.to_string()))?)),
            None => Ok(None),
        }
    }

    /// Compose the state a game shows: game-scoped fields from the
    /// group row when one exists (it is the source of truth),
    /// play history always from the item's own path row.
    fn compose_rom_state(
        group: Option<&RomGameState>,
        path: Option<&RomGameState>,
    ) -> RomGameState {
        let mut out = path.cloned().unwrap_or_default();
        if let Some(g) = group {
            out.rating = g.rating;
            out.favorite = g.favorite;
            out.completed = g.completed;
            out.title_override = g.title_override.clone();
            out.genre = g.genre.clone();
            out.players = g.players.clone();
            out.notes = g.notes.clone();
        }
        out
    }

    /// Donate one group's path-row state into its group row, when
    /// the group row doesn't exist yet. Merge rule (donors ordered
    /// by updated_at): rating / title_override / genre / players /
    /// notes — the newest non-empty write wins; favorite /
    /// completed — OR (once marked anywhere, the game is marked).
    /// Donors keep their fields (copies, not moves): a rollback
    /// build reading path rows still sees them.
    fn backfill_group(&self, group_key: &str) -> Result<()> {
        if self.rom_group_state_row(group_key)?.is_some() {
            return Ok(());
        }
        let mut stmt = self
            .conn
            .prepare(
                "SELECT provider_key, rating, favorite, completed,
                        title_override, genre, players, notes
                 FROM rom_game_state
                 WHERE rating IS NOT NULL OR favorite != 0 OR completed != 0
                    OR title_override IS NOT NULL OR genre IS NOT NULL
                    OR players IS NOT NULL OR notes IS NOT NULL
                 ORDER BY updated_at ASC",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let donors = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, i64>(2)? != 0,
                    row.get::<_, i64>(3)? != 0,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| UfmError::Database(e.to_string()))?;
        drop(stmt);
        let mut merged: Option<RomGameState> = None;
        for (pk, rating, favorite, completed, title, genre, players, notes) in donors {
            if self.rom_group_key_for_path(&pk)?.as_deref() != Some(group_key) {
                continue;
            }
            let m = merged.get_or_insert_with(RomGameState::default);
            if rating.is_some() {
                m.rating = rating;
            }
            m.favorite = m.favorite || favorite;
            m.completed = m.completed || completed;
            if title.is_some() {
                m.title_override = title;
            }
            if genre.is_some() {
                m.genre = genre;
            }
            if players.is_some() {
                m.players = players;
            }
            if notes.is_some() {
                m.notes = notes;
            }
        }
        if let Some(m) = merged {
            let now = ufm_core::now_utc().to_rfc3339();
            self.conn
                .execute(
                    "INSERT INTO rom_group_state (group_key, rating, favorite,
                        completed, title_override, genre, players, notes, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        group_key,
                        m.rating,
                        m.favorite as i64,
                        m.completed as i64,
                        m.title_override,
                        m.genre,
                        m.players,
                        m.notes,
                        now
                    ],
                )
                .map_err(|e| UfmError::Database(e.to_string()))?;
        }
        Ok(())
    }

    /// The open-time backfill: every group any donor path row
    /// resolves to gets its group row. Idempotent (groups with a
    /// row are skipped); orphan rows (no identity) never resolve
    /// and keep their state untouched.
    fn backfill_rom_group_state(&self) -> Result<()> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT DISTINCT provider_key FROM rom_game_state
                 WHERE rating IS NOT NULL OR favorite != 0 OR completed != 0
                    OR title_override IS NOT NULL OR genre IS NOT NULL
                    OR players IS NOT NULL OR notes IS NOT NULL",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let keys = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| UfmError::Database(e.to_string()))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| UfmError::Database(e.to_string()))?;
        drop(stmt);
        let mut groups = std::collections::HashSet::new();
        for pk in &keys {
            if let Some(gk) = self.rom_group_key_for_path(pk)? {
                groups.insert(gk);
            }
        }
        for gk in groups {
            self.backfill_group(&gk)?;
        }
        Ok(())
    }

    /// rom_game_state_touch for the group table — with the lazy
    /// per-group backfill first, so a same-session sequence (rate a
    /// file, THEN it identifies, then rate again) never strands the
    /// earlier path-row values.
    fn rom_group_state_touch(
        &self,
        group_key: &str,
        apply: &str,
        args: &[rusqlite::types::Value],
    ) -> Result<()> {
        self.backfill_group(group_key)?;
        let now = ufm_core::now_utc().to_rfc3339();
        self.conn
            .execute(
                "INSERT OR IGNORE INTO rom_group_state (group_key, updated_at)
                 VALUES (?1, ?2)",
                params![group_key, now],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let sql = format!(
            "UPDATE rom_group_state SET {apply}, updated_at = ? WHERE group_key = ?"
        );
        let mut values: Vec<rusqlite::types::Value> = args.to_vec();
        values.push(rusqlite::types::Value::Text(now));
        values.push(rusqlite::types::Value::Text(group_key.to_string()));
        self.conn
            .execute(&sql, rusqlite::params_from_iter(values.iter()))
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// One game's personal state (the default — unrated, unplayed —
    /// when the user has never touched it). For identified games the
    /// game-scoped fields come from the group row (Phase 3); play
    /// history always comes from the item's own path row.
    pub fn rom_game_state_for(&self, provider_key: &str) -> Result<RomGameState> {
        let path = self.rom_path_state_row(provider_key)?;
        let group = match self.rom_group_key_for_path(provider_key)? {
            Some(gk) => self.rom_group_state_row(&gk)?,
            None => None,
        };
        Ok(Self::compose_rom_state(group.as_ref(), path.as_ref()))
    }

    /// Ensure a state row exists, then apply `apply`'s column writes.
    fn rom_game_state_touch(
        &self,
        provider_key: &str,
        apply: &str,
        args: &[rusqlite::types::Value],
    ) -> Result<()> {
        let now = ufm_core::now_utc().to_rfc3339();
        self.conn
            .execute(
                "INSERT OR IGNORE INTO rom_game_state (provider_key, updated_at)
                 VALUES (?1, ?2)",
                params![provider_key, now],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        // Anonymous `?` placeholders bind in order of appearance:
        // the apply-columns first, then updated_at, then the key.
        let sql = format!(
            "UPDATE rom_game_state SET {apply}, updated_at = ? WHERE provider_key = ?"
        );
        let mut values: Vec<rusqlite::types::Value> = args.to_vec();
        values.push(rusqlite::types::Value::Text(now));
        values.push(rusqlite::types::Value::Text(provider_key.to_string()));
        self.conn
            .execute(&sql, rusqlite::params_from_iter(values.iter()))
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// Route a game-scoped write: the group row when the path
    /// resolves to an identified game (Phase 3 — the mark belongs
    /// to the GAME), the path row otherwise (extension-rule and
    /// not-yet-identified files keep today's behavior exactly).
    fn rom_scoped_touch(
        &self,
        provider_key: &str,
        apply: &str,
        args: &[rusqlite::types::Value],
    ) -> Result<()> {
        match self.rom_group_key_for_path(provider_key)? {
            Some(gk) => self.rom_group_state_touch(&gk, apply, args),
            None => self.rom_game_state_touch(provider_key, apply, args),
        }
    }

    /// Set (or clear, with None) the user's star rating for a game.
    pub fn rom_game_set_rating(&self, provider_key: &str, rating: Option<i64>) -> Result<()> {
        let rating = rating.map(|r| r.clamp(1, 5));
        self.rom_scoped_touch(
            provider_key,
            "rating = ?",
            &[match rating {
                Some(r) => rusqlite::types::Value::Integer(r),
                None => rusqlite::types::Value::Null,
            }],
        )
    }

    pub fn rom_game_set_favorite(&self, provider_key: &str, on: bool) -> Result<()> {
        self.rom_scoped_touch(
            provider_key,
            "favorite = ?",
            &[rusqlite::types::Value::Integer(on as i64)],
        )
    }

    pub fn rom_game_set_completed(&self, provider_key: &str, on: bool) -> Result<()> {
        self.rom_scoped_touch(
            provider_key,
            "completed = ?",
            &[rusqlite::types::Value::Integer(on as i64)],
        )
    }

    /// The Edit sheet's fields: the user's own words for a game.
    /// Empty strings clear back to absent.
    pub fn rom_game_set_overrides(
        &self,
        provider_key: &str,
        title_override: Option<&str>,
        genre: Option<&str>,
        players: Option<&str>,
        notes: Option<&str>,
    ) -> Result<()> {
        let clean = |v: Option<&str>| -> rusqlite::types::Value {
            match v.map(|s| s.trim()).filter(|s| !s.is_empty()) {
                Some(s) => rusqlite::types::Value::Text(s.to_string()),
                None => rusqlite::types::Value::Null,
            }
        };
        self.rom_scoped_touch(
            provider_key,
            "title_override = ?, genre = ?, players = ?, notes = ?",
            &[
                clean(title_override),
                clean(genre),
                clean(players),
                clean(notes),
            ],
        )
    }

    /// Play was pressed: the count climbs and last-played lands.
    /// Returns the new state so the caller can paint truth.
    pub fn rom_game_note_play(&self, provider_key: &str) -> Result<RomGameState> {
        let now = ufm_core::now_utc().to_rfc3339();
        self.conn
            .execute(
                "INSERT INTO rom_game_state (provider_key, play_count, last_played, updated_at)
                 VALUES (?1, 1, ?2, ?2)
                 ON CONFLICT(provider_key) DO UPDATE SET
                     play_count = play_count + 1,
                     last_played = excluded.last_played,
                     updated_at = excluded.updated_at",
                params![provider_key, now],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        self.rom_game_state_for(provider_key)
    }

    /// Music Glow-Up Phase 2 (crew Diva): note a play for a
    /// track — the count climbs and last-played lands. Returns the
    /// new state so the caller can paint truth.
    pub fn audio_note_play(&self, provider_key: &str) -> Result<AudioPlayState> {
        let now = ufm_core::now_utc().to_rfc3339();
        self.conn
            .execute(
                "INSERT INTO audio_play_state (provider_key, play_count, last_played_at, updated_at)
                 VALUES (?1, 1, ?2, ?2)
                 ON CONFLICT(provider_key) DO UPDATE SET
                     play_count = play_count + 1,
                     last_played_at = excluded.last_played_at,
                     updated_at = excluded.updated_at",
                params![provider_key, now],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        self.audio_play_state_for(provider_key)
    }

    /// One track's listening state (the default — unplayed — when
    /// the user has never played it).
    pub fn audio_play_state_for(&self, provider_key: &str) -> Result<AudioPlayState> {
        use rusqlite::OptionalExtension;
        let row = self
            .conn
            .query_row(
                "SELECT play_count, last_played_at, last_position_secs, completed_count
                 FROM audio_play_state WHERE provider_key = ?1",
                params![provider_key],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, f64>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(match row {
            Some((play_count, last_played_at, last_position_secs, completed_count)) => {
                AudioPlayState {
                    play_count,
                    last_played_at,
                    last_position_secs,
                    completed_count,
                }
            }
            None => AudioPlayState::default(),
        })
    }

    /// Persist a listening position (the player core's tick saves
    /// every ~15 s of play; a completed listen also bumps the
    /// completion count and rewinds the saved position to 0).
    pub fn audio_set_position(
        &self,
        provider_key: &str,
        secs: f64,
        completed: bool,
    ) -> Result<()> {
        let now = ufm_core::now_utc().to_rfc3339();
        let stored = if completed { 0.0 } else { secs.max(0.0) };
        self.conn
            .execute(
                "INSERT INTO audio_play_state (provider_key, last_position_secs, completed_count, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(provider_key) DO UPDATE SET
                     last_position_secs = excluded.last_position_secs,
                     completed_count = completed_count + ?3,
                     updated_at = excluded.updated_at",
                params![provider_key, stored, completed as i64, now],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// Cached durations for a batch of paths (the catalog's
    /// probe-fed `items.duration_secs`), positionally aligned
    /// with the input. Music surfaces merge these at queue-open
    /// so durations are known before anything plays.
    pub fn durations_for_paths(&self, paths: &[String]) -> Result<Vec<Option<f64>>> {
        use rusqlite::OptionalExtension;
        let mut out = Vec::with_capacity(paths.len());
        for p in paths {
            let d = self
                .conn
                .query_row(
                    "SELECT duration_secs FROM items WHERE provider_key = ?1",
                    params![p],
                    |r| r.get::<_, Option<f64>>(0),
                )
                .optional()
                .map_err(|e| UfmError::Database(e.to_string()))?
                .flatten();
            out.push(d);
        }
        Ok(out)
    }

    /// The Recently Played rail: most recent listens first.
    pub fn audio_recently_played(&self, limit: i64) -> Result<Vec<RecentAudio>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT provider_key, play_count, last_played_at, last_position_secs, completed_count
                 FROM audio_play_state
                 WHERE last_played_at IS NOT NULL
                 ORDER BY last_played_at DESC
                 LIMIT ?1",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![limit.max(1)], |r| {
                Ok(RecentAudio {
                    provider_key: r.get(0)?,
                    state: AudioPlayState {
                        play_count: r.get(1)?,
                        last_played_at: r.get(2)?,
                        last_position_secs: r.get(3)?,
                        completed_count: r.get(4)?,
                    },
                })
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| UfmError::Database(e.to_string()))?);
        }
        Ok(out)
    }

    /// A Smart List: a saved filter, stored as a smart collection
    /// whose query_json is the owner's own filter model (the ROM
    /// Home's filter state). Membership is evaluated by the room
    /// against the loaded games — the sentence on the rail IS the
    /// rule, rendered from the same JSON.
    pub fn create_smart_collection(
        &self,
        id: &CollectionId,
        name: &str,
        query_json: &str,
    ) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO collections (id, name, kind, query_json)
                 VALUES (?1, ?2, 'smart', ?3)",
                params![id.to_string(), name, query_json],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// Rewrite a Smart List's rule in place (the room's chip
    /// editor: open the list, adjust the chips, save — same list,
    /// new sentence). Only kind='smart' rows can be rewritten.
    pub fn update_smart_collection(&self, id: &str, query_json: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE collections SET query_json = ?1 WHERE id = ?2 AND kind = 'smart'",
                params![query_json, id],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// Every Smart List as (id, name, query_json), name-ordered —
    /// the ROM Home rail's Lists section reads these.
    pub fn smart_collections(&self) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, name, COALESCE(query_json, '{}') FROM collections
                 WHERE kind = 'smart' ORDER BY name COLLATE NOCASE",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| UfmError::Database(e.to_string()))?);
        }
        Ok(out)
    }

    /// The stored filter model for a smart collection, when it is one.

    pub fn smart_collection_query(&self, id: &CollectionId) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT query_json FROM collections WHERE id = ?1 AND kind = 'smart'")
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut rows = stmt
            .query_map(params![id.to_string()], |row| row.get::<_, Option<String>>(0))
            .map_err(|e| UfmError::Database(e.to_string()))?;
        match rows.next() {
            Some(r) => Ok(r.map_err(|e| UfmError::Database(e.to_string()))?),
            None => Ok(None),
        }
    }
}

/// One recorded mount of a volume (a path it has been seen at).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MountSighting {
    pub path: String,
    pub first_seen: String,
    pub last_seen: String,
}

/// A persistent per-volume record (Library Memory). Identity is the
/// filesystem UUID; labels and mount points are observations.
#[derive(Debug, Clone)]
pub struct VolumeRecord {
    pub fs_uuid: String,
    pub label: Option<String>,
    /// RFC 3339: first time this volume was ever observed.
    pub first_seen: String,
    /// RFC 3339: most recent time this volume was observed MOUNTED.
    pub last_seen: String,
    /// RFC 3339: most recent completed index of a root on this volume.
    pub last_indexed: Option<String>,
    pub mount_history: Vec<MountSighting>,
}

/// A persistent per-server record (Network Locations' Library
/// Memory). Identity is the normalized server URI; display names and
/// FUSE mount prefixes are observations, exactly like VolumeRecord.
#[derive(Debug, Clone)]
pub struct NetworkLocationRecord {
    pub uri: String,
    pub display_name: Option<String>,
    /// RFC 3339: first time this server was ever observed.
    pub first_seen: String,
    /// RFC 3339: most recent time this server was observed MOUNTED.
    pub last_seen: String,
    /// RFC 3339: most recent completed index of its mount.
    pub last_indexed: Option<String>,
    pub mount_history: Vec<MountSighting>,
}

/// Outcome of a terminal-job slimming pass (catalog hygiene).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SlimReport {
    pub jobs_slimmed: u64,
    pub bytes_freed: u64,
}

impl Catalog {
    /// Record that volume `fs_uuid` is mounted at `mount_point` at
    /// `at` (RFC 3339). Creates the record on first sight; refreshes
    /// label/last_seen afterwards and merges the mount point into
    /// the history (same path updates its last_seen, a new path is
    /// appended — identity never changes with the mount point).
    pub fn record_volume_sighting(
        &self,
        fs_uuid: &str,
        label: Option<&str>,
        mount_point: &str,
        at: &str,
    ) -> Result<()> {
        let existing = self.volume(fs_uuid)?;
        match existing {
            None => {
                let history = vec![MountSighting {
                    path: mount_point.to_string(),
                    first_seen: at.to_string(),
                    last_seen: at.to_string(),
                }];
                let history_json = serde_json::to_string(&history)
                    .map_err(|e| UfmError::Database(e.to_string()))?;
                self.conn
                    .execute(
                        "INSERT INTO volumes (fs_uuid, label, first_seen, last_seen, last_indexed, mount_history_json)
                         VALUES (?1, ?2, ?3, ?3, NULL, ?4)",
                        params![fs_uuid, label, at, history_json],
                    )
                    .map_err(|e| UfmError::Database(e.to_string()))?;
            }
            Some(mut rec) => {
                if label.is_some() {
                    rec.label = label.map(|s| s.to_string());
                }
                rec.last_seen = at.to_string();
                match rec.mount_history.iter_mut().find(|m| m.path == mount_point) {
                    Some(m) => m.last_seen = at.to_string(),
                    None => rec.mount_history.push(MountSighting {
                        path: mount_point.to_string(),
                        first_seen: at.to_string(),
                        last_seen: at.to_string(),
                    }),
                }
                let history_json = serde_json::to_string(&rec.mount_history)
                    .map_err(|e| UfmError::Database(e.to_string()))?;
                self.conn
                    .execute(
                        "UPDATE volumes SET label = ?2, last_seen = ?3, mount_history_json = ?4
                         WHERE fs_uuid = ?1",
                        params![fs_uuid, rec.label, rec.last_seen, history_json],
                    )
                    .map_err(|e| UfmError::Database(e.to_string()))?;
            }
        }
        Ok(())
    }

    /// Mark a volume as indexed at `at` (a completed index run over
    /// one of its roots). Also refreshes last_seen — it was mounted.
    pub fn mark_volume_indexed(&self, fs_uuid: &str, at: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE volumes SET last_indexed = ?2, last_seen = ?3 WHERE fs_uuid = ?1",
                params![fs_uuid, at, at],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// Record that the server `uri` is mounted at FUSE prefix
    /// `mount_prefix` at `at` (RFC 3339). Creates the record on first
    /// sight; refreshes display name/last_seen afterwards and merges
    /// the prefix into the history (same prefix updates its last_seen,
    /// a new prefix is appended — identity never changes with the
    /// mount point, exactly like volumes).
    pub fn record_network_sighting(
        &self,
        uri: &str,
        display_name: Option<&str>,
        mount_prefix: &str,
        at: &str,
    ) -> Result<()> {
        let existing = self.network_location(uri)?;
        match existing {
            None => {
                let history = vec![MountSighting {
                    path: mount_prefix.to_string(),
                    first_seen: at.to_string(),
                    last_seen: at.to_string(),
                }];
                let history_json = serde_json::to_string(&history)
                    .map_err(|e| UfmError::Database(e.to_string()))?;
                self.conn
                    .execute(
                        "INSERT INTO network_locations (uri, display_name, first_seen, last_seen, last_indexed, mount_history_json)
                         VALUES (?1, ?2, ?3, ?3, NULL, ?4)",
                        params![uri, display_name, at, history_json],
                    )
                    .map_err(|e| UfmError::Database(e.to_string()))?;
            }
            Some(mut rec) => {
                if display_name.is_some() {
                    rec.display_name = display_name.map(|s| s.to_string());
                }
                rec.last_seen = at.to_string();
                match rec.mount_history.iter_mut().find(|m| m.path == mount_prefix) {
                    Some(m) => m.last_seen = at.to_string(),
                    None => rec.mount_history.push(MountSighting {
                        path: mount_prefix.to_string(),
                        first_seen: at.to_string(),
                        last_seen: at.to_string(),
                    }),
                }
                let history_json = serde_json::to_string(&rec.mount_history)
                    .map_err(|e| UfmError::Database(e.to_string()))?;
                self.conn
                    .execute(
                        "UPDATE network_locations SET display_name = ?2, last_seen = ?3, mount_history_json = ?4
                         WHERE uri = ?1",
                        params![uri, rec.display_name, rec.last_seen, history_json],
                    )
                    .map_err(|e| UfmError::Database(e.to_string()))?;
            }
        }
        Ok(())
    }

    /// Mark a server as indexed at `at` (a completed index run over
    /// its mount). Also refreshes last_seen — it was mounted.
    pub fn mark_network_indexed(&self, uri: &str, at: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE network_locations SET last_indexed = ?2, last_seen = ?3 WHERE uri = ?1",
                params![uri, at, at],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn network_location(&self, uri: &str) -> Result<Option<NetworkLocationRecord>> {
        use rusqlite::OptionalExtension;
        let row = self
            .conn
            .query_row(
                "SELECT uri, display_name, first_seen, last_seen, last_indexed, mount_history_json
                 FROM network_locations WHERE uri = ?1",
                params![uri],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| UfmError::Database(e.to_string()))?;
        match row {
            None => Ok(None),
            Some((uri, display_name, first_seen, last_seen, last_indexed, history_json)) => {
                let mount_history: Vec<MountSighting> =
                    serde_json::from_str(&history_json).unwrap_or_default();
                Ok(Some(NetworkLocationRecord {
                    uri,
                    display_name,
                    first_seen,
                    last_seen,
                    last_indexed,
                    mount_history,
                }))
            }
        }
    }

    /// Every remembered server, most recently seen first.
    pub fn network_locations(&self) -> Result<Vec<NetworkLocationRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT uri FROM network_locations ORDER BY last_seen DESC")
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let ids = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| UfmError::Database(e.to_string()))?
            .collect::<std::result::Result<Vec<String>, _>>()
            .map_err(|e| UfmError::Database(e.to_string()))?;
        drop(stmt);
        let mut out = Vec::new();
        for id in ids {
            if let Some(v) = self.network_location(&id)? {
                out.push(v);
            }
        }
        Ok(out)
    }

    /// Remove a server record (Forget Server; its items must already
    /// be purged — see `purge_items_under_prefixes`).
    pub fn delete_network_location(&self, uri: &str) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM network_locations WHERE uri = ?1",
                params![uri],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn volume(&self, fs_uuid: &str) -> Result<Option<VolumeRecord>> {
        use rusqlite::OptionalExtension;
        let row = self
            .conn
            .query_row(
                "SELECT fs_uuid, label, first_seen, last_seen, last_indexed, mount_history_json
                 FROM volumes WHERE fs_uuid = ?1",
                params![fs_uuid],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| UfmError::Database(e.to_string()))?;
        match row {
            None => Ok(None),
            Some((fs_uuid, label, first_seen, last_seen, last_indexed, history_json)) => {
                let mount_history: Vec<MountSighting> =
                    serde_json::from_str(&history_json).unwrap_or_default();
                Ok(Some(VolumeRecord {
                    fs_uuid,
                    label,
                    first_seen,
                    last_seen,
                    last_indexed,
                    mount_history,
                }))
            }
        }
    }

    /// Every remembered volume, most recently seen first.
    pub fn volumes(&self) -> Result<Vec<VolumeRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT fs_uuid FROM volumes ORDER BY last_seen DESC")
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let ids = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| UfmError::Database(e.to_string()))?
            .collect::<std::result::Result<Vec<String>, _>>()
            .map_err(|e| UfmError::Database(e.to_string()))?;
        drop(stmt);
        let mut out = Vec::new();
        for id in ids {
            if let Some(v) = self.volume(&id)? {
                out.push(v);
            }
        }
        Ok(out)
    }

    /// Remove a volume record (its items must already be purged —
    /// see `purge_items_under_prefixes`).
    pub fn delete_volume(&self, fs_uuid: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM volumes WHERE fs_uuid = ?1", params![fs_uuid])
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// The incremental-index manifest for one location: provider_key
    /// -> (size_bytes, modified RFC 3339) for every stored item. The
    /// indexer diffs its walk against this; unchanged entries skip
    /// the catalog write entirely. The manifest is derived from the
    /// catalog itself — the source of truth — so no sidecar state
    /// can poison it.
    pub fn manifest_for_location(
        &self,
        location: &LocationId,
    ) -> Result<std::collections::HashMap<String, (Option<i64>, Option<String>)>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT provider_key, size_bytes, modified FROM items
                 WHERE location_id = ?1 AND provider_key IS NOT NULL",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![location.to_string()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut out = std::collections::HashMap::new();
        for row in rows {
            let (pk, size, modified) = row.map_err(|e| UfmError::Database(e.to_string()))?;
            out.insert(pk, (size, modified));
        }
        Ok(out)
    }

    /// SQL fragment + params matching items at or under any of
    /// `prefixes` (a prefix matches itself or prefix + "/..."). Uses
    /// substr comparison, never LIKE, so mount paths containing
    /// LIKE metacharacters stay literal.
    fn prefix_predicate(prefixes: &[String]) -> (String, Vec<rusqlite::types::Value>) {
        let mut parts = Vec::new();
        let mut values: Vec<rusqlite::types::Value> = Vec::new();
        for (i, p) in prefixes.iter().enumerate() {
            let a = i * 2 + 1;
            let b = i * 2 + 2;
            parts.push(format!(
                "(i.provider_key = ?{a} OR substr(i.provider_key, 1, length(?{b}) + 1) = ?{b} || '/')"
            ));
            values.push(rusqlite::types::Value::Text(p.clone()));
            values.push(rusqlite::types::Value::Text(p.clone()));
        }
        (parts.join(" OR "), values)
    }

    /// How many catalog items live at or under any of `prefixes`.
    pub fn item_count_under_prefixes(&self, prefixes: &[String]) -> Result<u64> {
        if prefixes.is_empty() {
            return Ok(0);
        }
        let (pred, values) = Self::prefix_predicate(prefixes);
        let sql = format!(
            "SELECT COUNT(*) FROM items AS i WHERE i.provider_key IS NOT NULL AND ({pred})"
        );
        let n: i64 = self
            .conn
            .query_row(&sql, rusqlite::params_from_iter(values.iter()), |r| r.get(0))
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(n as u64)
    }

    /// Delete every item at or under any of `prefixes`, together
    /// with everything that hangs off those items (FTS entries,
    /// labels, path labels, ROM-media cache rows; FK cascades take
    /// labels/collection members/media identities). This is the
    /// Forget / retention-prune / rebuild purge. Runs as one
    /// transaction: a crash mid-purge rolls back rather than leaving
    /// a half-forgotten volume. Returns the item count deleted.
    pub fn purge_items_under_prefixes(&self, prefixes: &[String]) -> Result<u64> {
        if prefixes.is_empty() {
            return Ok(0);
        }
        let (pred, values) = Self::prefix_predicate(prefixes);
        let select_sql = format!(
            "SELECT i.rowid, i.id, i.name, i.provider_key FROM items AS i
             WHERE i.provider_key IS NOT NULL AND ({pred})"
        );
        let mut stmt = self
            .conn
            .prepare(&select_sql)
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(values.iter()), |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut victims: Vec<(i64, String, String, String)> = Vec::new();
        for row in rows {
            victims.push(row.map_err(|e| UfmError::Database(e.to_string()))?);
        }
        drop(stmt);
        if victims.is_empty() {
            return Ok(0);
        }
        self.conn
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let work = (|| -> Result<()> {
            for (rowid, id, name, pk) in &victims {
                self.conn
                    .execute(
                        "INSERT INTO items_fts (items_fts, rowid, name) VALUES ('delete', ?1, ?2)",
                        params![rowid, name],
                    )
                    .map_err(|e| UfmError::Database(e.to_string()))?;
                self.conn
                    .execute(
                        "DELETE FROM rom_media WHERE item_id = ?1",
                        params![id],
                    )
                    .map_err(|e| UfmError::Database(e.to_string()))?;
                self.conn
                    .execute(
                        "DELETE FROM path_labels WHERE provider_key = ?1",
                        params![pk],
                    )
                    .map_err(|e| UfmError::Database(e.to_string()))?;
                self.conn
                    .execute("DELETE FROM items WHERE id = ?1", params![id])
                    .map_err(|e| UfmError::Database(e.to_string()))?;
            }
            Ok(())
        })();
        match work {
            Ok(()) => {
                self.conn
                    .execute_batch("COMMIT")
                    .map_err(|e| UfmError::Database(e.to_string()))?;
                Ok(victims.len() as u64)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// Catalog hygiene (Indexer v2): slim the snapshots of TERMINAL
    /// jobs (completed / cancelled / failed). A finished job never
    /// resumes, so its steps map — the replay payload, and by far
    /// the largest part of the blob — is dropped; title, state,
    /// created, preview (the summary), outcomes (the audit/undo
    /// feed) and the step tallies are kept, so every reader sees the
    /// same job, only smaller. Unfinished jobs are never touched:
    /// their steps are how they resume. Idempotent — slimmed jobs
    /// are skipped on later passes.
    pub fn slim_terminal_job_blobs(&self) -> Result<SlimReport> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT job_id, job_json FROM jobs
                 WHERE state IN ('completed', 'cancelled', 'failed')",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut pending: Vec<(String, String)> = Vec::new();
        for row in rows {
            pending.push(row.map_err(|e| UfmError::Database(e.to_string()))?);
        }
        drop(stmt);
        let mut report = SlimReport::default();
        for (job_id, old_json) in pending {
            // A blob that no longer parses is left exactly as found —
            // slimming must never become the thing that loses data.
            let mut job: ufm_jobs::Job = match serde_json::from_str(&old_json) {
                Ok(j) => j,
                Err(_) => continue,
            };
            if job.steps_slimmed || job.steps.is_empty() {
                continue;
            }
            job.step_counts = Some(ufm_jobs::StepCounts::tally(&job.steps));
            job.steps.clear();
            job.steps_slimmed = true;
            let new_json = match serde_json::to_string(&job) {
                Ok(j) => j,
                Err(_) => continue,
            };
            if new_json.len() < old_json.len() {
                self.conn
                    .execute(
                        "UPDATE jobs SET job_json = ?2 WHERE job_id = ?1",
                        params![job_id, new_json],
                    )
                    .map_err(|e| UfmError::Database(e.to_string()))?;
                report.jobs_slimmed += 1;
                report.bytes_freed += (old_json.len() - new_json.len()) as u64;
            }
        }
        Ok(report)
    }

    /// Reclaim free pages (after a material slimming or purge).
    pub fn vacuum(&self) -> Result<()> {
        self.conn
            .execute_batch("VACUUM")
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// Current database size in bytes (page_count × page_size) —
    /// the number VACUUM actually shrinks.
    pub fn database_size_bytes(&self) -> Result<u64> {
        let pages: i64 = self
            .conn
            .query_row("PRAGMA page_count", [], |r| r.get(0))
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let size: i64 = self
            .conn
            .query_row("PRAGMA page_size", [], |r| r.get(0))
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok((pages * size) as u64)
    }
}

// ---------------------------------------------------------------------------
// Rule (smart folder) result store — the durable memory of rule
// evaluations (Dickles' smart-folder amnesia fix, 2026-10-15).
//
// Before this, a rule view had NO memory: the only record of probing
// was items.duration_secs (which insert_item silently wiped on every
// re-index of a changed file), and the UI's view cache died with the
// process. Evaluation now lives here, in the catalog:
//
//   * rule_snapshot — the remembered result set. Renders instantly on
//     revisit; no probing, no rescan.
//   * rule_refresh — ONE delta pass: classify every candidate from
//     STORED durations only (zero probing), diffed against the stored
//     snapshot so unchanged rows are never rewritten. New files are
//     classified, changed files fall back to unprobed (their duration
//     described the old bytes), removed files leave the snapshot.
//   * rule_probe_batch — the duration sweep, in bounded batches.
//     Every probe persists the moment it lands, so a cancelled or
//     interrupted sweep resumes where it stopped instead of starting
//     over. Candidates on OFFLINE volumes are never probed (the
//     `online` predicate is Library Memory's mounted test): they
//     render from catalog data and join when the volume returns.
// ---------------------------------------------------------------------------

/// A duration rule (a smart folder definition): file extensions plus
/// an optional minimum duration in seconds. With no minimum, every
/// candidate is a member and nothing is ever probed.
#[derive(Debug, Clone, PartialEq)]
pub struct RuleSpec {
    pub extensions: Vec<String>,
    pub min_duration_secs: Option<f64>,
}

/// The durable identity of a rule: sorted lowercase extensions plus
/// the duration floor. Two rules that ask the same question share
/// one stored result.
pub fn rule_key(spec: &RuleSpec) -> String {
    let mut exts: Vec<String> = spec
        .extensions
        .iter()
        .map(|e| e.trim().trim_start_matches('.').to_lowercase())
        .filter(|e| !e.is_empty())
        .collect();
    exts.sort();
    exts.dedup();
    format!(
        "{}|{}",
        exts.join(","),
        spec.min_duration_secs
            .map(|m| m.to_string())
            .unwrap_or_default()
    )
}

/// Outcome of probing one file for its duration. Only outcomes for
/// files that exist and were actually probed settle anything.
#[derive(Debug, Clone, PartialEq)]
pub enum ProbeOutcome {
    /// Probed: the media duration in seconds.
    Duration(f64),
    /// Probed: no readable duration (not media, corrupt, …). Settles
    /// as duration 0.0 — the file was examined; don't ask again
    /// until its bytes change.
    NoDuration,
    /// The path vanished before probing (unmounted mid-sweep,
    /// deleted). NEVER settles — recording this as zero-length was
    /// the old poisoning bug.
    Missing,
    /// Probing itself is unavailable (the probe tool failed to
    /// start). The batch stops and reports it; nothing settles.
    Unavailable(String),
}

/// The durable state of one rule's evaluation.
#[derive(Debug, Clone)]
pub struct RuleStatus {
    /// Member items, hydrated from the catalog (labels included),
    /// in name order. On a snapshot this is the remembered set.
    pub members: Vec<ufm_vfs::Item>,
    pub member_count: u32,
    /// Total candidates the last evaluation saw.
    pub candidates: u32,
    /// Unprobed candidates on mounted volumes — the sweep's
    /// remaining work.
    pub pending: u32,
    /// Unprobed candidates on OFFLINE volumes. Never probed; they
    /// join automatically when the volume returns.
    pub pending_offline: u32,
    /// True when no online probing remains.
    pub sweep_complete: bool,
    /// When the last full/delta evaluation ran (RFC 3339).
    pub evaluated_at: Option<String>,
}

/// One bounded slice of the duration sweep.
#[derive(Debug, Clone)]
pub struct RuleProbeBatch {
    /// Files actually probed in this batch (settled or not).
    pub probed: u32,
    /// Unprobed candidates remaining on mounted volumes.
    pub remaining: u32,
    /// Unprobed candidates on offline volumes (never probed).
    pub remaining_offline: u32,
    /// Files this batch newly proved to be members — the UI merges
    /// them into the visible list live.
    pub new_members: Vec<ufm_vfs::Item>,
    /// The batch stopped early because cancellation was requested.
    /// Everything probed so far is persisted.
    pub cancelled: bool,
    /// Probing is unavailable (see ProbeOutcome::Unavailable).
    pub probe_error: Option<String>,
}

#[derive(Debug, Clone)]
struct RuleHeader {
    candidates: u32,
    pending: u32,
    pending_offline: u32,
    sweep_complete: bool,
    evaluated_at: String,
}

/// Membership from a stored duration: `None` = undecided (not
/// probed yet). A stored 0.0 is a settled "no duration" and fails
/// every positive floor.
fn rule_is_member(min_duration_secs: Option<f64>, dur: Option<f64>) -> Option<bool> {
    match min_duration_secs {
        None => Some(true),
        Some(min) => dur.map(|d| d > min),
    }
}

impl Catalog {
    fn rule_header(&self, key: &str) -> Result<Option<RuleHeader>> {
        let row = self
            .conn
            .query_row(
                "SELECT candidates, pending, pending_offline, sweep_complete, evaluated_at
                 FROM rule_results WHERE rule_key = ?1",
                params![key],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                },
            )
            .ok();
        Ok(row.map(|(c, p, po, sc, at)| RuleHeader {
            candidates: c.max(0) as u32,
            pending: p.max(0) as u32,
            pending_offline: po.max(0) as u32,
            sweep_complete: sc != 0,
            evaluated_at: at,
        }))
    }

    /// Rule candidates with their stored durations, WITHOUT label
    /// hydration (the sweep classifies thousands of rows; labels are
    /// only paid for members, at hydration time).
    fn rule_candidates_bare(
        &self,
        exts: &[String],
    ) -> Result<Vec<(ufm_vfs::Item, Option<f64>)>> {
        if exts.is_empty() {
            return Ok(Vec::new());
        }
        let conds: Vec<String> = exts
            .iter()
            .map(|_| "lower(i.name) LIKE '%.' || ?".to_string())
            .collect();
        let sql = format!(
            "SELECT i.id, i.location_id, i.name, i.kind, i.size_bytes,
                    i.modified, i.provider_key, i.duration_secs
             FROM items AS i
             WHERE ({})
             ORDER BY i.name COLLATE NOCASE",
            conds.join(" OR ")
        );
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(exts.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<f64>>(7)?,
                ))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, location_id, name, kind_s, size_bytes, modified_s, provider_key, duration) =
                row.map_err(|e| UfmError::Database(e.to_string()))?;
            let item = self.hydrate_item(
                &id,
                &location_id,
                name,
                &kind_s,
                size_bytes,
                modified_s,
                provider_key,
            )?;
            out.push((item, duration));
        }
        Ok(out)
    }

    /// The stored member items of a rule, fully hydrated.
    fn rule_member_items(&self, key: &str) -> Result<Vec<ufm_vfs::Item>> {
        self.items_by_query(
            "SELECT i.id, i.location_id, i.name, i.kind, i.size_bytes,
                    i.modified, i.provider_key
             FROM rule_result_items AS r
             JOIN items AS i ON i.provider_key = r.provider_key
             WHERE r.rule_key = ?1 AND r.is_member = 1
             ORDER BY i.name COLLATE NOCASE",
            key,
        )
    }

    /// Fully-hydrated items for an explicit provider-key list (the
    /// probe batch's new members).
    fn rule_items_by_provider_keys(
        &self,
        keys: &[String],
    ) -> Result<Vec<ufm_vfs::Item>> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders: Vec<String> = keys.iter().map(|_| "?".to_string()).collect();
        let sql = format!(
            "SELECT i.id, i.location_id, i.name, i.kind, i.size_bytes,
                    i.modified, i.provider_key
             FROM items AS i
             WHERE i.provider_key IN ({})
             ORDER BY i.name COLLATE NOCASE",
            placeholders.join(", ")
        );
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(keys.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, location_id, name, kind_s, size_bytes, modified_s, provider_key) =
                row.map_err(|e| UfmError::Database(e.to_string()))?;
            out.push(self.hydrate_item(
                &id,
                &location_id,
                name,
                &kind_s,
                size_bytes,
                modified_s,
                provider_key,
            )?);
        }
        Ok(out)
    }

    /// The remembered result for a rule, if it has ever been
    /// evaluated. Pure reads: no probing, no candidate rescan.
    pub fn rule_snapshot(&self, spec: &RuleSpec) -> Result<Option<RuleStatus>> {
        let key = rule_key(spec);
        let Some(header) = self.rule_header(&key)? else {
            return Ok(None);
        };
        let members = self.rule_member_items(&key)?;
        Ok(Some(RuleStatus {
            member_count: members.len() as u32,
            candidates: header.candidates,
            pending: header.pending,
            pending_offline: header.pending_offline,
            sweep_complete: header.sweep_complete,
            evaluated_at: Some(header.evaluated_at),
            members,
        }))
    }

    /// One delta evaluation of a rule: classify every candidate from
    /// stored durations ONLY (zero probing), diff against the stored
    /// snapshot, and persist the new snapshot. Unchanged snapshot
    /// rows are not rewritten — a steady-state refresh writes only
    /// the header. `online` decides probe eligibility per path
    /// (Library Memory's mounted test); offline candidates are
    /// counted apart and never block completion of the online set.
    pub fn rule_refresh(
        &self,
        spec: &RuleSpec,
        online: &dyn Fn(&str) -> bool,
        now: &str,
    ) -> Result<RuleStatus> {
        let key = rule_key(spec);
        let candidates = self.rule_candidates_bare(&spec.extensions)?;
        // Previous snapshot rows: provider_key -> (is_member, size, modified).
        let mut prev: std::collections::HashMap<
            String,
            (bool, Option<i64>, Option<String>),
        > = std::collections::HashMap::new();
        {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT provider_key, is_member, size_bytes, modified
                     FROM rule_result_items WHERE rule_key = ?1",
                )
                .map_err(|e| UfmError::Database(e.to_string()))?;
            let rows = stmt
                .query_map(params![key], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                })
                .map_err(|e| UfmError::Database(e.to_string()))?;
            for row in rows {
                let (pk, is_member, size, modified) =
                    row.map_err(|e| UfmError::Database(e.to_string()))?;
                prev.insert(pk, (is_member != 0, size, modified));
            }
        }
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut total = 0u32;
        let mut member_count = 0u32;
        let mut pending = 0u32;
        let mut pending_offline = 0u32;
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| UfmError::Database(e.to_string()))?;
        for (item, dur) in &candidates {
            let Some(pk) = &item.provider_key else {
                continue;
            };
            total += 1;
            seen.insert(pk.clone());
            let member = match rule_is_member(spec.min_duration_secs, *dur) {
                Some(m) => m,
                None => {
                    // Unprobed: not a member YET. Which pending pile
                    // it lands in depends on its volume being here.
                    if online(pk) {
                        pending += 1;
                    } else {
                        pending_offline += 1;
                    }
                    false
                }
            };
            if member {
                member_count += 1;
            }
            let size = item.size_bytes.map(|s| s as i64);
            let modified = item.modified.map(|m| m.to_rfc3339());
            let stale = match prev.get(pk) {
                None => true,
                Some((m, s, md)) => *m != member || *s != size || *md != modified,
            };
            if stale {
                tx.execute(
                    "INSERT OR REPLACE INTO rule_result_items
                     (rule_key, provider_key, is_member, size_bytes, modified)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![key, pk, member as i64, size, modified],
                )
                .map_err(|e| UfmError::Database(e.to_string()))?;
            }
        }
        // Candidates that vanished (deleted files) leave the snapshot.
        for pk in prev.keys() {
            if !seen.contains(pk) {
                tx.execute(
                    "DELETE FROM rule_result_items
                     WHERE rule_key = ?1 AND provider_key = ?2",
                    params![key, pk],
                )
                .map_err(|e| UfmError::Database(e.to_string()))?;
            }
        }
        tx.execute(
            "INSERT OR REPLACE INTO rule_results
             (rule_key, extensions, min_duration_secs, evaluated_at,
              candidates, member_count, pending, pending_offline, sweep_complete)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                key,
                spec.extensions.join(","),
                spec.min_duration_secs,
                now,
                total as i64,
                member_count as i64,
                pending as i64,
                pending_offline as i64,
                (pending == 0) as i64,
            ],
        )
        .map_err(|e| UfmError::Database(e.to_string()))?;
        tx.commit()
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let members = self.rule_member_items(&key)?;
        Ok(RuleStatus {
            members,
            member_count,
            candidates: total,
            pending,
            pending_offline,
            sweep_complete: pending == 0,
            evaluated_at: Some(now.to_string()),
        })
    }

    /// Probe up to `budget` unprobed, ONLINE candidates of a rule, in
    /// stable (name) order, persisting every settled duration the
    /// moment it lands. Checks `cancel` between files; a cancelled or
    /// interrupted batch leaves all partial progress in the catalog,
    /// so the next batch resumes instead of restarting. Offline
    /// candidates are counted, never probed. A rule with no duration
    /// floor never probes at all.
    pub fn rule_probe_batch(
        &self,
        spec: &RuleSpec,
        budget: u32,
        probe: &mut dyn FnMut(&std::path::Path) -> ProbeOutcome,
        online: &dyn Fn(&str) -> bool,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<RuleProbeBatch> {
        let mut batch = RuleProbeBatch {
            probed: 0,
            remaining: 0,
            remaining_offline: 0,
            new_members: Vec::new(),
            cancelled: false,
            probe_error: None,
        };
        let Some(min) = spec.min_duration_secs else {
            return Ok(batch);
        };
        let key = rule_key(spec);
        let has_snapshot = self.rule_header(&key)?.is_some();
        let candidates = self.rule_candidates_bare(&spec.extensions)?;
        let mut new_member_keys: Vec<String> = Vec::new();
        for (item, dur) in &candidates {
            if dur.is_some() {
                continue; // settled already — never re-probed
            }
            let Some(pk) = &item.provider_key else {
                continue;
            };
            if !online(pk) {
                batch.remaining_offline += 1;
                continue;
            }
            if batch.probed >= budget || batch.probe_error.is_some() {
                batch.remaining += 1;
                continue;
            }
            if cancel.load(std::sync::atomic::Ordering::SeqCst) {
                batch.cancelled = true;
                batch.remaining += 1;
                continue;
            }
            batch.probed += 1;
            match probe(std::path::Path::new(pk)) {
                ProbeOutcome::Duration(d) => {
                    self.set_duration(pk, d)?;
                    let member = d > min;
                    if member {
                        new_member_keys.push(pk.clone());
                    }
                    if has_snapshot {
                        self.rule_note_probe_result(&key, item, member)?;
                    }
                }
                ProbeOutcome::NoDuration => {
                    self.set_duration(pk, 0.0)?;
                    if has_snapshot {
                        self.rule_note_probe_result(&key, item, false)?;
                    }
                }
                ProbeOutcome::Missing => {
                    // Vanished between listing and probing: settles
                    // NOTHING. It stays pending and honest.
                    batch.remaining += 1;
                }
                ProbeOutcome::Unavailable(msg) => {
                    batch.probe_error = Some(msg);
                    batch.remaining += 1;
                }
            }
        }
        if !new_member_keys.is_empty() {
            batch.new_members = self.rule_items_by_provider_keys(&new_member_keys)?;
        }
        if has_snapshot {
            self.conn
                .execute(
                    "UPDATE rule_results
                     SET pending = ?2, pending_offline = ?3,
                         member_count = member_count + ?4,
                         sweep_complete = ?5
                     WHERE rule_key = ?1",
                    params![
                        key,
                        batch.remaining as i64,
                        batch.remaining_offline as i64,
                        new_member_keys.len() as i64,
                        (batch.remaining == 0) as i64,
                    ],
                )
                .map_err(|e| UfmError::Database(e.to_string()))?;
        }
        Ok(batch)
    }

    /// Record one settled probe outcome in the rule snapshot.
    fn rule_note_probe_result(
        &self,
        key: &str,
        item: &ufm_vfs::Item,
        member: bool,
    ) -> Result<()> {
        let Some(pk) = &item.provider_key else {
            return Ok(());
        };
        self.conn
            .execute(
                "INSERT OR REPLACE INTO rule_result_items
                 (rule_key, provider_key, is_member, size_bytes, modified)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    key,
                    pk,
                    member as i64,
                    item.size_bytes.map(|s| s as i64),
                    item.modified.map(|m| m.to_rfc3339()),
                ],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }
}

// ------------------------------------------------------------------
// Ebook P1 — "The Library" (2026-10-17): book identity, shelves,
// reading states, facets. See the book_metadata schema comment in
// migrate() for the storage doctrine.
// ------------------------------------------------------------------

/// One book's decoded metadata as stored in `book_metadata` +
/// `book_authors`. `authors[0]` is the primary author (the first
/// credited) — Author shelves group by it; the author: facet and
/// `items_by_book_author` match ANY credited author exactly.
#[derive(Debug, Clone, PartialEq)]
pub struct BookMetadataRow {
    pub item_id: String,
    pub provider_key: String,
    /// epub | pdf
    pub format: String,
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub series: Option<String>,
    pub series_index: Option<f64>,
    pub publisher: Option<String>,
    pub published_date: Option<String>,
    pub pub_year: Option<i64>,
    pub language: Option<String>,
    pub description: Option<String>,
    pub subjects: Vec<String>,
    pub isbn: Option<String>,
    pub identifier: Option<String>,
    pub spine_count: Option<i64>,
    pub page_count: Option<i64>,
    pub cover_path: Option<String>,
    /// opf | pdfinfo
    pub source: String,
    /// Freshness snapshot of the item at identify time (the ES
    /// contract): a later size/mtime change makes the book a
    /// re-identify candidate again.
    pub size_bytes: Option<i64>,
    pub modified: Option<String>,
    pub identified_at: String,
}

/// One identified book as the shelves render it.
#[derive(Debug, Clone)]
pub struct BookShelfRow {
    pub item: ufm_vfs::Item,
    pub meta: BookMetadataRow,
    /// want | reading | read, when a state is recorded.
    pub state: Option<String>,
}

/// One series on the Series shelf: its books in reading order and
/// the honest whole-number gaps in 1..=max(present).
#[derive(Debug, Clone)]
pub struct SeriesShelf {
    pub series: String,
    pub books: Vec<BookShelfRow>,
    pub present: Vec<i64>,
    pub missing: Vec<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct BookCounts {
    pub identified: u64,
    pub unsorted: u64,
    pub series: u64,
    pub authors: u64,
    pub want: u64,
    pub reading: u64,
    pub read: u64,
}

/// Whole-number series positions in 1..=max(present whole positions)
/// that no book holds. Fractional indices (0.5 novellas) neither
/// create nor fill a whole-number gap; a series holding only book 2
/// honestly reports book 1 missing.
pub fn series_missing_positions(present_indices: &[f64]) -> Vec<i64> {
    let whole: Vec<i64> = present_indices
        .iter()
        .filter(|v| v.fract() == 0.0 && **v >= 1.0)
        .map(|v| *v as i64)
        .collect();
    let Some(max) = whole.iter().max() else {
        return Vec::new();
    };
    (1..=*max).filter(|n| !whole.contains(n)).collect()
}

impl Catalog {
    /// Store one book's decoded metadata + authors + its
    /// `media_identities` row (media_kind 'ebook' — the persisted
    /// assignment of MediaKind::Ebook), in one transaction. The
    /// identity's evidence JSON is built here from the row itself so
    /// the two can never disagree.
    pub fn upsert_book_metadata(&self, row: &BookMetadataRow) -> Result<()> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| UfmError::Database(e.to_string()))?;
        tx.execute(
            "INSERT OR REPLACE INTO book_metadata
             (item_id, provider_key, format, title, series, series_index,
              publisher, published_date, pub_year, language, description,
              subjects_json, isbn, identifier, spine_count, page_count,
              cover_path, source, size_bytes, modified, identified_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                     ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
            params![
                row.item_id,
                row.provider_key,
                row.format,
                row.title,
                row.series,
                row.series_index,
                row.publisher,
                row.published_date,
                row.pub_year,
                row.language,
                row.description,
                serde_json::to_string(&row.subjects).unwrap_or_else(|_| "[]".into()),
                row.isbn,
                row.identifier,
                row.spine_count,
                row.page_count,
                row.cover_path,
                row.source,
                row.size_bytes,
                row.modified,
                row.identified_at,
            ],
        )
        .map_err(|e| UfmError::Database(e.to_string()))?;
        tx.execute(
            "DELETE FROM book_authors WHERE item_id = ?1",
            params![row.item_id],
        )
        .map_err(|e| UfmError::Database(e.to_string()))?;
        for (pos, author) in row.authors.iter().enumerate() {
            tx.execute(
                "INSERT INTO book_authors (item_id, author, position) VALUES (?1, ?2, ?3)",
                params![row.item_id, author, pos as i64],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        }
        let evidence = serde_json::json!({
            "source": row.source,
            "isbn": row.isbn,
            "identifier": row.identifier,
            "series": row.series,
            "series_index": row.series_index,
        })
        .to_string();
        tx.execute(
            "INSERT OR REPLACE INTO media_identities
             (item_id, media_kind, title, year, tmdb_id, platform, sha1,
              match_evidence_json, season, episode, episode_end, confidence, confirmed)
             VALUES (?1, 'ebook', ?2, ?3, NULL, ?4, NULL, ?5, NULL, NULL, NULL, NULL, 0)",
            params![row.item_id, row.title, row.pub_year, row.format, evidence],
        )
        .map_err(|e| UfmError::Database(e.to_string()))?;
        tx.commit().map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// One book's stored metadata by item id (authors re-read from
    /// `book_authors` in credited order).
    pub fn book_metadata_for(&self, item_id: &str) -> Result<Option<BookMetadataRow>> {
        let mut rows = self.book_shelf_rows_inner(Some(item_id))?;
        let first = rows.drain(..).next();
        Ok(first.map(|r| r.meta))
    }

    /// Run an item-columns query with an optional single parameter.
    fn query_items(&self, sql: &str, param: Option<&str>) -> Result<Vec<ufm_vfs::Item>> {
        let mut stmt = self
            .conn
            .prepare(sql)
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let map_row = |row: &rusqlite::Row<'_>| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        };
        let raw = match param {
            Some(p) => stmt.query_map(params![p], map_row),
            None => stmt.query_map([], map_row),
        }
        .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut items = Vec::new();
        for row in raw {
            let (id, loc, name, kind_s, size, modified, pk) =
                row.map_err(|e| UfmError::Database(e.to_string()))?;
            items.push(self.hydrate_item(&id, &loc, name, &kind_s, size, modified, pk)?);
        }
        Ok(items)
    }

    /// Withdraw a book identification (2026-10-18, crew Dewey):
    /// the file failed content validation on a re-judge, so its
    /// derived metadata goes away — the book_metadata row, its
    /// authors, and the 'ebook' media identity the upsert wrote.
    /// The ITEM row stays (the file is still a plain file in its
    /// location) and reading state is untouched: book_state is
    /// keyed by provider_key and is the reader's own record, not
    /// identification output. Returns whether a row existed.
    pub fn delete_book_metadata(&self, item_id: &str) -> Result<bool> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let n = tx
            .execute(
                "DELETE FROM book_metadata WHERE item_id = ?1",
                params![item_id],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        tx.execute(
            "DELETE FROM book_authors WHERE item_id = ?1",
            params![item_id],
        )
        .map_err(|e| UfmError::Database(e.to_string()))?;
        tx.execute(
            "DELETE FROM media_identities WHERE item_id = ?1 AND media_kind = 'ebook'",
            params![item_id],
        )
        .map_err(|e| UfmError::Database(e.to_string()))?;
        tx.commit().map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(n > 0)
    }

    /// Book files (EPUB/PDF by name) that still owe an identify:
    /// no metadata row, or the file's size/mtime moved since the
    /// stored snapshot. Optionally scoped to one location (the
    /// post-index hook identifies only what the walk just touched).
    pub fn book_candidates(&self, location_id: Option<&str>) -> Result<Vec<ufm_vfs::Item>> {
        // The pdf/page_count clause (2026-10-18, crew Dewey): a PDF
        // that passed content validation ALWAYS has a page count,
        // so a stored PDF row without one was shelved by the old
        // extension-trusting rules (the paper-100k fixture) and
        // owes a re-judge under the content gate — after which it
        // either gains a real page count or its row is withdrawn.
        // EPUB rows legitimately carry page_count NULL (they store
        // spine_count instead), so the clause names format 'pdf'.
        let base = "SELECT i.id, i.location_id, i.name, i.kind, i.size_bytes, i.modified, i.provider_key
             FROM items i LEFT JOIN book_metadata bm ON bm.item_id = i.id
             WHERE i.provider_key IS NOT NULL
               AND (lower(i.name) LIKE '%.epub' OR lower(i.name) LIKE '%.pdf')
               AND (bm.item_id IS NULL
                    OR bm.size_bytes IS NOT i.size_bytes
                    OR bm.modified IS NOT i.modified
                    OR (bm.format = 'pdf' AND bm.page_count IS NULL))";
        match location_id {
            Some(loc) => self.query_items(&format!("{base} AND i.location_id = ?1 ORDER BY i.name"), Some(loc)),
            None => self.query_items(&format!("{base} ORDER BY i.name"), None),
        }
    }

    /// Book files with NO metadata row at all — the Unsorted pile.
    /// (A book whose file yielded nothing still gets a row —
    /// source 'filename' — so unsorted always means "not yet
    /// identified", never "identified but thin".)
    pub fn book_unsorted_items(&self) -> Result<Vec<ufm_vfs::Item>> {
        // Books-fix (2026-10-18): the room renders at most 60 of
        // these; the SQL cap keeps a million-row library from
        // materializing the whole unsorted pile into memory on
        // every room paint. The honest total still comes from
        // books_counts(). (The scan itself is priced in the
        // incident diagnosis: an unindexed suffix LIKE over the
        // items table — bounded here, and the caller now composes
        // off the UI thread.)
        self.query_items(
            "SELECT i.id, i.location_id, i.name, i.kind, i.size_bytes, i.modified, i.provider_key
             FROM items i LEFT JOIN book_metadata bm ON bm.item_id = i.id
             WHERE bm.item_id IS NULL AND i.provider_key IS NOT NULL
               AND (lower(i.name) LIKE '%.epub' OR lower(i.name) LIKE '%.pdf')
             ORDER BY i.name
             LIMIT 240",
            None,
        )
    }

    /// The sidebar door count: identified books only. One indexed
    /// COUNT — NOT the full books payload (the door once paid for
    /// the whole room compose, unsorted scan included, just to
    /// render a badge number; Books incident, 2026-10-18).
    pub fn books_identified_count(&self) -> Result<u64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM book_metadata", [], |r| {
                r.get::<_, i64>(0)
            })
            .map(|c| c as u64)
            .map_err(|e| UfmError::Database(e.to_string()))
    }

    /// Write batching for the indexer's private catalog instance:
    /// BEGIN IMMEDIATE … COMMIT around a run of insert_item calls
    /// so a census pays one commit fsync per batch instead of one
    /// per file. Same execute_batch shape as the purge path below.
    /// Only safe where the caller owns the connection outright
    /// (the Indexer does); app-side catalogs never batch.
    pub fn begin_write_batch(&self) -> Result<()> {
        self.conn
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| UfmError::Database(e.to_string()))
    }

    /// Commit a batch opened by [`Catalog::begin_write_batch`].
    pub fn commit_write_batch(&self) -> Result<()> {
        self.conn
            .execute_batch("COMMIT")
            .map_err(|e| UfmError::Database(e.to_string()))
    }

    /// Best-effort rollback of an open batch (error paths; the
    /// connection's close would roll back anyway, this just keeps
    /// the state honest if the instance lives on).
    pub fn rollback_write_batch(&self) {
        let _ = self.conn.execute_batch("ROLLBACK");
    }

    fn book_shelf_rows_inner(&self, only_item: Option<&str>) -> Result<Vec<BookShelfRow>> {
        let sql = "SELECT i.id, i.location_id, i.name, i.kind, i.size_bytes, i.modified, i.provider_key,
                    bm.format, bm.title, bm.series, bm.series_index, bm.publisher,
                    bm.published_date, bm.pub_year, bm.language, bm.description,
                    bm.subjects_json, bm.isbn, bm.identifier, bm.spine_count,
                    bm.page_count, bm.cover_path, bm.source, bm.identified_at, bs.state
             FROM items i
             JOIN book_metadata bm ON bm.item_id = i.id
             LEFT JOIN book_state bs ON bs.provider_key = i.provider_key";
        let sql = match only_item {
            Some(_) => format!("{sql} WHERE i.id = ?1"),
            None => format!("{sql} ORDER BY lower(COALESCE(bm.title, i.name)), i.name"),
        };
        // Authors for every shelved book, in one pass.
        let mut author_map: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT item_id, author FROM book_authors ORDER BY item_id, position")
                .map_err(|e| UfmError::Database(e.to_string()))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| UfmError::Database(e.to_string()))?;
            for row in rows {
                let (item_id, author) = row.map_err(|e| UfmError::Database(e.to_string()))?;
                author_map.entry(item_id).or_default().push(author);
            }
        }
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let map_row = |row: &rusqlite::Row<'_>| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<f64>>(10)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, Option<String>>(12)?,
                row.get::<_, Option<i64>>(13)?,
                row.get::<_, Option<String>>(14)?,
                row.get::<_, Option<String>>(15)?,
                row.get::<_, String>(16)?,
                row.get::<_, Option<String>>(17)?,
                row.get::<_, Option<String>>(18)?,
                row.get::<_, Option<i64>>(19)?,
                row.get::<_, Option<i64>>(20)?,
                row.get::<_, Option<String>>(21)?,
                row.get::<_, String>(22)?,
                row.get::<_, String>(23)?,
                row.get::<_, Option<String>>(24)?,
            ))
        };
        let raw = match only_item {
            Some(id) => stmt.query_map(params![id], map_row),
            None => stmt.query_map([], map_row),
        }
        .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut out = Vec::new();
        for row in raw {
            let (
                id,
                loc,
                name,
                kind_s,
                size,
                modified,
                pk,
                format,
                title,
                series,
                series_index,
                publisher,
                published_date,
                pub_year,
                language,
                description,
                subjects_json,
                isbn,
                identifier,
                spine_count,
                page_count,
                cover_path,
                source,
                identified_at,
                state,
            ) = row.map_err(|e| UfmError::Database(e.to_string()))?;
            let item = self.hydrate_item(&id, &loc, name, &kind_s, size, modified.clone(), pk.clone())?;
            let meta = BookMetadataRow {
                item_id: id.clone(),
                provider_key: pk.unwrap_or_default(),
                format,
                title,
                authors: author_map.get(&id).cloned().unwrap_or_default(),
                series,
                series_index,
                publisher,
                published_date,
                pub_year,
                language,
                description,
                subjects: serde_json::from_str(&subjects_json).unwrap_or_default(),
                isbn,
                identifier,
                spine_count,
                page_count,
                cover_path,
                source,
                size_bytes: size,
                modified,
                identified_at,
            };
            out.push(BookShelfRow { item, meta, state });
        }
        Ok(out)
    }

    /// Every identified book, title-ordered — the All Books shelf.
    pub fn books_all(&self) -> Result<Vec<BookShelfRow>> {
        self.book_shelf_rows_inner(None)
    }

    /// Series shelves: books grouped by series name, in reading
    /// order (series index, unnumbered last, then title), with the
    /// honest gap list computed over whole-number positions.
    pub fn books_series_shelves(&self) -> Result<Vec<SeriesShelf>> {
        let rows = self.book_shelf_rows_inner(None)?;
        let mut by_series: std::collections::BTreeMap<String, Vec<BookShelfRow>> =
            std::collections::BTreeMap::new();
        for row in rows {
            if let Some(series) = &row.meta.series {
                by_series.entry(series.clone()).or_default().push(row);
            }
        }
        let mut shelves: Vec<SeriesShelf> = by_series
            .into_iter()
            .map(|(series, mut books)| {
                books.sort_by(|a, b| {
                    let ai = a.meta.series_index;
                    let bi = b.meta.series_index;
                    ai.is_none()
                        .cmp(&bi.is_none())
                        .then(ai.partial_cmp(&bi).unwrap_or(std::cmp::Ordering::Equal))
                        .then(
                            a.meta
                                .title
                                .clone()
                                .unwrap_or_default()
                                .to_lowercase()
                                .cmp(&b.meta.title.clone().unwrap_or_default().to_lowercase()),
                        )
                });
                let indices: Vec<f64> = books.iter().filter_map(|b| b.meta.series_index).collect();
                let present: Vec<i64> = {
                    let mut whole: Vec<i64> = indices
                        .iter()
                        .filter(|v| v.fract() == 0.0 && **v >= 1.0)
                        .map(|v| *v as i64)
                        .collect();
                    whole.sort_unstable();
                    whole.dedup();
                    whole
                };
                SeriesShelf {
                    series,
                    missing: series_missing_positions(&indices),
                    present,
                    books,
                }
            })
            .collect();
        shelves.sort_by(|a, b| a.series.to_lowercase().cmp(&b.series.to_lowercase()));
        Ok(shelves)
    }

    /// Author shelves: (primary author, their books) — books sorted
    /// by series then title so an author's series reads in order.
    pub fn books_author_shelves(&self) -> Result<Vec<(String, Vec<BookShelfRow>)>> {
        let rows = self.book_shelf_rows_inner(None)?;
        let mut by_author: std::collections::BTreeMap<String, Vec<BookShelfRow>> =
            std::collections::BTreeMap::new();
        for row in rows {
            if let Some(primary) = row.meta.authors.first() {
                by_author.entry(primary.clone()).or_default().push(row);
            }
        }
        let mut shelves: Vec<(String, Vec<BookShelfRow>)> = by_author.into_iter().collect();
        for (_, books) in &mut shelves {
            books.sort_by(|a, b| {
                a.meta
                    .series
                    .clone()
                    .unwrap_or_default()
                    .to_lowercase()
                    .cmp(&b.meta.series.clone().unwrap_or_default().to_lowercase())
                    .then(
                        a.meta
                            .series_index
                            .partial_cmp(&b.meta.series_index)
                            .unwrap_or(std::cmp::Ordering::Equal),
                    )
                    .then(
                        a.meta
                            .title
                            .clone()
                            .unwrap_or_default()
                            .to_lowercase()
                            .cmp(&b.meta.title.clone().unwrap_or_default().to_lowercase()),
                    )
            });
        }
        shelves.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
        Ok(shelves)
    }

    pub fn books_counts(&self) -> Result<BookCounts> {
        let count = |sql: &str| -> Result<u64> {
            self.conn
                .query_row(sql, [], |r| r.get::<_, i64>(0))
                .map(|c| c as u64)
                .map_err(|e| UfmError::Database(e.to_string()))
        };
        let state_count = |state: &str| -> Result<u64> {
            self.conn
                .query_row(
                    "SELECT COUNT(*) FROM book_state WHERE state = ?1",
                    params![state],
                    |r| r.get::<_, i64>(0),
                )
                .map(|c| c as u64)
                .map_err(|e| UfmError::Database(e.to_string()))
        };
        Ok(BookCounts {
            identified: count("SELECT COUNT(*) FROM book_metadata")?,
            unsorted: count(
                "SELECT COUNT(*) FROM items i LEFT JOIN book_metadata bm ON bm.item_id = i.id
                 WHERE bm.item_id IS NULL AND i.provider_key IS NOT NULL
                   AND (lower(i.name) LIKE '%.epub' OR lower(i.name) LIKE '%.pdf')",
            )?,
            series: count("SELECT COUNT(DISTINCT series) FROM book_metadata WHERE series IS NOT NULL")?,
            authors: count("SELECT COUNT(DISTINCT author) FROM book_authors")?,
            want: state_count("want")?,
            reading: state_count("reading")?,
            read: state_count("read")?,
        })
    }

    // -- Reading states ---------------------------------------------

    /// (state, origin, updated_at) for one book path.
    pub fn book_state_for(&self, provider_key: &str) -> Result<Option<(String, String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT state, origin, updated_at FROM book_state WHERE provider_key = ?1")
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut rows = stmt
            .query_map(params![provider_key], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        match rows.next() {
            Some(r) => Ok(Some(r.map_err(|e| UfmError::Database(e.to_string()))?)),
            None => Ok(None),
        }
    }

    pub fn set_book_state(
        &self,
        provider_key: &str,
        state: &str,
        origin: &str,
        updated_at: &str,
    ) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO book_state (provider_key, state, origin, updated_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![provider_key, state, origin, updated_at],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn delete_book_state(&self, provider_key: &str) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM book_state WHERE provider_key = ?1",
                params![provider_key],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    /// Progress automation (the reader just saved a position at
    /// `now`): the book is being read. Rules, in order: a recorded
    /// 'read' is never auto-demoted; an auto row just refreshes; a
    /// USER row older than this reading session yields to 'reading'
    /// (the user's word stands until they actually read again —
    /// then the truth is they're reading it).
    pub fn touch_book_reading(&self, provider_key: &str, now: &str) -> Result<()> {
        match self.book_state_for(provider_key)? {
            None => self.set_book_state(provider_key, "reading", "auto", now),
            Some((state, _, _)) if state == "read" => Ok(()),
            Some((_, origin, updated)) if origin == "user" && updated.as_str() >= now => Ok(()),
            Some(_) => self.set_book_state(provider_key, "reading", "auto", now),
        }
    }

    // -- Facet lookups (search language: author: / series: / state:)

    pub fn items_by_book_author(&self, author: &str) -> Result<Vec<ufm_vfs::Item>> {
        self.items_by_query(
            "SELECT i.id, i.location_id, i.name, i.kind, i.size_bytes, i.modified, i.provider_key
             FROM items i JOIN book_authors ba ON ba.item_id = i.id
             WHERE ba.author = ?1 COLLATE NOCASE
             ORDER BY i.name",
            author,
        )
    }

    pub fn items_by_book_series(&self, series: &str) -> Result<Vec<ufm_vfs::Item>> {
        self.items_by_query(
            "SELECT i.id, i.location_id, i.name, i.kind, i.size_bytes, i.modified, i.provider_key
             FROM items i JOIN book_metadata bm ON bm.item_id = i.id
             WHERE bm.series = ?1 COLLATE NOCASE
             ORDER BY bm.series_index IS NULL, bm.series_index, i.name",
            series,
        )
    }

    pub fn items_by_book_state(&self, state: &str) -> Result<Vec<ufm_vfs::Item>> {
        self.items_by_query(
            "SELECT i.id, i.location_id, i.name, i.kind, i.size_bytes, i.modified, i.provider_key
             FROM items i JOIN book_state bs ON bs.provider_key = i.provider_key
             WHERE bs.state = ?1
             ORDER BY i.name",
            state,
        )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn rom_media_cache_round_trip() {
        let cat = Catalog::open_in_memory().unwrap();
        // No row yet.
        assert!(cat.rom_media_for("item-1", "boxart").unwrap().is_none());
        cat.upsert_rom_media(
            "item-1",
            "boxart",
            "found",
            Some("libretro"),
            Some("/media/nes/covers/X.png"),
            "{}",
        )
        .unwrap();
        cat.upsert_rom_media(
            "item-1", "trailer", "missing", None, None, "{}",
        )
        .unwrap();
        let row = cat.rom_media_for("item-1", "boxart").unwrap().unwrap();
        assert_eq!(row.status, "found");
        assert_eq!(row.source.as_deref(), Some("libretro"));
        assert_eq!(
            row.local_path.as_deref(),
            Some("/media/nes/covers/X.png")
        );
        // Replace works (a later rung wins).
        cat.upsert_rom_media(
            "item-1",
            "boxart",
            "found",
            Some("screenscraper"),
            Some("/media/nes/covers/X.jpg"),
            "{}",
        )
        .unwrap();
        let row = cat.rom_media_for("item-1", "boxart").unwrap().unwrap();
        assert_eq!(row.source.as_deref(), Some("screenscraper"));
        // Art map only carries found box art.
        let art = cat.rom_media_art(&["item-1".to_string()]).unwrap();
        assert_eq!(art.get("item-1").map(|s| s.as_str()), Some("/media/nes/covers/X.jpg"));
        let art = cat.rom_media_art(&["item-1".to_string(), "nope".to_string()]).unwrap();
        assert_eq!(art.len(), 1);
    }

    use super::*;
    use ufm_core::{ItemId, LocationId};
    use ufm_vfs::{Item, ItemKind};

    fn sample_item() -> Item {
        Item {
            id: ItemId::new(),
            location: LocationId::new(),
            name: "return-2026.pdf".into(),
            kind: ItemKind::File,
            size_bytes: Some(1024),
            modified: None,
            provider_key: Some("test-key".into()),
            labels: vec![],
            starred: false,
        }
    }

    fn sample_location(id: LocationId) -> ufm_vfs::Location {
        ufm_vfs::Location {
            id,
            provider: "local".into(),
            display_name: "test".into(),
            capabilities: ufm_vfs::Capabilities {
                read: true,
                ..Default::default()
            },
            protected: ufm_vfs::ProtectionTier::None,
        }
    }

    #[test]
    fn tag_roundtrip() {
        let cat = Catalog::open_in_memory().unwrap();
        let item = sample_item();
        cat.insert_location(&sample_location(item.location)).unwrap();
        cat.insert_item(&item).unwrap();
        let id = item.id;
        cat.tag(&id, "taxes-2026").unwrap();
        cat.tag(&id, "receipts").unwrap();
        assert_eq!(cat.labels_for(&id).unwrap(), vec!["receipts", "taxes-2026"]);
        cat.untag(&id, "receipts").unwrap();
        assert_eq!(cat.labels_for(&id).unwrap(), vec!["taxes-2026"]);
    }

    #[test]
    fn video_identity_roundtrip_and_path_lookup() {
        let cat = Catalog::open_in_memory().unwrap();
        let loc = LocationId::new();
        cat.insert_location(&sample_location(loc)).unwrap();
        let mut ep = named_item(loc, "Severance.S01E01.mkv", ItemKind::File);
        ep.provider_key = Some("/tv/Severance.S01E01.mkv".into());
        cat.insert_item(&ep).unwrap();
        cat.upsert_video_identity(
            &ep.id,
            "episode",
            Some("Severance"),
            Some(2022),
            Some(95396),
            Some(1),
            Some(1),
            Some(1),
            Some(0.97),
            true,
            r#"{"source":"tmdb","size":10,"modified":null}"#,
        )
        .unwrap();
        let row = cat.media_identity_for(&ep.id).unwrap().unwrap();
        assert_eq!(row.media_kind, "episode");
        assert_eq!(row.title.as_deref(), Some("Severance"));
        assert_eq!(row.year, Some(2022));
        assert_eq!(row.tmdb_id, Some(95396));
        assert_eq!(row.season, Some(1));
        assert_eq!(row.episode, Some(1));
        assert_eq!(row.episode_end, Some(1));
        assert_eq!(row.confidence, Some(0.97));
        assert!(row.confirmed);
        // Path lookup resolves the same item.
        let found = cat
            .item_by_provider_key("/tv/Severance.S01E01.mkv")
            .unwrap()
            .unwrap();
        assert_eq!(found.id, ep.id);
        assert!(cat.item_by_provider_key("/nope.mkv").unwrap().is_none());
        // A ROM upsert on another item leaves video columns alone.
        let mut rom = named_item(loc, "game.zip", ItemKind::File);
        rom.provider_key = Some("/roms/game.zip".into());
        cat.insert_item(&rom).unwrap();
        cat.upsert_media_identity(&rom.id, "rom", Some("Game"), Some("nes"), None, "{}")
            .unwrap();
        let rrow = cat.media_identity_for(&rom.id).unwrap().unwrap();
        assert_eq!(rrow.tmdb_id, None);
        assert_eq!(rrow.season, None);
        assert!(!rrow.confirmed);
    }

    #[test]
    fn rom_identity_roundtrip_and_collection_scope() {
        let cat = Catalog::open_in_memory().unwrap();
        let loc = LocationId::new();
        cat.insert_location(&sample_location(loc)).unwrap();
        let mut set_item = named_item(loc, "mslug.zip", ItemKind::File);
        set_item.provider_key = Some("/roms/mslug.zip".into());
        let mut plain = named_item(loc, "notes.md", ItemKind::File);
        plain.provider_key = Some("/docs/notes.md".into());
        cat.insert_item(&set_item).unwrap();
        cat.insert_item(&plain).unwrap();
        // A MAME set identified primarily as arcade, also Neo Geo.
        cat.upsert_media_identity(
            &set_item.id,
            "rom",
            Some("Metal Slug - Super Vehicle-001"),
            Some("arcade"),
            Some("deadbeef"),
            r#"{"crc":0,"also":["neogeo"],"size":100,"modified":null,"container":"set"}"#,
        )
        .unwrap();
        // An unmatched candidate is cached too (title/platform NULL).
        cat.upsert_media_identity(
            &plain.id,
            "rom",
            None,
            None,
            None,
            r#"{"matched":false,"size":10,"modified":null}"#,
        )
        .unwrap();
        let row = cat.media_identity_for(&set_item.id).unwrap().unwrap();
        assert_eq!(row.platform.as_deref(), Some("arcade"));
        assert_eq!(row.title.as_deref(), Some("Metal Slug - Super Vehicle-001"));
        // Primary platform, also-platform, and a miss.
        assert_eq!(cat.rom_collection_items("arcade", None).unwrap().len(), 1);
        let neo = cat.rom_collection_items("neogeo", None).unwrap();
        assert_eq!(neo.len(), 1);
        assert_eq!(neo[0].1, "Metal Slug - Super Vehicle-001");
        assert!(cat.rom_collection_items("nes", None).unwrap().is_empty());
        // Scope: the item's own location includes it; another excludes it.
        let mine = vec![loc.to_string()];
        assert_eq!(cat.rom_collection_items("arcade", Some(&mine)).unwrap().len(), 1);
        let other = vec![LocationId::new().to_string()];
        assert!(cat.rom_collection_items("arcade", Some(&other)).unwrap().is_empty());
        // The unmatched file is in NO collection.
        assert!(cat
            .rom_collection_items("c64", None)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn integrity_ok_on_fresh_db() {
        let cat = Catalog::open_in_memory().unwrap();
        assert_eq!(cat.integrity_check().unwrap(), "ok");
    }

    fn named_item(location: LocationId, name: &str, kind: ItemKind) -> Item {
        Item {
            id: ItemId::new(),
            location,
            name: name.into(),
            kind,
            size_bytes: Some(2048),
            modified: Some(
                chrono::DateTime::parse_from_rfc3339("2026-10-01T12:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            ),
            provider_key: Some(format!("/fake/{name}")),
            labels: vec![],
            starred: false,
        }
    }

    #[test]
    fn reinsert_same_path_leaves_single_fts_hit() {
        // Re-walks re-insert the same (location, provider_key) under
        // a fresh ItemId; the items REPLACE churns the rowid, and
        // without retiring the old external-content FTS entry the
        // search ghosts a duplicate hit (found by the P0-2 gate).
        let cat = Catalog::open_in_memory().unwrap();
        let loc = LocationId::new();
        cat.insert_location(&sample_location(loc)).unwrap();
        cat.insert_item(&named_item(loc, "ghostcheck.txt", ItemKind::File))
            .unwrap();
        cat.insert_item(&named_item(loc, "ghostcheck.txt", ItemKind::File))
            .unwrap();
        let hits = cat.search_fts("ghostcheck*", 10).unwrap();
        assert_eq!(hits.len(), 1, "no ghost FTS hits after re-insert");
        assert_eq!(hits[0].0.name, "ghostcheck.txt");
    }

    #[test]
    fn search_fts_roundtrip() {
        let cat = Catalog::open_in_memory().unwrap();
        let loc = LocationId::new();
        cat.insert_location(&sample_location(loc)).unwrap();
        cat.insert_item(&named_item(loc, "quarterly report.pdf", ItemKind::File))
            .unwrap();
        cat.insert_item(&named_item(loc, "draft notes.txt", ItemKind::File))
            .unwrap();
        cat.insert_item(&named_item(loc, "holiday photos", ItemKind::Folder))
            .unwrap();

        // Prefix query matching exactly one row.
        let hits = cat.search_fts("quarter*", 10).unwrap();
        assert_eq!(hits.len(), 1);
        let (item, rank) = &hits[0];
        assert_eq!(item.name, "quarterly report.pdf");
        assert_eq!(item.kind, ItemKind::File);
        assert_eq!(item.size_bytes, Some(2048));
        assert_eq!(item.location, loc);
        assert_eq!(item.provider_key.as_deref(), Some("/fake/quarterly report.pdf"));
        assert_eq!(
            item.modified,
            Some(
                chrono::DateTime::parse_from_rfc3339("2026-10-01T12:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc)
            )
        );
        assert!(rank.is_finite());

        // Results are bm25-ranked; a broader query still orders sanely.
        let hits = cat.search_fts("report* OR photos*", 10).unwrap();
        assert_eq!(hits.len(), 2);

        // Empty / whitespace queries return no rows, never an error.
        assert!(cat.search_fts("", 10).unwrap().is_empty());
        assert!(cat.search_fts("   ", 10).unwrap().is_empty());

        // Limit is honored (3 rows match, only 1 returned).
        let hits = cat.search_fts("report* OR photos* OR draft*", 1).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn item_by_id_and_path_roundtrip() {
        let cat = Catalog::open_in_memory().unwrap();
        let item = sample_item();
        cat.insert_location(&sample_location(item.location)).unwrap();
        cat.insert_item(&item).unwrap();
        let got = cat.item_by_id(&item.id).unwrap().unwrap();
        assert_eq!(got.id, item.id);
        assert_eq!(got.name, "return-2026.pdf");
        assert_eq!(got.provider_key.as_deref(), Some("test-key"));
        assert!(cat.item_by_id(&ItemId::new()).unwrap().is_none());
        let by_path = cat.item_by_path("test-key").unwrap().unwrap();
        assert_eq!(by_path.id, item.id);
        assert!(cat.item_by_path("/no/such/path").unwrap().is_none());
        assert_eq!(cat.item_count().unwrap(), 1);
    }

    #[test]
    fn recent_jobs_returns_newest_first() {
        let cat = Catalog::open_in_memory().unwrap();
        let mut older = ufm_jobs::Job::new("older job", ufm_jobs::JobPreview::default());
        let mut newer = ufm_jobs::Job::new("newer job", ufm_jobs::JobPreview::default());
        newer.created = older.created + chrono::Duration::seconds(5);
        older.state = ufm_jobs::JobState::Completed;
        newer.state = ufm_jobs::JobState::Running;
        cat.upsert_job(&older).unwrap();
        cat.upsert_job(&newer).unwrap();
        let all = cat.recent_jobs(10).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].title, "newer job");
        assert_eq!(all[1].title, "older job");
        // Any state is returned (not just unfinished), and limit caps.
        let one = cat.recent_jobs(1).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].title, "newer job");
    }

    #[test]
    #[test]
    fn mediacomplete_runs_upsert_recent_and_prune() {
        let cat = Catalog::open_in_memory().unwrap();
        assert!(cat.recent_mediacomplete_runs(10).unwrap().is_empty());
        let mk = |id: &str, started: &str, status: &str| MediaCompleteRunRow {
            id: id.into(),
            schedule_id: Some("sched-1".into()),
            schedule_name: "Overnight films".into(),
            target: "/media/films".into(),
            started: started.into(),
            finished: None,
            was_late: false,
            status: status.into(),
            summary_json: "{}".into(),
            actions_json: "[]".into(),
            pending_json: "[]".into(),
            quota_json: None,
            undone_at: None,
        };
        cat.upsert_mediacomplete_run(&mk("run-1", "2026-10-15T06:30:00Z", "running"))
            .unwrap();
        // Finish update replaces the same row (crash-evidence write,
        // then the full journal).
        let mut done = mk("run-1", "2026-10-15T06:30:00Z", "completed");
        done.finished = Some("2026-10-15T06:47:00Z".into());
        done.summary_json = "{\"organized\":3}".into();
        done.actions_json = "[{\"kind\":\"move\",\"from\":\"/a\",\"to\":\"/b\"}]".into();
        done.quota_json = Some("{\"remaining\":4}".into());
        cat.upsert_mediacomplete_run(&done).unwrap();
        cat.upsert_mediacomplete_run(&mk("run-2", "2026-10-16T06:30:00Z", "completed"))
            .unwrap();
        let rows = cat.recent_mediacomplete_runs(10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "run-2"); // newest first
        let one = cat.mediacomplete_run("run-1").unwrap().unwrap();
        assert_eq!(one.status, "completed");
        assert_eq!(one.summary_json, "{\"organized\":3}");
        assert!(one.quota_json.is_some());
        assert_eq!(cat.prune_mediacomplete_runs(1).unwrap(), 1);
        assert_eq!(cat.recent_mediacomplete_runs(10).unwrap().len(), 1);
    }

    #[test]
    fn mcp_audit_append_recent_and_prune() {
        let cat = Catalog::open_in_memory().unwrap();
        // Empty chain reads empty.
        assert!(cat.recent_mcp_audit(10).unwrap().is_empty());
        let s1 = cat
            .append_mcp_audit(
                "client-1",
                "search",
                "{\"query\":\"zelda\"}",
                "{\"outcome\":\"allowed\"}",
                false,
                12,
            )
            .unwrap();
        let s2 = cat
            .append_mcp_audit(
                "client-1",
                "organize",
                "{}",
                "{\"outcome\":\"needs_approval\"}",
                true,
                1,
            )
            .unwrap();
        assert!(s2 > s1);
        let rows = cat.recent_mcp_audit(10).unwrap();
        assert_eq!(rows.len(), 2);
        // Newest first.
        assert_eq!(rows[0].tool, "organize");
        assert!(rows[0].is_write);
        assert_eq!(rows[1].tool, "search");
        assert!(!rows[1].is_write);
        assert_eq!(rows[1].duration_ms, 12);
        // Retention: keep only the newest row.
        assert_eq!(cat.prune_mcp_audit(1).unwrap(), 1);
        let rows = cat.recent_mcp_audit(10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tool, "organize");
        // Pruning an already-small chain is a no-op.
        assert_eq!(cat.prune_mcp_audit(10).unwrap(), 0);
    }

    #[test]
    fn duplicate_candidates_groups_by_size() {
        let cat = Catalog::open_in_memory().unwrap();
        let loc = LocationId::new();
        cat.insert_location(&sample_location(loc)).unwrap();
        for (name, size) in [
            ("a.bin", 500u64),
            ("b.bin", 500),
            ("c.bin", 500),
            ("d.bin", 900),
            ("e.bin", 900),
            ("solo.bin", 42),
            ("empty.bin", 0),
        ] {
            let mut it = named_item(loc, name, ItemKind::File);
            it.size_bytes = Some(size);
            it.provider_key = Some(format!("/data/{name}"));
            cat.insert_item(&it).unwrap();
        }
        // A folder with a shared size must NOT form a group.
        let mut dir = named_item(loc, "folder", ItemKind::Folder);
        dir.size_bytes = Some(500);
        dir.provider_key = Some("/data/folder".into());
        cat.insert_item(&dir).unwrap();

        let groups = cat.duplicate_candidates(10).unwrap();
        assert_eq!(groups.len(), 2);
        // Biggest waste first: 500x(3-1)=1000 beats 900x(2-1)=900.
        assert_eq!(groups[0].size_bytes, 500);
        assert_eq!(groups[0].count, 3);
        assert_eq!(groups[0].wasted_bytes, 1000);
        assert_eq!(groups[0].sample_paths.len(), 3);
        assert_eq!(groups[1].size_bytes, 900);
        assert_eq!(groups[1].count, 2);
    }
}

// ---------------------------------------------------------------------------
// Job persistence helpers (spec §25)
// ---------------------------------------------------------------------------

/// Serialize a state enum the same way serde does (snake_case), so stored
/// strings always match the JSON representation of the job itself.
fn state_name<T: serde::Serialize>(state: &T) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn parse_state<T: serde::de::DeserializeOwned>(s: &str) -> Result<T> {
    serde_json::from_value(serde_json::Value::String(s.to_string()))
        .map_err(|e| UfmError::Database(format!("bad stored state {s:?}: {e}")))
}

fn insert_journal_entry(conn: &Connection, entry: &ufm_jobs::JournalEntry) -> Result<()> {
    conn.execute(
        "INSERT INTO job_journal (job_id, step_id, from_state, to_state, at, note)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            entry.job.to_string(),
            entry.step,
            state_name(&entry.from),
            state_name(&entry.to),
            entry.at.to_rfc3339(),
            entry.note
        ],
    )
    .map_err(|e| UfmError::Database(e.to_string()))?;
    Ok(())
}

/// `ufm_jobs::JournalSink` backed by the catalog database file. Opens a
/// short-lived connection per record — the same idiom the app uses for
/// background catalog work; WAL mode keeps it cheap.
pub struct CatalogJournalSink {
    path: std::path::PathBuf,
}

impl CatalogJournalSink {
    pub fn new(path: std::path::PathBuf) -> Self {
        Self { path }
    }
}

impl ufm_jobs::JournalSink for CatalogJournalSink {
    fn record(&self, entry: &ufm_jobs::JournalEntry) -> Result<()> {
        let conn =
            Connection::open(&self.path).map_err(|e| UfmError::Database(e.to_string()))?;
        // P0: without a busy timeout, a journal insert that races a
        // checkpoint write fails instantly with SQLITE_BUSY and the
        // journal silently loses rows (the live journal was EMPTY
        // after a night of exactly that). Wait for the lock instead.
        let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
        insert_journal_entry(&conn, entry)
    }
}

#[cfg(test)]
mod job_tests {
    use super::*;
    use ufm_jobs::{Job, JobPreview, JobState, JournalEntry, Step, StepOp, StepState};

    fn item(name: &str) -> ufm_vfs::Item {
        ufm_vfs::Item {
            id: ItemId::new(),
            location: LocationId::new(),
            name: name.into(),
            kind: ufm_vfs::ItemKind::File,
            size_bytes: Some(42),
            modified: None,
            provider_key: Some(format!("/tmp/{name}")),
            labels: vec![],
            starred: false,
        }
    }

    fn sample_job(state: JobState) -> Job {
        let mut job = Job::new("copy test", JobPreview::default());
        job.state = state;
        job.add_step(Step {
            id: "s0001".into(),
            label: "copy a.txt".into(),
            op: StepOp::Copy {
                src: item("a.txt"),
                dst_parent: item("dest"),
                new_name: "a.txt".into(),
            },
            depends_on: vec![],
            state: StepState::Pending,
            compensation: None,
            outcome: false,
            outcome_path: None,
        });
        job
    }

    fn entry(job: JobId, step: &str, from: StepState, to: StepState) -> JournalEntry {
        JournalEntry {
            job,
            step: step.into(),
            from,
            to,
            at: ufm_core::now_utc(),
            note: None,
        }
    }

    #[test]
    fn job_snapshot_roundtrip_and_unfinished_filter() {
        let cat = Catalog::open_in_memory().unwrap();
        let running = sample_job(JobState::Running);
        let done = sample_job(JobState::Completed);
        cat.upsert_job(&running).unwrap();
        cat.upsert_job(&done).unwrap();

        let loaded = cat.load_job(&running.id).unwrap().unwrap();
        assert_eq!(loaded.id, running.id);
        assert_eq!(loaded.title, "copy test");
        assert_eq!(loaded.steps.len(), 1);
        assert_eq!(loaded.state, JobState::Running);

        let unfinished = cat.unfinished_jobs().unwrap();
        assert_eq!(unfinished.len(), 1);
        assert_eq!(unfinished[0].id, running.id);

        // Transition to a terminal state: no longer unfinished.
        let mut finished = running.clone();
        finished.state = JobState::Completed;
        cat.upsert_job(&finished).unwrap();
        assert!(cat.unfinished_jobs().unwrap().is_empty());
    }

    #[test]
    fn job_outcomes_roundtrip() {
        // Inc-2 §4.3: outcomes ride inside job_json — a job carrying a
        // trash outcome and a move outcome loads back intact, and the
        // unfinished filter is unaffected.
        let cat = Catalog::open_in_memory().unwrap();
        let mut job = sample_job(JobState::Running);
        job.outcomes = vec![
            ufm_jobs::JobOutcome {
                step_id: "s0001".into(),
                src_path: "/tmp/a.txt".into(),
                result_path: None,
                trash_name: Some("a.txt".into()),
            },
            ufm_jobs::JobOutcome {
                step_id: "s0002".into(),
                src_path: "/tmp/b.txt".into(),
                result_path: Some("/dst/b.txt".into()),
                trash_name: None,
            },
        ];
        cat.upsert_job(&job).unwrap();
        let loaded = cat.load_job(&job.id).unwrap().unwrap();
        assert_eq!(loaded.outcomes.len(), 2);
        assert_eq!(loaded.outcomes[0].step_id, "s0001");
        assert_eq!(loaded.outcomes[0].trash_name.as_deref(), Some("a.txt"));
        assert_eq!(loaded.outcomes[0].result_path, None);
        assert_eq!(
            loaded.outcomes[1].result_path.as_deref(),
            Some("/dst/b.txt")
        );
        let unfinished = cat.unfinished_jobs().unwrap();
        assert_eq!(unfinished.len(), 1);
        assert_eq!(unfinished[0].id, job.id);
    }

    #[test]
    fn journal_entries_roundtrip_in_order() {
        let cat = Catalog::open_in_memory().unwrap();
        let job = sample_job(JobState::Running);
        cat.record_journal_entry(&entry(
            job.id,
            "s0001",
            StepState::Pending,
            StepState::Running,
        ))
        .unwrap();
        let mut done = entry(job.id, "s0001", StepState::Running, StepState::Done);
        done.note = Some("ok".into());
        cat.record_journal_entry(&done).unwrap();

        let entries = cat.journal_entries(&job.id).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].to, StepState::Running);
        assert_eq!(entries[1].to, StepState::Done);
        assert_eq!(entries[1].note.as_deref(), Some("ok"));
        assert!(cat.journal_entries(&JobId::new()).unwrap().is_empty());
    }

    #[test]
    fn catalog_journal_sink_persists_to_file() {
        let path = std::env::temp_dir().join(format!(
            "ufm-catalog-sink-test-{}.db",
            uuid::Uuid::new_v4()
        ));
        let job = sample_job(JobState::Running);
        {
            let _cat = Catalog::open(&path).unwrap(); // creates schema
            let sink = CatalogJournalSink::new(path.clone());
            ufm_jobs::JournalSink::record(
                &sink,
                &entry(job.id, "s0001", StepState::Pending, StepState::Running),
            )
            .unwrap();
        }
        let cat = Catalog::open(&path).unwrap();
        let entries = cat.journal_entries(&job.id).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].step, "s0001");
        drop(cat);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }
}

#[cfg(test)]
mod library_memory_tests {
    use super::*;
    use ufm_core::{ItemId, LocationId};
    use ufm_jobs::{Job, JobPreview, JobState, Step, StepOp, StepState};

    fn loc() -> (LocationId, ufm_vfs::Location) {
        let id = LocationId::new();
        (
            id,
            ufm_vfs::Location {
                id,
                provider: "local".into(),
                display_name: "test".into(),
                capabilities: ufm_vfs::Capabilities {
                    read: true,
                    ..Default::default()
                },
                protected: ufm_vfs::ProtectionTier::None,
            },
        )
    }

    fn file_item(location: LocationId, path: &str, size: u64) -> ufm_vfs::Item {
        ufm_vfs::Item {
            id: ItemId::new(),
            location,
            name: path.rsplit('/').next().unwrap_or(path).into(),
            kind: ufm_vfs::ItemKind::File,
            size_bytes: Some(size),
            modified: Some(
                chrono::DateTime::parse_from_rfc3339("2026-10-01T12:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            ),
            provider_key: Some(path.into()),
            labels: vec![],
            starred: false,
        }
    }

    #[test]
    fn volume_sightings_merge_mount_history() {
        let cat = Catalog::open_in_memory().unwrap();
        cat.record_volume_sighting(
            "uuid-1",
            Some("Desk SSD"),
            "/media/richard/Desk SSD",
            "2026-10-01T10:00:00Z",
        )
        .unwrap();
        // Same mount again: history stays one entry, last_seen moves.
        cat.record_volume_sighting(
            "uuid-1",
            Some("Desk SSD"),
            "/media/richard/Desk SSD",
            "2026-10-05T10:00:00Z",
        )
        .unwrap();
        // Same drive, new mount point: SAME volume, history grows.
        cat.record_volume_sighting(
            "uuid-1",
            None,
            "/run/media/richard/Desk SSD",
            "2026-10-09T10:00:00Z",
        )
        .unwrap();
        let v = cat.volume("uuid-1").unwrap().unwrap();
        assert_eq!(v.label.as_deref(), Some("Desk SSD"));
        assert_eq!(v.first_seen, "2026-10-01T10:00:00Z");
        assert_eq!(v.last_seen, "2026-10-09T10:00:00Z");
        assert_eq!(v.mount_history.len(), 2);
        assert_eq!(v.mount_history[0].last_seen, "2026-10-05T10:00:00Z");
        cat.mark_volume_indexed("uuid-1", "2026-10-09T11:00:00Z").unwrap();
        let v = cat.volume("uuid-1").unwrap().unwrap();
        assert_eq!(v.last_indexed.as_deref(), Some("2026-10-09T11:00:00Z"));
        assert_eq!(cat.volumes().unwrap().len(), 1);
        cat.delete_volume("uuid-1").unwrap();
        assert!(cat.volume("uuid-1").unwrap().is_none());
    }

    #[test]
    fn network_sightings_merge_mount_history() {
        let cat = Catalog::open_in_memory().unwrap();
        cat.record_network_sighting(
            "sftp://fabletest@127.0.0.1/share",
            Some("127.0.0.1"),
            "/run/user/1000/gvfs/sftp:host=127.0.0.1,user=fabletest",
            "2026-10-01T10:00:00Z",
        )
        .unwrap();
        // Same mount again: history stays one entry, last_seen moves.
        cat.record_network_sighting(
            "sftp://fabletest@127.0.0.1/share",
            Some("127.0.0.1"),
            "/run/user/1000/gvfs/sftp:host=127.0.0.1,user=fabletest",
            "2026-10-05T10:00:00Z",
        )
        .unwrap();
        // Same server re-mounted under a different FUSE prefix (the
        // gvfs dir name changed): SAME server, history grows.
        cat.record_network_sighting(
            "sftp://fabletest@127.0.0.1/share",
            None,
            "/run/user/1000/gvfs/sftp:host=127.0.0.1",
            "2026-10-09T10:00:00Z",
        )
        .unwrap();
        let v = cat.network_location("sftp://fabletest@127.0.0.1/share")
            .unwrap()
            .unwrap();
        assert_eq!(v.display_name.as_deref(), Some("127.0.0.1"));
        assert_eq!(v.first_seen, "2026-10-01T10:00:00Z");
        assert_eq!(v.last_seen, "2026-10-09T10:00:00Z");
        assert_eq!(v.mount_history.len(), 2);
        assert_eq!(v.mount_history[0].last_seen, "2026-10-05T10:00:00Z");
        cat.mark_network_indexed(
            "sftp://fabletest@127.0.0.1/share",
            "2026-10-09T11:00:00Z",
        )
        .unwrap();
        let v = cat.network_location("sftp://fabletest@127.0.0.1/share")
            .unwrap()
            .unwrap();
        assert_eq!(v.last_indexed.as_deref(), Some("2026-10-09T11:00:00Z"));
        assert_eq!(cat.network_locations().unwrap().len(), 1);
        cat.delete_network_location("sftp://fabletest@127.0.0.1/share")
            .unwrap();
        assert!(cat
            .network_location("sftp://fabletest@127.0.0.1/share")
            .unwrap()
            .is_none());
    }

    #[test]
    fn network_locations_order_by_last_seen_desc() {
        let cat = Catalog::open_in_memory().unwrap();
        cat.record_network_sighting(
            "smb://nas.local/photos",
            Some("nas"),
            "/run/user/1000/gvfs/smb-share:server=nas.local,share=photos",
            "2026-10-01T10:00:00Z",
        )
        .unwrap();
        cat.record_network_sighting(
            "dav://127.0.0.1:8971/dav",
            Some("127.0.0.1"),
            "/run/user/1000/gvfs/dav:host=127.0.0.1,port=8971,prefix=%2Fdav",
            "2026-10-03T10:00:00Z",
        )
        .unwrap();
        let all = cat.network_locations().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].uri, "dav://127.0.0.1:8971/dav");
        assert_eq!(all[1].uri, "smb://nas.local/photos");
    }

    #[test]
    fn manifest_and_prefix_purge() {
        let cat = Catalog::open_in_memory().unwrap();
        let (id, location) = loc();
        cat.insert_location(&location).unwrap();
        cat.insert_item(&file_item(id, "/media/desk/a.txt", 10)).unwrap();
        cat.insert_item(&file_item(id, "/media/desk/sub/b.txt", 20)).unwrap();
        cat.insert_item(&file_item(id, "/home/richard/c.txt", 30)).unwrap();

        let manifest = cat.manifest_for_location(&id).unwrap();
        assert_eq!(manifest.len(), 3);
        let (size, modified) = &manifest["/media/desk/a.txt"];
        assert_eq!(*size, Some(10));
        assert_eq!(modified.as_deref(), Some("2026-10-01T12:00:00+00:00"));

        // Prefix matching: exact-boundary semantics — "/media/desk"
        // must not swallow "/media/desk2".
        cat.insert_item(&file_item(id, "/media/desk2/d.txt", 40)).unwrap();
        assert_eq!(
            cat.item_count_under_prefixes(&["/media/desk".to_string()])
                .unwrap(),
            2
        );
        let purged = cat
            .purge_items_under_prefixes(&["/media/desk".to_string()])
            .unwrap();
        assert_eq!(purged, 2);
        // Survivors: the home file and the desk2 file; FTS is clean.
        assert_eq!(cat.manifest_for_location(&id).unwrap().len(), 2);
        assert!(cat.search_fts("a*", 10).unwrap().is_empty());
        assert_eq!(cat.search_fts("c*", 10).unwrap().len(), 1);
        assert_eq!(cat.search_fts("d*", 10).unwrap().len(), 1);
        // Purging nothing is a no-op, not an error.
        assert_eq!(
            cat.purge_items_under_prefixes(&["/media/desk".to_string()])
                .unwrap(),
            0
        );
        assert_eq!(cat.purge_items_under_prefixes(&[]).unwrap(), 0);
    }

    fn bloated_job(state: JobState, steps: usize) -> Job {
        let mut job = Job::new("Copy richard", JobPreview::default());
        job.state = state;
        for i in 0..steps {
            job.add_step(Step {
                id: format!("s{i:05}"),
                label: format!("copy file {i}"),
                op: StepOp::Copy {
                    src: file_item(LocationId::new(), &format!("/src/{i}.bin"), 100),
                    dst_parent: file_item(LocationId::new(), "/dst", 0),
                    new_name: format!("{i}.bin"),
                },
                depends_on: vec![],
                state: StepState::Done,
                compensation: None,
                outcome: false,
                outcome_path: None,
            });
        }
        job.outcomes.push(ufm_jobs::JobOutcome {
            step_id: "s00000".into(),
            src_path: "/src/0.bin".into(),
            result_path: Some("/dst/0.bin".into()),
            trash_name: None,
        });
        job
    }

    #[test]
    fn slimming_touches_terminal_jobs_only_and_keeps_the_audit() {
        let cat = Catalog::open_in_memory().unwrap();
        let cancelled = bloated_job(JobState::Cancelled, 50);
        let running = bloated_job(JobState::Running, 50);
        let paused = bloated_job(JobState::Paused, 50);
        cat.upsert_job(&cancelled).unwrap();
        cat.upsert_job(&running).unwrap();
        cat.upsert_job(&paused).unwrap();

        let report = cat.slim_terminal_job_blobs().unwrap();
        assert_eq!(report.jobs_slimmed, 1);
        assert!(report.bytes_freed > 0);

        // Terminal job: steps gone, tallies + audit intact.
        let slim = cat.load_job(&cancelled.id).unwrap().unwrap();
        assert!(slim.steps.is_empty());
        assert!(slim.steps_slimmed);
        let counts = slim.step_counts.unwrap();
        assert_eq!(counts.total, 50);
        assert_eq!(counts.done, 50);
        assert_eq!(slim.outcomes.len(), 1);
        assert_eq!(slim.title, "Copy richard");
        assert_eq!(slim.state, JobState::Cancelled);

        // Unfinished jobs: byte-for-byte the same job back.
        for original in [&running, &paused] {
            let loaded = cat.load_job(&original.id).unwrap().unwrap();
            assert_eq!(loaded.steps.len(), 50);
            assert!(!loaded.steps_slimmed);
        }
        // Unfinished filter still sees exactly the live two.
        assert_eq!(cat.unfinished_jobs().unwrap().len(), 2);

        // Idempotent: a second pass slims nothing.
        let again = cat.slim_terminal_job_blobs().unwrap();
        assert_eq!(again.jobs_slimmed, 0);
        assert_eq!(again.bytes_freed, 0);

        // Vacuum runs clean on a live schema.
        cat.vacuum().unwrap();
        assert!(cat.database_size_bytes().unwrap() > 0);
        assert_eq!(cat.integrity_check().unwrap(), "ok");
    }
}

#[cfg(test)]
mod phase2_tests {
    use super::*;
    use ufm_core::{CollectionId, ItemId, LocationId};
    use ufm_vfs::{Item, ItemKind};

    fn item_at(location: LocationId, name: &str, path: &str) -> Item {
        Item {
            id: ItemId::new(),
            location,
            name: name.into(),
            kind: ItemKind::File,
            size_bytes: Some(10),
            modified: None,
            provider_key: Some(path.into()),
            labels: vec![],
            starred: false,
        }
    }

    fn location(id: LocationId) -> ufm_vfs::Location {
        ufm_vfs::Location {
            id,
            provider: "local".into(),
            display_name: "test".into(),
            capabilities: ufm_vfs::Capabilities {
                read: true,
                ..Default::default()
            },
            protected: ufm_vfs::ProtectionTier::None,
        }
    }

    fn seeded() -> (Catalog, LocationId, Item, Item) {
        let cat = Catalog::open_in_memory().unwrap();
        let loc = LocationId::new();
        cat.insert_location(&location(loc)).unwrap();
        let a = item_at(loc, "alpha.txt", "/docs/alpha.txt");
        let b = item_at(loc, "beta.txt", "/docs/beta.txt");
        cat.insert_item(&a).unwrap();
        cat.insert_item(&b).unwrap();
        (cat, loc, a, b)
    }

    #[test]
    fn star_roundtrip_and_listing() {
        let (cat, _loc, _a, _b) = seeded();
        assert!(!cat.is_starred_path("/docs/alpha.txt").unwrap());
        assert!(cat.toggle_star_path("/docs/alpha.txt").unwrap());
        assert!(cat.is_starred_path("/docs/alpha.txt").unwrap());
        let items = cat.starred_items().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "alpha.txt");
        assert!(items[0].starred, "hydrated item carries its star");
        assert!(!cat.toggle_star_path("/docs/alpha.txt").unwrap());
        assert!(cat.starred_items().unwrap().is_empty());
    }

    #[test]
    fn collections_crud_members_and_missing() {
        let (cat, _loc, _a, _b) = seeded();
        let id = CollectionId::new();
        cat.create_collection(&id, "Reading", "manual").unwrap();
        cat.add_collection_member(&id, "/docs/alpha.txt").unwrap();
        cat.add_collection_member(&id, "/docs/beta.txt").unwrap();
        cat.add_collection_member(&id, "/docs/gone.txt").unwrap();
        // Duplicate add is a no-op.
        cat.add_collection_member(&id, "/docs/alpha.txt").unwrap();

        let (items, missing) = cat.collection_items(&id).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(missing, vec!["/docs/gone.txt".to_string()]);

        let rows = cat.list_collections().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].member_count, 3);
        assert_eq!(rows[0].missing_count, 1);

        cat.rename_collection(&id, "Reading 2026").unwrap();
        assert_eq!(cat.list_collections().unwrap()[0].name, "Reading 2026");

        cat.remove_collection_member(&id, "/docs/gone.txt").unwrap();
        assert_eq!(cat.list_collections().unwrap()[0].missing_count, 0);

        cat.delete_collection(&id).unwrap();
        assert!(cat.list_collections().unwrap().is_empty());
        // Files themselves untouched.
        assert_eq!(cat.starred_items().unwrap().len(), 0);
    }

    #[test]
    fn membership_survives_reindex_and_rekey() {
        let (cat, _loc, a, _b) = seeded();
        let id = CollectionId::new();
        cat.create_collection(&id, "Keep", "manual").unwrap();
        cat.add_collection_member(&id, "/docs/alpha.txt").unwrap();
        cat.set_star_path("/docs/alpha.txt", true).unwrap();

        // Re-index the same path: stable id, membership + star intact.
        let mut again = a.clone();
        again.id = ItemId::new(); // the walk mints a fresh id; catalog keeps the stable one
        again.size_bytes = Some(11);
        cat.insert_item(&again).unwrap();
        let (items, missing) = cat.collection_items(&id).unwrap();
        assert_eq!(items.len(), 1);
        assert!(missing.is_empty());
        assert!(cat.is_starred_path("/docs/alpha.txt").unwrap());

        // In-app rename re-keys both stores.
        cat.rekey_path("/docs/alpha.txt", "/docs/alpha-renamed.txt")
            .unwrap();
        assert!(cat.is_starred_path("/docs/alpha-renamed.txt").unwrap());
        assert_eq!(
            cat.collection_member_keys(&id).unwrap(),
            vec!["/docs/alpha-renamed.txt".to_string()]
        );
    }

    #[test]
    fn rekey_moves_folder_children() {
        let (cat, _loc, _a, _b) = seeded();
        let id = CollectionId::new();
        cat.create_collection(&id, "Folder", "manual").unwrap();
        cat.add_collection_member(&id, "/docs/beta.txt").unwrap();
        cat.set_star_path("/docs/beta.txt", true).unwrap();
        cat.rekey_path("/docs", "/papers").unwrap();
        assert!(cat.is_starred_path("/papers/beta.txt").unwrap());
        assert_eq!(
            cat.collection_member_keys(&id).unwrap(),
            vec!["/papers/beta.txt".to_string()]
        );
    }

    #[test]
    fn rekey_carries_item_row_and_its_identity() {
        // The Border incident (2026-10-16): Organize moved a
        // confirmed film and the catalog row stayed behind at the
        // dead path — then delete_unseen + ON DELETE CASCADE took
        // the user's confirmed match with it. The item row must
        // follow the file, id (and identity) intact.
        let (cat, _loc, a, _b) = seeded();
        cat.upsert_video_identity(
            &a.id,
            "movie",
            Some("Border"),
            Some(2018),
            Some(517839),
            None,
            None,
            None,
            Some(1.0),
            true,
            "{\"source\":\"user-confirm\"}",
        )
        .unwrap();
        cat.rekey_path(
            "/docs/alpha.txt",
            "/docs/Border (2018)/Border (2018).txt",
        )
        .unwrap();
        assert!(cat.item_by_provider_key("/docs/alpha.txt").unwrap().is_none());
        let moved = cat
            .item_by_provider_key("/docs/Border (2018)/Border (2018).txt")
            .unwrap()
            .expect("item row followed the move");
        assert_eq!(moved.id, a.id, "same item id at the new path");
        assert_eq!(moved.name, "Border (2018).txt");
        let idn = cat
            .media_identity_for(&a.id)
            .unwrap()
            .expect("identity survived the move");
        assert_eq!(idn.tmdb_id, Some(517839));
        assert!(idn.confirmed);
        // Search finds the file under its new name.
        let hits = cat.search_fts("border*", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0.id, a.id);
    }

    #[test]
    fn rekey_merges_into_a_row_a_rewalk_already_minted() {
        // Crash-resume ordering: the job completed while the app was
        // down, a re-walk minted a fresh row at the destination, and
        // only then did the move's follow run. The survivor keeps
        // its id and inherits the identity it was missing.
        let (cat, loc, a, _b) = seeded();
        cat.upsert_video_identity(
            &a.id,
            "movie",
            Some("Border"),
            Some(2018),
            Some(517839),
            None,
            None,
            None,
            Some(1.0),
            true,
            "{\"source\":\"user-confirm\"}",
        )
        .unwrap();
        let fresh = item_at(loc, "alpha.txt", "/docs/moved/alpha.txt");
        cat.insert_item(&fresh).unwrap();
        let survivor_id = cat
            .item_by_provider_key("/docs/moved/alpha.txt")
            .unwrap()
            .unwrap()
            .id;
        assert_ne!(survivor_id, a.id);
        cat.rekey_path("/docs/alpha.txt", "/docs/moved/alpha.txt")
            .unwrap();
        assert!(cat.item_by_provider_key("/docs/alpha.txt").unwrap().is_none());
        let after = cat
            .item_by_provider_key("/docs/moved/alpha.txt")
            .unwrap()
            .unwrap();
        assert_eq!(after.id, survivor_id, "the re-walked row survives");
        let idn = cat
            .media_identity_for(&survivor_id)
            .unwrap()
            .expect("identity transferred to the survivor");
        assert_eq!(idn.tmdb_id, Some(517839));
    }

    #[test]
    fn rekey_folder_move_carries_child_items_and_identities() {
        let (cat, _loc, a, _b) = seeded();
        cat.upsert_video_identity(
            &a.id,
            "movie",
            Some("Border"),
            Some(2018),
            Some(517839),
            None,
            None,
            None,
            Some(1.0),
            true,
            "{\"source\":\"user-confirm\"}",
        )
        .unwrap();
        cat.rekey_path("/docs", "/papers").unwrap();
        let child = cat
            .item_by_provider_key("/papers/alpha.txt")
            .unwrap()
            .expect("child item followed the folder move");
        assert_eq!(child.id, a.id);
        assert!(cat.media_identity_for(&a.id).unwrap().is_some());
    }

    #[test]
    fn view_profile_roundtrip() {
        let cat = Catalog::open_in_memory().unwrap();
        let loc = LocationId::new();
        assert!(cat.view_profile(&loc, "").unwrap().is_none());
        let profile = ViewProfileRow {
            view_mode: "list".into(),
            sort_json: "{\"key\":\"modified\",\"dir\":-1}".into(),
            show_extensions: false,
            group_by: String::new(),
        };
        cat.set_view_profile(&loc, "", &profile).unwrap();
        let got = cat.view_profile(&loc, "").unwrap().unwrap();
        assert_eq!(got.view_mode, "list");
        assert_eq!(got.sort_json, profile.sort_json);
        assert!(!got.show_extensions);
        // Overwrite wins.
        let p2 = ViewProfileRow {
            view_mode: "grid".into(),
            ..profile
        };
        cat.set_view_profile(&loc, "", &p2).unwrap();
        assert_eq!(cat.view_profile(&loc, "").unwrap().unwrap().view_mode, "grid");
    }

    #[test]
    fn view_profile_folder_inheritance() {
        // Grand Pass steal 5: folder rows inherit from the
        // location row, override it when present, and reset back.
        let cat = Catalog::open_in_memory().unwrap();
        let loc = LocationId::new();
        let base = ViewProfileRow {
            view_mode: "list".into(),
            sort_json: "{\"key\":\"name\",\"dir\":1}".into(),
            show_extensions: true,
            group_by: String::new(),
        };
        cat.set_view_profile(&loc, "", &base).unwrap();
        // A folder with no row of its own inherits the location's.
        let (row, own) = cat
            .view_profile_resolved(&loc, "/media/films")
            .unwrap();
        assert_eq!(row.unwrap().view_mode, "list");
        assert!(!own);
        // A folder row wins outright, group_by and all.
        let folder = ViewProfileRow {
            view_mode: "gallery".into(),
            sort_json: "{\"key\":\"size\",\"dir\":-1}".into(),
            show_extensions: false,
            group_by: "type".into(),
        };
        cat.set_view_profile(&loc, "/media/films", &folder).unwrap();
        let (row, own) = cat
            .view_profile_resolved(&loc, "/media/films")
            .unwrap();
        let row = row.unwrap();
        assert!(own);
        assert_eq!(row.view_mode, "gallery");
        assert_eq!(row.group_by, "type");
        // ...without disturbing the location row or other folders.
        let (row, own) = cat.view_profile_resolved(&loc, "/media").unwrap();
        assert_eq!(row.unwrap().view_mode, "list");
        assert!(!own);
        // Reset deletes only the folder's own row.
        cat.delete_view_profile(&loc, "/media/films").unwrap();
        let (row, own) = cat
            .view_profile_resolved(&loc, "/media/films")
            .unwrap();
        assert_eq!(row.unwrap().view_mode, "list");
        assert!(!own);
        // The location row itself is undeletable through this door.
        cat.delete_view_profile(&loc, "").unwrap();
        assert!(cat.view_profile(&loc, "").unwrap().is_some());
    }

    #[test]
    fn view_profile_migration_carries_location_rows() {
        // Grand Pass steal 5: a pre-pass database (location_id was
        // the whole primary key) keeps its remembered profile as
        // the location-level row after the rebuild — no data loss.
        let cat = Catalog::open_in_memory().unwrap();
        let loc = LocationId::new();
        cat.conn
            .execute_batch(
                "DROP TABLE view_profiles;
                 CREATE TABLE view_profiles (
                     location_id TEXT PRIMARY KEY REFERENCES locations(id) ON DELETE CASCADE,
                     show_extensions INTEGER NOT NULL DEFAULT 1,
                     view_mode TEXT NOT NULL DEFAULT 'list',
                     sort_json TEXT NOT NULL DEFAULT '{}'
                 );",
            )
            .unwrap();
        cat.conn
            .execute(
                "INSERT OR IGNORE INTO locations (id, provider, display_name, capabilities_json)
                 VALUES (?1, 'view-profile', ?1, '{}')",
                rusqlite::params![loc.to_string()],
            )
            .unwrap();
        cat.conn
            .execute(
                "INSERT INTO view_profiles (location_id, show_extensions, view_mode, sort_json)
                 VALUES (?1, 0, 'columns', '{\"key\":\"size\",\"dir\":-1}')",
                rusqlite::params![loc.to_string()],
            )
            .unwrap();
        cat.migrate().unwrap();
        let got = cat.view_profile(&loc, "").unwrap().unwrap();
        assert_eq!(got.view_mode, "columns");
        assert!(!got.show_extensions);
        assert_eq!(got.group_by, "");
        // And the rebuilt table accepts folder rows.
        let folder = ViewProfileRow {
            view_mode: "grid".into(),
            sort_json: "{}".into(),
            show_extensions: true,
            group_by: "date".into(),
        };
        cat.set_view_profile(&loc, "/somewhere", &folder).unwrap();
        assert_eq!(
            cat.view_profile(&loc, "/somewhere").unwrap().unwrap().group_by,
            "date"
        );
    }

    #[test]
    fn home_feed_learns_and_forgets() {
        // Grand Pass steal 1: recents order newest-first and
        // refresh in place; visits count up; forget + clear work
        // per shelf without touching the other.
        let cat = Catalog::open_in_memory().unwrap();
        let recent = |path: &str, at: i64| HomeRecentRow {
            path: path.into(),
            name: path.rsplit('/').next().unwrap().into(),
            kind: "file".into(),
            location_id: String::new(),
            item_id: String::new(),
            opened_at: at,
        };
        cat.record_recent(&recent("/docs/a.txt", 100)).unwrap();
        cat.record_recent(&recent("/docs/b.txt", 200)).unwrap();
        // Re-opening a.txt moves it to the top, no duplicate.
        cat.record_recent(&recent("/docs/a.txt", 300)).unwrap();
        let recents = cat.home_recents(30).unwrap();
        assert_eq!(recents.len(), 2);
        assert_eq!(recents[0].path, "/docs/a.txt");
        assert_eq!(recents[0].opened_at, 300);
        assert_eq!(recents[1].path, "/docs/b.txt");

        cat.record_folder_visit("/docs", "docs", "", "", 100).unwrap();
        cat.record_folder_visit("/docs", "docs", "", "", 400).unwrap();
        cat.record_folder_visit("/pics", "pics", "", "", 500).unwrap();
        let frequent = cat.home_frequent(12).unwrap();
        assert_eq!(frequent.len(), 2);
        assert_eq!(frequent[0].path, "/docs");
        assert_eq!(frequent[0].visits, 2);
        assert_eq!(frequent[0].last_at, 400);
        assert_eq!(frequent[1].path, "/pics");

        cat.forget_recent("/docs/a.txt").unwrap();
        assert_eq!(cat.home_recents(30).unwrap().len(), 1);
        cat.forget_frequent("/docs").unwrap();
        assert_eq!(cat.home_frequent(12).unwrap().len(), 1);
        cat.clear_recents().unwrap();
        assert!(cat.home_recents(30).unwrap().is_empty());
        // Clearing one shelf leaves the other standing.
        assert_eq!(cat.home_frequent(12).unwrap().len(), 1);
        cat.clear_frequent().unwrap();
        assert!(cat.home_frequent(12).unwrap().is_empty());
    }

    #[test]
    fn musiccomplete_runs_roundtrip() {
        let cat = Catalog::open_in_memory().unwrap();
        let run = MusicCompleteRunRow {
            id: "mu-1".into(),
            schedule_id: None,
            schedule_name: "Manual completion".into(),
            target: "/music".into(),
            started: "2026-10-15T03:00:00".into(),
            finished: None,
            was_late: false,
            status: "running".into(),
            summary_json: "{}".into(),
            actions_json: "[]".into(),
            pending_json: "[]".into(),
            undone_at: None,
        };
        cat.upsert_musiccomplete_run(&run).unwrap();
        let mut done = run.clone();
        done.status = "completed".into();
        done.finished = Some("2026-10-15T03:04:00".into());
        done.actions_json =
            "[{\"kind\":\"tags\",\"path\":\"/music/a.flac\",\"prior\":{\"album\":null}}]".into();
        done.pending_json = "[{\"path\":\"/music/b.mp3\"}]".into();
        cat.upsert_musiccomplete_run(&done).unwrap();
        let got = cat.musiccomplete_run("mu-1").unwrap().unwrap();
        assert_eq!(got.status, "completed");
        assert!(got.actions_json.contains("\"prior\""));
        assert_eq!(cat.recent_musiccomplete_runs(10).unwrap().len(), 1);
        // Undo stamp persists through the upsert.
        let mut undone = got.clone();
        undone.status = "undone".into();
        undone.undone_at = Some("2026-10-15T09:00:00".into());
        cat.upsert_musiccomplete_run(&undone).unwrap();
        assert_eq!(
            cat.musiccomplete_run("mu-1").unwrap().unwrap().undone_at.as_deref(),
            Some("2026-10-15T09:00:00")
        );
        // Retention prunes the older run.
        let mut run2 = run.clone();
        run2.id = "mu-2".into();
        run2.started = "2026-10-16T03:00:00".into();
        cat.upsert_musiccomplete_run(&run2).unwrap();
        assert_eq!(cat.prune_musiccomplete_runs(1).unwrap(), 1);
        assert!(cat.musiccomplete_run("mu-1").unwrap().is_none());
    }

    #[test]
    fn auditor_runs_and_flags_roundtrip() {
        let cat = Catalog::open_in_memory().unwrap();
        let run = AuditorRunRow {
            id: "run-1".into(),
            schedule_id: None,
            schedule_name: "Manual audit".into(),
            target: "/data".into(),
            started: "2026-10-15T03:00:00".into(),
            finished: None,
            was_late: false,
            status: "running".into(),
            summary_json: "{}".into(),
        };
        cat.upsert_auditor_run(&run).unwrap();
        let mut done = run.clone();
        done.status = "completed".into();
        done.finished = Some("2026-10-15T03:05:00".into());
        done.summary_json = "{\"flags\":2}".into();
        cat.upsert_auditor_run(&done).unwrap();
        let got = cat.auditor_run("run-1").unwrap().unwrap();
        assert_eq!(got.status, "completed");
        assert_eq!(got.summary_json, "{\"flags\":2}");
        assert_eq!(cat.recent_auditor_runs(10).unwrap().len(), 1);

        let flag = |id: &str, path: &str, family: &str| AuditorFlagRow {
            id: id.into(),
            run_id: "run-1".into(),
            path: path.into(),
            family: family.into(),
            lane: "certain".into(),
            class_key: format!("{family}:x"),
            title: "t".into(),
            detail: "d".into(),
            proposal_json: None,
            state: "open".into(),
            created: "2026-10-15T03:01:00".into(),
            updated: "2026-10-15T03:01:00".into(),
        };
        cat.insert_auditor_flag(&flag("f1", "/data/a.txt", "corrupt")).unwrap();
        cat.insert_auditor_flag(&flag("f2", "/data/sub/b.txt", "junk")).unwrap();
        cat.insert_auditor_flag(&flag("f3", "/other/c.txt", "junk")).unwrap();
        assert_eq!(cat.auditor_open_flag_count().unwrap(), 3);
        assert_eq!(cat.auditor_flags(Some("open"), 10).unwrap().len(), 3);
        // Under-target listing respects the folder boundary.
        let under = cat.open_auditor_flags_under("/data").unwrap();
        assert_eq!(under.len(), 2);
        // State transitions stick.
        cat.set_auditor_flag_state("f1", "dismissed", "2026-10-15T04:00:00").unwrap();
        assert_eq!(cat.auditor_open_flag_count().unwrap(), 2);
        assert_eq!(cat.auditor_flag("f1").unwrap().unwrap().state, "dismissed");
        // Retention keeps the newest run only.
        let mut run2 = run.clone();
        run2.id = "run-2".into();
        run2.started = "2026-10-16T03:00:00".into();
        cat.upsert_auditor_run(&run2).unwrap();
        assert_eq!(cat.prune_auditor_runs(1).unwrap(), 1);
        assert!(cat.auditor_run("run-1").unwrap().is_none());
    }

    #[test]
    fn auditor_memory_tables_roundtrip() {
        let cat = Catalog::open_in_memory().unwrap();
        // Dismissals: permanent until explicitly removed.
        assert!(!cat.auditor_is_dismissed("/x/f.txt", "typo").unwrap());
        cat.add_auditor_dismissal("/x/f.txt", "typo", "2026-10-15T03:00:00").unwrap();
        assert!(cat.auditor_is_dismissed("/x/f.txt", "typo").unwrap());
        assert!(!cat.auditor_is_dismissed("/x/f.txt", "junk").unwrap());
        assert_eq!(cat.auditor_dismissals().unwrap().len(), 1);
        cat.delete_auditor_dismissal("/x/f.txt", "typo").unwrap();
        assert!(!cat.auditor_is_dismissed("/x/f.txt", "typo").unwrap());
        // Rules: class-level law.
        assert!(cat.auditor_rule("junk:exe-in-video-folder").unwrap().is_none());
        cat.set_auditor_rule("junk:exe-in-video-folder", "suppress", "2026-10-15T03:00:00").unwrap();
        assert_eq!(
            cat.auditor_rule("junk:exe-in-video-folder").unwrap().as_deref(),
            Some("suppress")
        );
        assert_eq!(cat.auditor_rules().unwrap().len(), 1);
        cat.delete_auditor_rule("junk:exe-in-video-folder").unwrap();
        assert!(cat.auditor_rule("junk:exe-in-video-folder").unwrap().is_none());
        // User words normalize to lowercase.
        cat.add_auditor_user_word("Richrd", "2026-10-15T03:00:00").unwrap();
        assert_eq!(cat.auditor_user_words().unwrap(), vec!["richrd".to_string()]);
        // Hash cache keys on (path, size, mtime) exactly.
        assert!(cat.auditor_hash("/x/a.bin", 10, "m1").unwrap().is_none());
        cat.set_auditor_hash("/x/a.bin", 10, "m1", "abc123").unwrap();
        assert_eq!(cat.auditor_hash("/x/a.bin", 10, "m1").unwrap().as_deref(), Some("abc123"));
        assert!(cat.auditor_hash("/x/a.bin", 11, "m1").unwrap().is_none());
        assert!(cat.auditor_hash("/x/a.bin", 10, "m2").unwrap().is_none());
    }

    #[test]
    fn auditor_clear_findings_wipes_flags_keeps_memory() {
        // Item 13: Clear findings empties the findings list ONLY —
        // dismissal memory and standing rules provably survive.
        let cat = Catalog::open_in_memory().unwrap();
        let flag = |id: &str, state: &str| AuditorFlagRow {
            id: id.into(),
            run_id: "run-1".into(),
            path: format!("/data/{id}.txt"),
            family: "typo".into(),
            lane: "question".into(),
            class_key: "typo:richrd".into(),
            title: "t".into(),
            detail: "d".into(),
            proposal_json: None,
            state: state.into(),
            created: "2026-10-15T03:01:00".into(),
            updated: "2026-10-15T03:01:00".into(),
        };
        cat.insert_auditor_flag(&flag("f1", "open")).unwrap();
        cat.insert_auditor_flag(&flag("f2", "open")).unwrap();
        cat.insert_auditor_flag(&flag("f3", "dismissed")).unwrap();
        cat.add_auditor_dismissal("/data/keep.txt", "typo", "2026-10-15T03:00:00").unwrap();
        cat.set_auditor_rule("typo:richrd", "fix", "2026-10-15T03:00:00").unwrap();
        cat.add_auditor_user_word("garrison", "2026-10-15T03:00:00").unwrap();
        // The clear reports its count (the journal entry's number)
        // and wipes every state of flag.
        assert_eq!(cat.clear_auditor_flags().unwrap(), 3);
        assert_eq!(cat.auditor_flags(None, 10).unwrap().len(), 0);
        assert_eq!(cat.auditor_open_flag_count().unwrap(), 0);
        // Memory survives intact.
        assert!(cat.auditor_is_dismissed("/data/keep.txt", "typo").unwrap());
        assert_eq!(
            cat.auditor_rule("typo:richrd").unwrap().as_deref(),
            Some("fix")
        );
        assert_eq!(cat.auditor_user_words().unwrap(), vec!["garrison".to_string()]);
        // Clearing an already-empty list is a reported no-op.
        assert_eq!(cat.clear_auditor_flags().unwrap(), 0);
    }
}

#[cfg(test)]
mod rule_tests {
    use super::*;
    use std::cell::RefCell;
    use std::sync::atomic::AtomicBool;

    const T1: &str = "2026-10-15T00:00:00Z";
    const T2: &str = "2026-10-16T00:00:00Z";

    fn ts(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    fn seed_location(cat: &Catalog) -> LocationId {
        let loc_id = LocationId::new();
        cat.insert_location(&ufm_vfs::Location {
            id: loc_id,
            provider: "local".into(),
            display_name: "Drive".into(),
            capabilities: ufm_vfs::Capabilities::default(),
            protected: ufm_vfs::ProtectionTier::None,
        })
        .unwrap();
        loc_id
    }

    fn seed_item(cat: &Catalog, loc: LocationId, pk: &str, size: u64, modified: &str) {
        let name = pk.rsplit('/').next().unwrap().to_string();
        cat.insert_item(&ufm_vfs::Item {
            id: ItemId::new(),
            location: loc,
            name,
            kind: ufm_vfs::ItemKind::File,
            size_bytes: Some(size),
            modified: Some(ts(modified)),
            provider_key: Some(pk.to_string()),
            labels: vec![],
            starred: false,
        })
        .unwrap();
    }

    fn music_spec() -> RuleSpec {
        RuleSpec {
            extensions: vec!["mp3".to_string()],
            min_duration_secs: Some(30.0),
        }
    }

    fn online(pk: &str) -> bool {
        !pk.starts_with("/offline")
    }

    /// The standard world: one known member, three unprobed files on
    /// a mounted volume (a future member, a no-duration, a short),
    /// one unprobed file on an offline volume, one non-candidate.
    struct World {
        cat: Catalog,
        loc: LocationId,
    }

    fn world() -> World {
        let cat = Catalog::open_in_memory().unwrap();
        let loc = seed_location(&cat);
        seed_item(&cat, loc, "/music/known.mp3", 100, T1);
        cat.set_duration("/music/known.mp3", 100.0).unwrap();
        seed_item(&cat, loc, "/music/song-a.mp3", 100, T1);
        seed_item(&cat, loc, "/music/song-b.mp3", 100, T1);
        seed_item(&cat, loc, "/music/short.mp3", 100, T1);
        seed_item(&cat, loc, "/offline/song-c.mp3", 100, T1);
        seed_item(&cat, loc, "/music/notes.txt", 10, T1);
        World { cat, loc }
    }

    /// A probe with scripted outcomes that records every path it was
    /// asked about — the record proves settled files are never
    /// re-probed and offline files are never probed at all.
    fn scripted_probe(
        log: &RefCell<Vec<String>>,
    ) -> impl FnMut(&std::path::Path) -> ProbeOutcome + '_ {
        move |p: &std::path::Path| {
            let s = p.to_string_lossy().into_owned();
            log.borrow_mut().push(s.clone());
            match s.as_str() {
                "/music/song-a.mp3" => ProbeOutcome::Duration(200.0),
                "/music/short.mp3" => ProbeOutcome::Duration(10.0),
                "/music/song-b.mp3" => ProbeOutcome::NoDuration,
                _ => ProbeOutcome::Missing,
            }
        }
    }

    #[test]
    fn insert_item_preserves_duration_until_file_changes() {
        let cat = Catalog::open_in_memory().unwrap();
        let loc = seed_location(&cat);
        seed_item(&cat, loc, "/music/track.mp3", 100, T1);
        cat.set_duration("/music/track.mp3", 212.0).unwrap();
        let dur = |c: &Catalog| {
            c.rule_candidates(&["mp3".to_string()]).unwrap()[0]
                .1
        };
        assert_eq!(dur(&cat), Some(212.0));
        // A re-walk re-inserts the UNCHANGED file (fresh ItemId, same
        // size + mtime): the probed duration must survive.
        seed_item(&cat, loc, "/music/track.mp3", 100, T1);
        assert_eq!(dur(&cat), Some(212.0), "unchanged file keeps its duration");
        // The file changed (retagging rewrites bytes + mtime): the
        // old duration described other bytes — unprobed again.
        seed_item(&cat, loc, "/music/track.mp3", 101, T2);
        assert_eq!(dur(&cat), None, "changed file loses its stale duration");
    }

    #[test]
    fn rule_refresh_classifies_and_persists() {
        let w = world();
        let spec = music_spec();
        let st = w.cat.rule_refresh(&spec, &online, T1).unwrap();
        assert_eq!(st.candidates, 5);
        assert_eq!(st.member_count, 1);
        assert_eq!(st.members.len(), 1);
        assert_eq!(
            st.members[0].provider_key.as_deref(),
            Some("/music/known.mp3")
        );
        assert_eq!(st.pending, 3, "short + song-a + song-b await probing");
        assert_eq!(st.pending_offline, 1, "song-c sits on the offline volume");
        assert!(!st.sweep_complete);
        // The snapshot reads back identically with no re-evaluation:
        // this is the instant paint a revisit gets.
        let snap = w
            .cat
            .rule_snapshot(&spec)
            .unwrap()
            .expect("snapshot stored");
        assert_eq!(snap.member_count, 1);
        assert_eq!(snap.pending, 3);
        assert_eq!(snap.pending_offline, 1);
        assert_eq!(snap.evaluated_at.as_deref(), Some(T1));
    }

    #[test]
    fn rule_probe_batch_streams_persists_and_resumes() {
        let w = world();
        let spec = music_spec();
        w.cat.rule_refresh(&spec, &online, T1).unwrap();
        let log = RefCell::new(Vec::new());
        let mut probe = scripted_probe(&log);
        let cancel = AtomicBool::new(false);
        // Budget 2 — name order settles short.mp3 (10s: not a member)
        // and then song-a.mp3 (200s: member, streamed back).
        let b1 = w
            .cat
            .rule_probe_batch(&spec, 2, &mut probe, &online, &cancel)
            .unwrap();
        assert_eq!(b1.probed, 2);
        assert_eq!(b1.remaining, 1, "only song-b remains");
        assert_eq!(b1.remaining_offline, 1);
        assert_eq!(b1.new_members.len(), 1);
        assert_eq!(
            b1.new_members[0].provider_key.as_deref(),
            Some("/music/song-a.mp3")
        );
        assert!(!b1.cancelled && b1.probe_error.is_none());
        // Partial progress is durable the moment the batch ends.
        let snap = w.cat.rule_snapshot(&spec).unwrap().unwrap();
        assert_eq!(snap.member_count, 2);
        assert_eq!(snap.pending, 1);
        // Resume: only the unsettled file is probed. Settled files
        // are never asked again, offline files never at all.
        let b2 = w
            .cat
            .rule_probe_batch(&spec, 10, &mut probe, &online, &cancel)
            .unwrap();
        assert_eq!(b2.probed, 1);
        assert_eq!(b2.remaining, 0);
        assert!(b2.new_members.is_empty(), "song-b has no duration");
        let asked = log.borrow().clone();
        assert_eq!(
            asked,
            vec![
                "/music/short.mp3".to_string(),
                "/music/song-a.mp3".to_string(),
                "/music/song-b.mp3".to_string(),
            ]
        );
        let snap = w.cat.rule_snapshot(&spec).unwrap().unwrap();
        assert!(snap.sweep_complete);
        assert_eq!(snap.member_count, 2);
    }

    #[test]
    fn rule_probe_batch_honours_cancellation() {
        let w = world();
        let spec = music_spec();
        w.cat.rule_refresh(&spec, &online, T1).unwrap();
        let log = RefCell::new(Vec::new());
        let mut probe = scripted_probe(&log);
        let cancel = AtomicBool::new(true); // the user already left
        let b = w
            .cat
            .rule_probe_batch(&spec, 250, &mut probe, &online, &cancel)
            .unwrap();
        assert_eq!(b.probed, 0);
        assert!(b.cancelled);
        assert_eq!(b.remaining, 3);
        assert!(log.borrow().is_empty(), "a cancelled sweep probes nothing");
    }

    #[test]
    fn rule_result_survives_reopen() {
        let path =
            std::env::temp_dir().join(format!("ufm-rule-reopen-{}.db", std::process::id()));
        let cleanup = |path: &std::path::Path| {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
            }
        };
        cleanup(&path);
        let spec = music_spec();
        {
            let cat = Catalog::open(&path).unwrap();
            let loc = seed_location(&cat);
            seed_item(&cat, loc, "/music/known.mp3", 100, T1);
            cat.set_duration("/music/known.mp3", 100.0).unwrap();
            seed_item(&cat, loc, "/music/song-a.mp3", 100, T1);
            let st = cat.rule_refresh(&spec, &online, T1).unwrap();
            assert_eq!(st.member_count, 1);
            assert_eq!(st.pending, 1);
        }
        // A brand-new process (fresh Catalog on the same file) paints
        // the remembered result without evaluating anything.
        let cat = Catalog::open(&path).unwrap();
        let snap = cat
            .rule_snapshot(&spec)
            .unwrap()
            .expect("the remembered result must survive a restart");
        assert_eq!(snap.member_count, 1);
        assert_eq!(snap.pending, 1);
        assert_eq!(
            snap.members[0].provider_key.as_deref(),
            Some("/music/known.mp3")
        );
        drop(cat);
        cleanup(&path);
    }

    #[test]
    fn rule_refresh_applies_deltas() {
        let w = world();
        let spec = music_spec();
        let log = RefCell::new(Vec::new());
        let mut probe = scripted_probe(&log);
        let cancel = AtomicBool::new(false);
        w.cat.rule_refresh(&spec, &online, T1).unwrap();
        w.cat
            .rule_probe_batch(&spec, 10, &mut probe, &online, &cancel)
            .unwrap();
        let settled = w.cat.rule_snapshot(&spec).unwrap().unwrap();
        assert_eq!(settled.member_count, 2);
        assert!(settled.sweep_complete);
        // Delta 1: a NEW file arrives with its duration already known
        // (probed by another door) — member at refresh, no probing.
        seed_item(&w.cat, w.loc, "/music/song-d.mp3", 100, T1);
        w.cat.set_duration("/music/song-d.mp3", 55.0).unwrap();
        // Delta 2: song-a is retagged (size + mtime change) — its
        // duration is dropped by insert_item, so it must re-queue.
        seed_item(&w.cat, w.loc, "/music/song-a.mp3", 104, T2);
        // Delta 3: short.mp3 is deleted; the indexer's prune removes
        // its catalog row.
        let seen: std::collections::HashSet<String> = [
            "/music/known.mp3",
            "/music/song-a.mp3",
            "/music/song-b.mp3",
            "/music/song-d.mp3",
            "/music/notes.txt",
            "/offline/song-c.mp3",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        w.cat.delete_unseen(&w.loc, &seen).unwrap();
        // The revisit evaluates ONLY the deltas.
        let st = w.cat.rule_refresh(&spec, &online, T2).unwrap();
        assert_eq!(st.candidates, 5, "5 before + song-d − short = 5");
        assert_eq!(st.pending, 1, "only the changed song-a re-queues");
        assert_eq!(st.pending_offline, 1);
        let names: Vec<&str> = st
            .members
            .iter()
            .filter_map(|i| i.provider_key.as_deref())
            .collect();
        assert!(names.contains(&"/music/known.mp3"));
        assert!(names.contains(&"/music/song-d.mp3"));
        assert!(
            !names.contains(&"/music/song-a.mp3"),
            "a changed file leaves membership until it is re-probed"
        );
        assert!(!names.contains(&"/music/short.mp3"));
        // The re-probe heals the delta and completes the sweep again.
        let b = w
            .cat
            .rule_probe_batch(&spec, 10, &mut probe, &online, &cancel)
            .unwrap();
        assert_eq!(b.probed, 1);
        assert_eq!(b.new_members.len(), 1);
        let snap = w.cat.rule_snapshot(&spec).unwrap().unwrap();
        assert!(snap.sweep_complete);
        assert_eq!(snap.member_count, 3);
    }
}

// ---------------------------------------------------------------------------
// ROM Home tests (2026-10-15): shelf queries, counts, per-game state.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod rom_home_tests {
    use super::*;
    use ufm_vfs::{Item, ItemKind};

    fn sample_location(id: LocationId) -> ufm_vfs::Location {
        ufm_vfs::Location {
            id,
            provider: "local".into(),
            display_name: "test".into(),
            capabilities: ufm_vfs::Capabilities {
                read: true,
                ..Default::default()
            },
            protected: ufm_vfs::ProtectionTier::None,
        }
    }

    fn named_item(location: LocationId, name: &str, kind: ItemKind) -> Item {
        Item {
            id: ItemId::new(),
            location,
            name: name.into(),
            kind,
            size_bytes: Some(2048),
            modified: Some(
                chrono::DateTime::parse_from_rfc3339("2026-10-01T12:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            ),
            provider_key: Some(format!("/fake/{name}")),
            labels: vec![],
            starred: false,
        }
    }

    fn rom_item(cat: &Catalog, loc: LocationId, name: &str, pk: &str) -> Item {
        let mut it = named_item(loc, name, ItemKind::File);
        it.provider_key = Some(pk.into());
        cat.insert_item(&it).unwrap();
        it
    }

    fn seed_library(cat: &Catalog) -> (Item, Item, Item) {
        let loc = LocationId::new();
        cat.insert_location(&sample_location(loc)).unwrap();
        // NES with box art found; SNES without; a MAME set that is
        // also Neo Geo, without art.
        let nes = rom_item(cat, loc, "1942 (JU).nes", "/roms/nes/1942.nes");
        cat.upsert_media_identity(
            &nes.id,
            "rom",
            Some("1942 (Japan, USA) (En)"),
            Some("nes"),
            Some("aaaa"),
            r#"{"matched":true,"also":[]}"#,
        )
        .unwrap();
        cat.upsert_rom_media(
            &nes.id.to_string(),
            "boxart",
            "found",
            Some("libretro"),
            Some("/media/nes/1942.png"),
            "{}",
        )
        .unwrap();
        let snes = rom_item(cat, loc, "smw.sfc", "/roms/snes/smw.sfc");
        cat.upsert_media_identity(
            &snes.id,
            "rom",
            Some("Super Mario World (USA)"),
            Some("snes"),
            Some("bbbb"),
            r#"{"matched":true,"also":[]}"#,
        )
        .unwrap();
        cat.upsert_rom_media(
            &snes.id.to_string(),
            "boxart",
            "missing",
            None,
            None,
            "{}",
        )
        .unwrap();
        let mame = rom_item(cat, loc, "mslug.zip", "/roms/arcade/mslug.zip");
        cat.upsert_media_identity(
            &mame.id,
            "rom",
            Some("Metal Slug - Super Vehicle-001"),
            Some("arcade"),
            Some("cccc"),
            r#"{"matched":true,"also":["neogeo"]}"#,
        )
        .unwrap();
        (nes, snes, mame)
    }

    #[test]
    fn needs_id_scoped_surfaces_scanned_files_and_keeps_the_noise_gate() {
        let cat = Catalog::open_in_memory().unwrap();
        let loc = LocationId::new();
        cat.insert_location(&sample_location(loc)).unwrap();
        // An identified game: its folder becomes ROM territory.
        let nes = rom_item(&cat, loc, "1942 (JU).nes", "/roms/nes/1942.nes");
        cat.upsert_media_identity(
            &nes.id,
            "rom",
            Some("1942 (Japan, USA) (En)"),
            Some("nes"),
            Some("aaaa"),
            r#"{"matched":true,"also":[]}"#,
        )
        .unwrap();
        // Scanned-and-failed .bin in a random folder (the Klax
        // evidence shape): judged ROM-kind, matched nothing. It
        // must surface in Needs Identification — by path it is in
        // no ROM territory at all.
        let song = rom_item(
            &cat,
            loc,
            "song-167.bin",
            "/home/user/klax/evidence/audio/song-167.bin",
        );
        cat.upsert_media_identity(
            &song.id,
            "rom",
            None,
            None,
            None,
            r#"{"matched":false,"also":[]}"#,
        )
        .unwrap();
        // Never-scanned .bin in a random folder: stays out — the
        // noise gate for files no pass has ever judged.
        let _stray = rom_item(&cat, loc, "stray.bin", "/home/user/random/stray.bin");
        // Never-scanned .bin inside ROM territory: lists.
        let _loose = rom_item(&cat, loc, "loose.bin", "/roms/loose.bin");
        // A FOLDER named like a ROM file, identity row and all:
        // the candidates query only takes files.
        let mut folder = named_item(loc, "fake.bin", ItemKind::Folder);
        folder.provider_key = Some("/home/user/klax/evidence/audio/fake.bin".into());
        cat.insert_item(&folder).unwrap();
        cat.upsert_media_identity(
            &folder.id,
            "rom",
            None,
            None,
            None,
            r#"{"matched":false,"also":[]}"#,
        )
        .unwrap();

        let exts = vec!["bin".to_string(), "nes".to_string()];
        let scoped = cat.rom_needs_id_scoped(&exts).unwrap();
        let keys: Vec<&str> = scoped
            .iter()
            .filter_map(|(i, _)| i.provider_key.as_deref())
            .collect();
        assert!(keys.contains(&"/home/user/klax/evidence/audio/song-167.bin"));
        assert!(keys.contains(&"/roms/loose.bin"));
        assert!(!keys.contains(&"/home/user/random/stray.bin"));
        assert!(!keys.contains(&"/home/user/klax/evidence/audio/fake.bin"));
        assert!(!keys.contains(&"/roms/nes/1942.nes"));
        // The scanned flag rides through: the song was scanned,
        // the loose file was not.
        let flag = |pk: &str| {
            scoped
                .iter()
                .find(|(i, _)| i.provider_key.as_deref() == Some(pk))
                .map(|(_, h)| *h)
        };
        assert_eq!(
            flag("/home/user/klax/evidence/audio/song-167.bin"),
            Some(true)
        );
        assert_eq!(flag("/roms/loose.bin"), Some(false));
    }

    #[test]
    fn rom_home_counts_split_per_system_and_missing() {
        let cat = Catalog::open_in_memory().unwrap();
        seed_library(&cat);
        let counts = cat.rom_home_counts().unwrap();
        assert_eq!(counts.total, 3);
        // SNES (missing row) + MAME (no row at all) lack box art.
        assert_eq!(counts.missing_media, 2);
        let per: std::collections::HashMap<&str, (i64, i64)> = counts
            .per_system
            .iter()
            .map(|(p, g, m)| (p.as_str(), (*g, *m)))
            .collect();
        assert_eq!(per["nes"], (1, 0));
        assert_eq!(per["snes"], (1, 1));
        assert_eq!(per["arcade"], (1, 1));
        // The cross-listed set counts on the Neo Geo shelf too.
        assert_eq!(per["neogeo"], (1, 1));
    }

    #[test]
    fn rom_home_games_carry_media_state_and_region() {
        let cat = Catalog::open_in_memory().unwrap();
        let (nes, _snes, _mame) = seed_library(&cat);
        let all = cat.rom_home_games(None).unwrap();
        assert_eq!(all.len(), 3);
        let g = all.iter().find(|g| g.platform == "nes").unwrap();
        assert_eq!(g.title, "1942 (Japan, USA) (En)");
        assert_eq!(g.region.as_deref(), Some("Japan, USA"));
        assert_eq!(g.sha1.as_deref(), Some("aaaa"));
        let art = g
            .media
            .iter()
            .find(|m| m.media_type == "boxart")
            .expect("boxart row");
        assert!(art.found);
        assert_eq!(art.local_path.as_deref(), Some("/media/nes/1942.png"));
        // Shelf membership: the cross-listed set is on both shelves.
        assert_eq!(cat.rom_home_games(Some("neogeo")).unwrap().len(), 1);
        assert_eq!(cat.rom_home_games(Some("arcade")).unwrap().len(), 1);
        assert_eq!(cat.rom_home_games(Some("nes")).unwrap().len(), 1);
        assert!(cat.rom_home_games(Some("genesis")).unwrap().is_empty());
        // The item identity survives the round trip.
        let g2 = cat.rom_home_games(Some("nes")).unwrap();
        assert_eq!(g2[0].item.id, nes.id);
    }

    #[test]
    fn rom_home_generation_grouping_inputs_are_stable() {
        // The rail groups by generation from the UI's system table;
        // the catalog side must hand back every platform it knows,
        // sorted — the counts test above pins membership; this pins
        // that a platform with games never vanishes from the split.
        let cat = Catalog::open_in_memory().unwrap();
        seed_library(&cat);
        let counts = cat.rom_home_counts().unwrap();
        let names: Vec<&str> = counts.per_system.iter().map(|(p, _, _)| p.as_str()).collect();
        assert!(names.contains(&"nes"));
        assert!(names.contains(&"snes"));
        assert!(names.contains(&"arcade"));
        assert!(names.contains(&"neogeo"));
    }

    #[test]
    fn rom_game_state_round_trip_and_play() {
        let cat = Catalog::open_in_memory().unwrap();
        let pk = "/roms/nes/1942.nes";
        // Untouched: the honest default.
        let st = cat.rom_game_state_for(pk).unwrap();
        assert_eq!(st, RomGameState::default());
        cat.rom_game_set_rating(pk, Some(4)).unwrap();
        cat.rom_game_set_favorite(pk, true).unwrap();
        cat.rom_game_set_completed(pk, true).unwrap();
        cat.rom_game_set_overrides(pk, Some("My 1942"), Some("Shooter"), Some("2"), Some("beat it"))
            .unwrap();
        let st = cat.rom_game_state_for(pk).unwrap();
        assert_eq!(st.rating, Some(4));
        assert!(st.favorite);
        assert!(st.completed);
        assert_eq!(st.title_override.as_deref(), Some("My 1942"));
        assert_eq!(st.genre.as_deref(), Some("Shooter"));
        assert_eq!(st.players.as_deref(), Some("2"));
        // Rating clamps and clears.
        cat.rom_game_set_rating(pk, Some(9)).unwrap();
        assert_eq!(cat.rom_game_state_for(pk).unwrap().rating, Some(5));
        cat.rom_game_set_rating(pk, None).unwrap();
        assert_eq!(cat.rom_game_state_for(pk).unwrap().rating, None);
        // Empty override strings clear back to absent.
        cat.rom_game_set_overrides(pk, Some("  "), None, None, None).unwrap();
        let st = cat.rom_game_state_for(pk).unwrap();
        assert_eq!(st.title_override, None);
        assert_eq!(st.genre, None);
        // Play accrues.
        let st = cat.rom_game_note_play(pk).unwrap();
        assert_eq!(st.play_count, 1);
        assert!(st.last_played.is_some());
        let st = cat.rom_game_note_play(pk).unwrap();
        assert_eq!(st.play_count, 2);
        // State surfaces through the games query.
        let loc = LocationId::new();
        cat.insert_location(&sample_location(loc)).unwrap();
        let it = rom_item(&cat, loc, "1942 (JU).nes", pk);
        cat.upsert_media_identity(
            &it.id,
            "rom",
            Some("1942 (Japan, USA) (En)"),
            Some("nes"),
            None,
            r#"{"matched":true,"also":[]}"#,
        )
        .unwrap();
        let games = cat.rom_home_games(Some("nes")).unwrap();
        assert_eq!(games.len(), 1);
        assert!(games[0].state.favorite);
        assert_eq!(games[0].state.play_count, 2);
    }

    #[test]
    fn audio_play_state_round_trip() {
        let cat = Catalog::open_in_memory().unwrap();
        let pk = "/music/Kind of Blue/So What.flac";
        // Default state for an untouched track.
        let st = cat.audio_play_state_for(pk).unwrap();
        assert_eq!(st.play_count, 0);
        assert!(st.last_played_at.is_none());
        assert_eq!(st.completed_count, 0);
        // Plays accrue; positions persist; completion counts and
        // rewinds the saved position.
        let st = cat.audio_note_play(pk).unwrap();
        assert_eq!(st.play_count, 1);
        assert!(st.last_played_at.is_some());
        cat.audio_set_position(pk, 123.5, false).unwrap();
        let st = cat.audio_play_state_for(pk).unwrap();
        assert_eq!(st.last_position_secs, 123.5);
        cat.audio_set_position(pk, 0.0, true).unwrap();
        let st = cat.audio_play_state_for(pk).unwrap();
        assert_eq!(st.completed_count, 1);
        assert_eq!(st.last_position_secs, 0.0);
        // Idempotent reads: noting another play keeps history sane,
        // and Recently Played surfaces the track.
        cat.audio_note_play(pk).unwrap();
        let recent = cat.audio_recently_played(10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].provider_key, pk);
        assert_eq!(recent[0].state.play_count, 2);
        // A never-played track never appears in Recently Played.
        cat.audio_set_position("/music/other.flac", 10.0, false).unwrap();
        let recent = cat.audio_recently_played(10).unwrap();
        assert_eq!(recent.len(), 1);
    }

    #[test]
    fn rom_group_state_backfill_merges_donors_and_spares_orphans() {
        // Pre-grouping world: two dumps of one game rated on their
        // paths before either is identified, plus an orphan path
        // that never identifies. The backfill must merge the donors
        // into ONE group row, newest rating winning, favorite
        // OR-merged, play history left per-path, orphan untouched.
        let cat = Catalog::open_in_memory().unwrap();
        let usa = "/roms/nes/klax-usa.nes";
        let japan = "/roms/nes/klax-japan.nes";
        let orphan = "/roms/nes/mystery.nes";
        cat.rom_game_set_rating(usa, Some(5)).unwrap();
        cat.rom_game_set_favorite(usa, true).unwrap();
        cat.rom_game_set_rating(japan, Some(3)).unwrap();
        cat.rom_game_set_rating(orphan, Some(2)).unwrap();
        cat.rom_game_note_play(usa).unwrap();
        cat.rom_game_note_play(usa).unwrap();
        // Deterministic donor order: USA older, Japan newer.
        cat.conn
            .execute(
                "UPDATE rom_game_state SET updated_at = '2026-01-01T00:00:00Z' WHERE provider_key = ?1",
                rusqlite::params![usa],
            )
            .unwrap();
        cat.conn
            .execute(
                "UPDATE rom_game_state SET updated_at = '2026-06-01T00:00:00Z' WHERE provider_key = ?1",
                rusqlite::params![japan],
            )
            .unwrap();
        // Now the dumps identify as one game.
        let loc = LocationId::new();
        cat.insert_location(&sample_location(loc)).unwrap();
        let it_usa = rom_item(&cat, loc, "Klax (USA) (Unl).nes", usa);
        cat.upsert_media_identity(
            &it_usa.id, "rom", Some("Klax (USA)"), Some("nes"), None,
            r#"{"matched":true,"also":[],"crc":111}"#,
        )
        .unwrap();
        let it_jp = rom_item(&cat, loc, "Klax (Japan).nes", japan);
        cat.upsert_media_identity(
            &it_jp.id, "rom", Some("Klax (Japan)"), Some("nes"), None,
            r#"{"matched":true,"also":[],"crc":222}"#,
        )
        .unwrap();
        cat.backfill_rom_group_state().unwrap();
        // One group row; both paths see the merged game state…
        let group_rows: i64 = cat
            .conn
            .query_row("SELECT COUNT(*) FROM rom_group_state", [], |r| r.get(0))
            .unwrap();
        assert_eq!(group_rows, 1);
        for pk in [usa, japan] {
            let st = cat.rom_game_state_for(pk).unwrap();
            assert_eq!(st.rating, Some(3), "newest donor rating wins");
            assert!(st.favorite, "favorite OR-merges");
        }
        // …while play history stays per-path.
        assert_eq!(cat.rom_game_state_for(usa).unwrap().play_count, 2);
        assert_eq!(cat.rom_game_state_for(japan).unwrap().play_count, 0);
        // The orphan keeps its own state and gains no group.
        assert_eq!(cat.rom_game_state_for(orphan).unwrap().rating, Some(2));
        assert_eq!(cat.rom_group_key_for_path(orphan).unwrap(), None);
        // Idempotent: a second backfill changes nothing.
        cat.backfill_rom_group_state().unwrap();
        let group_rows: i64 = cat
            .conn
            .query_row("SELECT COUNT(*) FROM rom_group_state", [], |r| r.get(0))
            .unwrap();
        assert_eq!(group_rows, 1);
        // New writes land on the group: unfavorite via Japan clears
        // it for USA too (the group is the source of truth).
        cat.rom_game_set_favorite(japan, false).unwrap();
        assert!(!cat.rom_game_state_for(usa).unwrap().favorite);
        // The games query composes the same truth, and both dumps
        // carry one group key with distinct variants + CRCs.
        let games = cat.rom_home_games(Some("nes")).unwrap();
        assert_eq!(games.len(), 2);
        assert_eq!(games[0].group_key, games[1].group_key);
        assert!(games.iter().all(|g| !g.state.favorite));
        let crcs: Vec<&str> = games.iter().filter_map(|g| g.crc.as_deref()).collect();
        assert_eq!(crcs.len(), 2);
        assert_ne!(
            games[0].variant.descriptor, games[1].variant.descriptor,
            "USA vs Japan variants differ"
        );
        // Counts speak in games now: 2 files, 1 group.
        let counts = cat.rom_home_counts().unwrap();
        assert_eq!(counts.total, 1);
        assert_eq!(counts.per_system, vec![("nes".to_string(), 1, 1)]);
    }

    /// Collection Completeness real-catalog census (2026-10-17):
    /// env-gated diagnostic — runs the SHIPPED engine over a copy
    /// of the real catalog plus the bundled dataset and prints the
    /// per-system table. Ignored by default so the gate count is
    /// untouched; the census run executes it explicitly.
    #[test]
    #[ignore]
    fn completeness_real_catalog_census() {
        let cat_path = std::env::var("CC_CENSUS_CATALOG").expect("CC_CENSUS_CATALOG set");
        let dat_path = std::env::var("CC_DATASET").expect("CC_DATASET set");
        let cat = Catalog::open(std::path::Path::new(&cat_path)).unwrap();
        let bytes = std::fs::read(&dat_path).unwrap();
        let ds = ufm_media::romdat::RomDataset::load(bytes).unwrap();
        let index = ufm_media::completeness::ReferenceIndex::build(&ds);
        let pairs = cat.rom_completeness_owned().unwrap();
        let owned = ufm_media::completeness::owned_group_keys(&pairs);
        let summaries = ufm_media::completeness::summarize(&index, &owned);
        let ext_census = cat.rom_completeness_ext_census().unwrap();
        println!("CENSUS pairs={} systems={}", pairs.len(), summaries.len());
        for s in &summaries {
            println!(
                "CENSUS {} owned={} total={} missing={} releases={} unmatched={} hasref={}",
                s.key, s.owned_groups, s.total_groups, s.missing_groups,
                s.total_releases, s.owned_unmatched, s.has_reference
            );
        }
        for (p, e, n) in &ext_census {
            println!("CENSUS-EXT {p} {e} {n}");
        }
    }

    #[test]
    fn completeness_owned_pairs_expand_also_and_census_extensions() {
        let cat = Catalog::open_in_memory().unwrap();
        let loc = LocationId::new();
        cat.insert_location(&sample_location(loc)).unwrap();
        // A MAME set that is also Neo Geo: one identity, two shelves.
        let it_set = rom_item(&cat, loc, "alph.zip", "/roms/arcade/alph.zip");
        cat.upsert_media_identity(
            &it_set.id, "rom", Some("Alpha Fighters (World)"), Some("arcade"), None,
            r#"{"matched":true,"also":["neogeo"],"crc":0}"#,
        )
        .unwrap();
        let it_nes = rom_item(&cat, loc, "Klax (USA).nes", "/roms/nes/Klax (USA).nes");
        cat.upsert_media_identity(
            &it_nes.id, "rom", Some("Klax (USA)"), Some("nes"), None,
            r#"{"matched":true,"also":[],"crc":111}"#,
        )
        .unwrap();
        let it_zip = rom_item(&cat, loc, "Klax (Japan).zip", "/roms/nes/Klax (Japan).zip");
        cat.upsert_media_identity(
            &it_zip.id, "rom", Some("Klax (Japan)"), Some("nes"), None,
            r#"{"matched":true,"also":[],"crc":222}"#,
        )
        .unwrap();
        // An unmatched candidate (no title/platform) never appears.
        let it_un = rom_item(&cat, loc, "Mystery.nes", "/roms/nes/Mystery.nes");
        cat.upsert_media_identity(
            &it_un.id, "rom", None, None, None, r#"{"matched":false,"also":[]}"#,
        )
        .unwrap();
        // A PC game never appears — it has no DAT universe.
        let it_pc = rom_item(&cat, loc, "Game", "/pc/Game");
        cat.upsert_media_identity(
            &it_pc.id, "pc-game", Some("Some PC Game"), Some("pc"), None, "{}",
        )
        .unwrap();

        let mut pairs = cat.rom_completeness_owned().unwrap();
        pairs.sort();
        assert_eq!(
            pairs,
            vec![
                ("arcade".to_string(), "Alpha Fighters (World)".to_string()),
                ("neogeo".to_string(), "Alpha Fighters (World)".to_string()),
                ("nes".to_string(), "Klax (Japan)".to_string()),
                ("nes".to_string(), "Klax (USA)".to_string()),
            ]
        );
        let mut census = cat.rom_completeness_ext_census().unwrap();
        census.sort();
        assert_eq!(
            census,
            vec![
                ("arcade".to_string(), ".zip".to_string(), 1),
                ("nes".to_string(), ".nes".to_string(), 1),
                ("nes".to_string(), ".zip".to_string(), 1),
            ]
        );
    }

    #[test]
    fn rom_home_favorites_and_recents_inputs() {
        // Favorites / Recently Played shelves are UI derivations of
        // the state the games query returns — pin that the inputs
        // (favorite flag, last_played ordering data) round-trip.
        let cat = Catalog::open_in_memory().unwrap();
        seed_library(&cat);
        cat.rom_game_set_favorite("/roms/snes/smw.sfc", true).unwrap();
        cat.rom_game_note_play("/roms/nes/1942.nes").unwrap();
        let all = cat.rom_home_games(None).unwrap();
        let favs: Vec<&str> = all
            .iter()
            .filter(|g| g.state.favorite)
            .map(|g| g.platform.as_str())
            .collect();
        assert_eq!(favs, vec!["snes"]);
        let played: Vec<&str> = all
            .iter()
            .filter(|g| g.state.last_played.is_some())
            .map(|g| g.platform.as_str())
            .collect();
        assert_eq!(played, vec!["nes"]);
    }

    #[test]
    fn region_of_title_reads_only_region_groups() {
        assert_eq!(
            region_of_title("1943 - The Battle of Midway (USA)").as_deref(),
            Some("USA")
        );
        assert_eq!(
            region_of_title("1942 (Japan, USA) (En)").as_deref(),
            Some("Japan, USA")
        );
        assert_eq!(
            region_of_title("Game (Europe) (Rev 1)").as_deref(),
            Some("Europe")
        );
        assert_eq!(region_of_title("Game (Rev 1) (Pirate)"), None);
        assert_eq!(region_of_title("Game"), None);
        // A non-region group first doesn't hide the region behind it.
        assert_eq!(
            region_of_title("Game (En) (World)").as_deref(),
            Some("World")
        );
    }

    #[test]
    fn smart_collection_stores_its_sentence() {
        let cat = Catalog::open_in_memory().unwrap();
        let id = CollectionId::new();
        let q = r#"{"genres":["Platformer"],"players":"2+","hasBoxArt":true}"#;
        cat.create_smart_collection(&id, "Platformers for two", q).unwrap();
        assert_eq!(
            cat.smart_collection_query(&id).unwrap().as_deref(),
            Some(q)
        );
        // The rule is editable in place (the room's chip editor).
        cat.update_smart_collection(&id.to_string(), r#"{"genres":["RPG"]}"#)
            .unwrap();
        assert_eq!(
            cat.smart_collection_query(&id).unwrap().as_deref(),
            Some(r#"{"genres":["RPG"]}"#)
        );
        // A manual collection is not a smart one.
        let mid = CollectionId::new();
        cat.create_collection(&mid, "Manual", "manual").unwrap();
        assert!(cat.smart_collection_query(&mid).unwrap().is_none());
    }

    // -----------------------------------------------------------------
    // EmulationStation import (Hyperbase #5): the precedence contract.
    // -----------------------------------------------------------------

    fn es_input(item: &ufm_vfs::Item, pk: &str) -> EsApplyInput {
        EsApplyInput {
            item_id: item.id,
            provider_key: pk.to_string(),
            es_system: "snes".to_string(),
            platform: "snes".to_string(),
            gamelist_path: "/roms/snes/gamelist.xml".to_string(),
            name: Some("Gamelist Name".to_string()),
            desc: Some("A description.".to_string()),
            developer: Some("Dev".to_string()),
            publisher: Some("Pub".to_string()),
            genre: Some("Platform".to_string()),
            family: None,
            players: Some("1".to_string()),
            lang: None,
            region: Some("USA".to_string()),
            release_year: Some(1992),
            releasedate_raw: Some("19921101T000000".to_string()),
            source_id: Some("2424".to_string()),
            scrap_source: Some("ScreenScraper".to_string()),
            scrap_date: None,
            md5: None,
            crc32: None,
            hidden: false,
            kidgame: false,
            gametime_secs: Some(3600),
            rating_stars: Some(4),
            favorite: true,
            playcount: Some(6),
            last_played_rfc3339: Some("2023-08-07T11:46:07Z".to_string()),
            boxart_path: Some("/roms/snes/images/Game-image.png".to_string()),
            boxart_kind: Some("image".to_string()),
            trailer_path: None,
            screenshot_path: None,
            image_path: Some("/roms/snes/images/Game-image.png".to_string()),
            marquee_path: None,
            thumbnail_path: None,
            fanart_path: None,
            manual_path: None,
            item_size: 12345,
            item_modified_rfc3339: Some("2026-01-01T00:00:00Z".to_string()),
        }
    }

    fn es_test_item(cat: &Catalog, loc: LocationId, pk: &str) -> ufm_vfs::Item {
        let mut item = named_item(loc, "Game (USA).zip", ItemKind::File);
        item.provider_key = Some(pk.to_string());
        cat.insert_item(&item).unwrap();
        item
    }

    #[test]
    fn es_apply_writes_identity_metadata_art_and_state() {
        let cat = Catalog::open_in_memory().unwrap();
        let loc = LocationId::new();
        cat.insert_location(&sample_location(loc)).unwrap();
        let pk = "/roms/snes/Game (USA).zip";
        let item = es_test_item(&cat, loc, pk);
        let out = cat.es_apply(&es_input(&item, pk)).unwrap();
        assert_eq!(out.identity, EsIdentityOutcome::GamelistWritten);
        assert_eq!(out.art_filled, vec!["boxart".to_string()]);
        // Identity: gamelist title + platform + year, gamelist evidence
        // carrying the freshness facts the identify pass checks.
        let row = cat.media_identity_for(&item.id).unwrap().unwrap();
        assert_eq!(row.title.as_deref(), Some("Gamelist Name"));
        assert_eq!(row.platform.as_deref(), Some("snes"));
        assert_eq!(row.year, Some(1992));
        let ev: serde_json::Value = serde_json::from_str(&row.evidence_json).unwrap();
        assert_eq!(ev["source"], "gamelist");
        assert_eq!(ev["size"], 12345);
        assert_eq!(ev["modified"], "2026-01-01T00:00:00Z");
        // Descriptive provenance landed whole.
        let meta = cat.es_metadata_for(pk).unwrap().unwrap();
        assert_eq!(meta.description.as_deref(), Some("A description."));
        assert_eq!(meta.developer.as_deref(), Some("Dev"));
        assert_eq!(meta.release_year, Some(1992));
        assert_eq!(meta.gametime_secs, Some(3600));
        // Art + state landed in their real homes.
        let art = cat.rom_media_art(&[item.id.to_string()]).unwrap();
        assert_eq!(
            art.get(&item.id.to_string()).map(|s| s.as_str()),
            Some("/roms/snes/images/Game-image.png")
        );
        let st = cat.rom_game_state_for(pk).unwrap();
        assert_eq!(st.rating, Some(4));
        assert!(st.favorite);
        assert_eq!(st.genre.as_deref(), Some("Platform"));
        assert_eq!(st.play_count, 6);
        assert_eq!(st.last_played.as_deref(), Some("2023-08-07T11:46:07Z"));
    }

    #[test]
    fn es_apply_never_touches_dat_identity() {
        let cat = Catalog::open_in_memory().unwrap();
        let loc = LocationId::new();
        cat.insert_location(&sample_location(loc)).unwrap();
        let pk = "/roms/snes/Game (USA).zip";
        let item = es_test_item(&cat, loc, pk);
        cat.upsert_media_identity(
            &item.id,
            "rom",
            Some("DAT Name"),
            Some("snes"),
            Some("0123456789abcdef0123456789abcdef01234567"),
            r#"{"matched":true,"source":"no-intro","size":12345,"modified":null,"also":[]}"#,
        )
        .unwrap();
        let out = cat.es_apply(&es_input(&item, pk)).unwrap();
        assert_eq!(out.identity, EsIdentityOutcome::DatKept, "P1: DAT truth stands");
        let row = cat.media_identity_for(&item.id).unwrap().unwrap();
        assert_eq!(row.title.as_deref(), Some("DAT Name"));
        assert_eq!(
            row.sha1.as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567")
        );
        // The descriptive data still lands — only the identity is sacred.
        let meta = cat.es_metadata_for(pk).unwrap().unwrap();
        assert_eq!(meta.name.as_deref(), Some("Gamelist Name"));
    }

    #[test]
    fn es_apply_fills_unmatched_row_and_refreshes_its_own() {
        let cat = Catalog::open_in_memory().unwrap();
        let loc = LocationId::new();
        cat.insert_location(&sample_location(loc)).unwrap();
        let pk = "/roms/snes/Game (USA).zip";
        let item = es_test_item(&cat, loc, pk);
        // The identify pass's unmatched row shape (no title, no source).
        cat.upsert_media_identity(
            &item.id,
            "rom",
            None,
            None,
            None,
            r#"{"matched":false,"size":1,"modified":null,"also":[]}"#,
        )
        .unwrap();
        let out = cat.es_apply(&es_input(&item, pk)).unwrap();
        assert_eq!(out.identity, EsIdentityOutcome::FilledUnmatched);
        // A re-import refreshes its own row rather than keeping it.
        let out2 = cat.es_apply(&es_input(&item, pk)).unwrap();
        assert_eq!(out2.identity, EsIdentityOutcome::GamelistRefreshed);
        // A film identity is never the import's business.
        let mut movie = named_item(loc, "Film.mkv", ItemKind::File);
        movie.provider_key = Some("/films/Film.mkv".to_string());
        cat.insert_item(&movie).unwrap();
        cat.upsert_video_identity(
            &movie.id, "movie", Some("Film"), Some(2020), None, None, None, None,
            None, false, "{}",
        )
        .unwrap();
        let mut input = es_input(&movie, "/films/Film.mkv");
        input.item_id = movie.id;
        let out3 = cat.es_apply(&input).unwrap();
        assert_eq!(out3.identity, EsIdentityOutcome::NotRom);
        assert_eq!(
            cat.media_identity_for(&movie.id).unwrap().unwrap().title.as_deref(),
            Some("Film")
        );
    }

    #[test]
    fn es_apply_state_merges_fill_only_and_max() {
        let cat = Catalog::open_in_memory().unwrap();
        let loc = LocationId::new();
        cat.insert_location(&sample_location(loc)).unwrap();
        let pk = "/roms/snes/Game (USA).zip";
        let item = es_test_item(&cat, loc, pk);
        // His Fable-side truth: 5 stars, 10 plays, later last-played,
        // his own genre. The import must not demote any of it (P4).
        cat.rom_game_set_rating(pk, Some(5)).unwrap();
        cat.rom_game_set_overrides(pk, None, Some("His Genre"), None, None)
            .unwrap();
        let mut input = es_input(&item, pk);
        input.playcount = Some(10);
        input.last_played_rfc3339 = Some("2024-01-01T00:00:00Z".to_string());
        cat.es_apply(&input).unwrap();
        let st = cat.rom_game_state_for(pk).unwrap();
        assert_eq!(st.rating, Some(5), "his rating wins over the import's 4");
        assert_eq!(st.genre.as_deref(), Some("His Genre"));
        assert_eq!(st.play_count, 10);
        assert_eq!(st.last_played.as_deref(), Some("2024-01-01T00:00:00Z"));
        assert!(st.favorite, "favorite is set-only: the import's true lands");
        // The import's weaker history must not drag anything down.
        let mut weaker = es_input(&item, pk);
        weaker.rating_stars = Some(2);
        weaker.playcount = Some(3);
        weaker.last_played_rfc3339 = Some("2020-01-01T00:00:00Z".to_string());
        let out = cat.es_apply(&weaker).unwrap();
        assert!(out.state_fields.is_empty(), "a no-news import writes no state");
        let st = cat.rom_game_state_for(pk).unwrap();
        assert_eq!(st.rating, Some(5));
        assert_eq!(st.play_count, 10);
        // And a second identical apply is a state no-op (idempotence).
        let out2 = cat.es_apply(&input).unwrap();
        assert!(out2.state_fields.is_empty());
        assert!(out2.art_filled.is_empty());
    }

    #[test]
    fn es_apply_art_found_row_wins() {
        let cat = Catalog::open_in_memory().unwrap();
        let loc = LocationId::new();
        cat.insert_location(&sample_location(loc)).unwrap();
        let pk = "/roms/snes/Game (USA).zip";
        let item = es_test_item(&cat, loc, pk);
        cat.upsert_rom_media(
            &item.id.to_string(),
            "boxart",
            "found",
            Some("libretro"),
            Some("/fable-media/covers/Game.png"),
            "{}",
        )
        .unwrap();
        let out = cat.es_apply(&es_input(&item, pk)).unwrap();
        assert_eq!(out.art_kept, vec!["boxart".to_string()], "P5: found wins");
        assert!(out.art_filled.is_empty());
        let art = cat.rom_media_art(&[item.id.to_string()]).unwrap();
        assert_eq!(
            art.get(&item.id.to_string()).map(|s| s.as_str()),
            Some("/fable-media/covers/Game.png")
        );
        // A 'missing' row, though, yields to the import's real file.
        let pk2 = "/roms/snes/Other (USA).zip";
        let item2 = es_test_item(&cat, loc, pk2);
        cat.upsert_rom_media(
            &item2.id.to_string(),
            "boxart",
            "missing",
            Some("screenscraper"),
            None,
            "{}",
        )
        .unwrap();
        let out2 = cat.es_apply(&es_input(&item2, pk2)).unwrap();
        assert_eq!(out2.art_filled, vec!["boxart".to_string()]);
    }

    #[test]
    fn es_import_system_end_to_end() {
        use ufm_media::esgamelist::EsSystemDir;
        let base = std::env::temp_dir().join(format!(
            "ufm-es-import-{}-{}",
            std::process::id(),
            "e2e"
        ));
        let _ = std::fs::remove_dir_all(&base);
        let sysdir = base.join("snes");
        let images = sysdir.join("images");
        std::fs::create_dir_all(&images).unwrap();
        std::fs::write(sysdir.join("Game One (USA).zip"), b"rom-bytes").unwrap();
        std::fs::write(images.join("Game One (USA)-image.png"), b"png").unwrap();
        std::fs::write(
            sysdir.join("gamelist.xml"),
            r#"<?xml version="1.0"?>
<gameList>
  <game id="1">
    <path>./Game One (USA).zip</path>
    <name>Game One</name>
    <desc>The first game.</desc>
    <genre>Platform</genre>
    <players>1</players>
    <rating>0.9</rating>
    <releasedate>19930101T000000</releasedate>
    <image>./images/Game One (USA)-image.png</image>
  </game>
  <game id="2">
    <path>./Ghost (USA).zip</path>
    <name>Ghost</name>
  </game>
</gameList>"#,
        )
        .unwrap();
        let cat = Catalog::open_in_memory().unwrap();
        let loc = LocationId::new();
        cat.insert_location(&sample_location(loc)).unwrap();
        let game_path = sysdir.join("Game One (USA).zip");
        let mut item = named_item(loc, "Game One (USA).zip", ItemKind::File);
        item.provider_key = Some(game_path.to_string_lossy().into_owned());
        item.size_bytes = Some(9);
        cat.insert_item(&item).unwrap();

        let sys = EsSystemDir {
            es_name: "snes".to_string(),
            dir: sysdir.clone(),
            gamelist_path: sysdir.join("gamelist.xml"),
        };
        let rep = cat.import_es_system(&sys).unwrap();
        assert!(rep.parse_error.is_none(), "{:?}", rep.parse_error);
        assert_eq!(rep.counts.entries, 2);
        assert_eq!(rep.counts.matched_exact, 1);
        assert_eq!(rep.counts.unmatched_missing_file, 1, "Ghost has no file, no item");
        assert_eq!(rep.counts.identity_written, 1);
        assert_eq!(rep.counts.art_filled, 1);
        let row = cat.media_identity_for(&item.id).unwrap().unwrap();
        assert_eq!(row.title.as_deref(), Some("Game One"));
        assert_eq!(row.year, Some(1993));
        let pk_str = game_path.to_string_lossy().into_owned();
        let st = cat.rom_game_state_for(&pk_str).unwrap();
        assert_eq!(st.rating, Some(5), "0.9 rounds to 5 stars");
        // Re-import: everything reconciles to refreshed + no fills.
        let rep2 = cat.import_es_system(&sys).unwrap();
        assert_eq!(rep2.counts.identity_refreshed, 1);
        assert_eq!(rep2.counts.art_filled, 0);
        assert_eq!(rep2.counts.art_kept, 1);
        assert_eq!(rep2.counts.state_updates, 0);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn es_import_system_malformed_duplicates_and_casefold() {
        use ufm_media::esgamelist::EsSystemDir;
        let base = std::env::temp_dir().join(format!(
            "ufm-es-import-{}-{}",
            std::process::id(),
            "neg"
        ));
        let _ = std::fs::remove_dir_all(&base);
        let sysdir = base.join("nes");
        std::fs::create_dir_all(&sysdir).unwrap();
        let cat = Catalog::open_in_memory().unwrap();
        let loc = LocationId::new();
        cat.insert_location(&sample_location(loc)).unwrap();

        // Malformed: the whole system reports its error, counts zero.
        std::fs::write(sysdir.join("gamelist.xml"), "<gameList><game><path>./x.zip</path>")
            .unwrap();
        let sys = EsSystemDir {
            es_name: "nes".to_string(),
            dir: sysdir.clone(),
            gamelist_path: sysdir.join("gamelist.xml"),
        };
        let rep = cat.import_es_system(&sys).unwrap();
        assert!(rep.parse_error.is_some());
        assert_eq!(rep.counts.entries, 0);

        // Duplicates + a case-mismatched path + a missing file.
        std::fs::write(sysdir.join("Case Game (USA).nes"), b"rom").unwrap();
        std::fs::write(
            sysdir.join("gamelist.xml"),
            r#"<?xml version="1.0"?>
<gameList>
  <game><path>./case game (usa).nes</path><name>Case Game</name></game>
  <game><path>./case game (usa).nes</path><name>Case Game Again</name></game>
  <game><path>./Gone (USA).nes</path><name>Gone</name></game>
  <game><name>No Path At All</name></game>
</gameList>"#,
        )
        .unwrap();
        let mut item = named_item(loc, "Case Game (USA).nes", ItemKind::File);
        item.provider_key = Some(
            sysdir
                .join("Case Game (USA).nes")
                .to_string_lossy()
                .into_owned(),
        );
        cat.insert_item(&item).unwrap();
        let rep = cat.import_es_system(&sys).unwrap();
        assert!(rep.parse_error.is_none(), "{:?}", rep.parse_error);
        assert_eq!(rep.counts.entries, 4);
        assert_eq!(rep.counts.matched_casefold, 1, "case-folded path still matches");
        assert_eq!(rep.counts.duplicates, 1);
        assert_eq!(rep.counts.unmatched_missing_file, 1);
        assert_eq!(rep.counts.no_path, 1);
        assert_eq!(rep.counts.identity_written, 1);
        let _ = std::fs::remove_dir_all(&base);
    }

    // --------------------------------------------------------
    // Hyperbase #4: windowed shelf queries. The fixture: five
    // groups over three systems — 1942 (art), Chrono Trigger
    // (art), Klax (two dumps, no art), Metal Slug (arcade, also
    // Neo Geo, no art), Super Mario World (missing-art row).
    // --------------------------------------------------------

    // --------------------------------------------------------
    // PC Games P1 (2026-10-17): import writes, precedence,
    // shelf integration at the imported scale.
    // --------------------------------------------------------

    fn pc_record(
        loc: &str,
        path: &str,
        title: &str,
        source_kind: &str,
        library: &str,
    ) -> PcInstallRecord {
        PcInstallRecord {
            location_id: loc.to_string(),
            provider_key: path.to_string(),
            folder_name: path.rsplit('/').next().unwrap_or(path).to_string(),
            folder_modified: None,
            title: title.to_string(),
            year: Some(2019),
            evidence_json: format!(
                "{{\"source\":\"{source_kind}\",\"library\":\"test-uuid\"}}"
            ),
            source_kind: source_kind.to_string(),
            library_name: library.to_string(),
            playnite_id: Some(format!("pid-{title}")),
            game_guid: None,
            description: Some(format!("About {title}")),
            genres: vec!["Action".to_string()],
            developers: vec!["Dev".to_string()],
            publishers: vec![],
            series: vec![],
            features: vec!["Single Player".to_string()],
            age_ratings: vec![],
            links: vec![PcLink {
                name: "Steam".to_string(),
                url: "https://store.steampowered.com/app/1".to_string(),
            }],
            storefronts: vec!["Steam".to_string()],
            critic_score: Some(85),
            community_score: None,
            install_size: Some(1_000_000),
            version: Some("1.0".to_string()),
            exe_name: Some("game.exe".to_string()),
            exe_rel: Some("game.exe".to_string()),
            release_raw: Some("2019-5-14".to_string()),
            mislabel_note: None,
            present: true,
            imported_at: "2026-10-17T00:00:00Z".to_string(),
        }
    }

    fn pc_seed_location(cat: &Catalog) -> String {
        let loc = LocationId::new();
        cat.insert_location(&sample_location(loc)).unwrap();
        loc.to_string()
    }

    #[test]
    fn pc_import_double_run_is_idempotent() {
        let cat = Catalog::open_in_memory().unwrap();
        let loc = pc_seed_location(&cat);
        for (path, title) in [
            ("/games/Alpha", "Alpha"),
            ("/games/Beta", "Beta"),
            ("/games/Gamma", "Gamma"),
        ] {
            let out = cat
                .upsert_pc_install(&pc_record(&loc, path, title, "playnite", "Playnite"))
                .unwrap();
            assert!(out.created, "first import creates");
        }
        for (path, title) in [
            ("/games/Alpha", "Alpha"),
            ("/games/Beta", "Beta"),
            ("/games/Gamma", "Gamma"),
        ] {
            let out = cat
                .upsert_pc_install(&pc_record(&loc, path, title, "playnite", "Playnite"))
                .unwrap();
            assert!(!out.created && !out.upgraded && !out.skipped_precedence);
        }
        let n = |sql: &str| -> i64 {
            cat.conn.query_row(sql, [], |r| r.get(0)).unwrap()
        };
        assert_eq!(n("SELECT COUNT(*) FROM media_identities WHERE media_kind='pc-game'"), 3);
        assert_eq!(n("SELECT COUNT(*) FROM pc_metadata"), 3);
        assert_eq!(n("SELECT COUNT(*) FROM pc_game_state"), 3);
        assert_eq!(n("SELECT COUNT(*) FROM items"), 3);
        let counts = cat.rom_home_counts().unwrap();
        assert_eq!(counts.total, 3);
        assert_eq!(counts.per_system, vec![("pc".to_string(), 3, 3)]);
        assert_eq!(counts.pc_libraries, vec![("Playnite".to_string(), 3)]);
    }

    #[test]
    fn pc_folder_scan_upgrades_never_downgrades() {
        let cat = Catalog::open_in_memory().unwrap();
        let loc = pc_seed_location(&cat);
        let out = cat
            .upsert_pc_install(&pc_record(
                &loc,
                "/games/loose/Solo",
                "Solo",
                "folder-scan",
                "Folder finds",
            ))
            .unwrap();
        assert!(out.created);
        let out = cat
            .upsert_pc_install(&pc_record(
                &loc,
                "/games/loose/Solo",
                "Solo",
                "playnite",
                "Playnite",
            ))
            .unwrap();
        assert!(out.upgraded, "playnite upgrades a folder find");
        let sheet = cat.pc_game_sheet("/games/loose/Solo").unwrap().unwrap();
        assert_eq!(sheet.source_kind, "playnite");
        assert_eq!(sheet.library_name, "Playnite");
        let out = cat
            .upsert_pc_install(&pc_record(
                &loc,
                "/games/loose/Solo",
                "Solo",
                "folder-scan",
                "Folder finds",
            ))
            .unwrap();
        assert!(out.skipped_precedence, "folder scan never downgrades");
        let sheet = cat.pc_game_sheet("/games/loose/Solo").unwrap().unwrap();
        assert_eq!(sheet.source_kind, "playnite", "still playnite-owned");
    }

    #[test]
    fn pc_shelf_windows_sources_genres_and_facets() {
        let cat = Catalog::open_in_memory().unwrap();
        let loc = pc_seed_location(&cat);
        let mut seed = |path: &str, title: &str, kind: &str, lib: &str, genres: Vec<&str>| {
            let mut rec = pc_record(&loc, path, title, kind, lib);
            rec.genres = genres.iter().map(|s| s.to_string()).collect();
            cat.upsert_pc_install(&rec).unwrap();
            // Cover adopted for one of them.
            if title == "Alpha" {
                let item_id: String = cat
                    .conn
                    .query_row(
                        "SELECT id FROM items WHERE provider_key = ?1",
                        params![path],
                        |r| r.get(0),
                    )
                    .unwrap();
                cat.upsert_rom_media(
                    &item_id,
                    "cover",
                    "found",
                    Some("playnite"),
                    Some("/media/pc/alpha/cover.jpg"),
                    "{}",
                )
                .unwrap();
            }
        };
        seed("/games/Alpha", "Alpha", "playnite", "Playnite", vec!["Action"]);
        seed("/games/Beta", "Beta", "playnite", "Playnite", vec!["RPG"]);
        seed("/games/Gamma", "Gamma", "folder-scan", "Folder finds", vec!["Action"]);

        // The PC system shelf sees all three; Alpha has art via
        // its adopted cover.
        let mut spec = shelf_spec("system");
        spec.platform = Some("pc".to_string());
        let page = cat.rom_home_shelf_window(&spec).unwrap();
        assert_eq!(page.total, 3);
        let alpha = page
            .games
            .iter()
            .find(|g| g.title == "Alpha")
            .expect("alpha row");
        let card = alpha.pc.as_ref().expect("pc card attached");
        assert_eq!(card.library_name, "Playnite");
        assert_eq!(card.install_state, "present");
        assert_eq!(card.launch_badge, "not-checked");
        assert_eq!(card.storefronts, vec!["Steam".to_string()]);
        // Library sub-shelves.
        let mut sub = shelf_spec("system");
        sub.platform = Some("pc".to_string());
        sub.pc_source = Some("Folder finds".to_string());
        let page = cat.rom_home_shelf_window(&sub).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.games[0].title, "Gamma");
        // Genre filter reads imported genres.
        let mut filtered = shelf_spec("system");
        filtered.platform = Some("pc".to_string());
        filtered.filters.genres = vec!["RPG".to_string()];
        let page = cat.rom_home_shelf_window(&filtered).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.games[0].title, "Beta");
        // Search matches an imported genre too.
        let mut search = shelf_spec("search");
        search.query = Some("rpg".to_string());
        let page = cat.rom_home_shelf_window(&search).unwrap();
        assert_eq!(page.total, 1);
        // Facets + counts carry the PC facts.
        let facets = cat.rom_home_genre_facets().unwrap();
        assert!(facets.contains(&("Action".to_string(), 2)), "{facets:?}");
        assert!(facets.contains(&("RPG".to_string(), 1)), "{facets:?}");
        let counts = cat.rom_home_counts().unwrap();
        assert_eq!(counts.missing_media, 2, "cover counts as art");
        assert_eq!(
            counts.pc_libraries,
            vec![("Playnite".to_string(), 2), ("Folder finds".to_string(), 1)]
        );
        // Missing pairs never include PC items (fetch is ROM-only).
        let pairs = cat.rom_home_missing_pairs(&shelf_spec("all")).unwrap();
        assert!(pairs.is_empty(), "{pairs:?}");
        // Byte totals ride install_size, not the folder item.
        let rows = cat.rom_home_system_file_rows().unwrap();
        let pc_rows: Vec<_> = rows.iter().filter(|r| r.platform == "pc").collect();
        assert_eq!(pc_rows.len(), 3);
        assert!(pc_rows.iter().all(|r| r.size_bytes == Some(1_000_000)));
    }

    #[test]
    fn pc_badge_survives_reimport_and_state_routes_to_group() {
        let cat = Catalog::open_in_memory().unwrap();
        let loc = pc_seed_location(&cat);
        cat.upsert_pc_install(&pc_record(
            &loc,
            "/games/Alpha",
            "Alpha",
            "playnite",
            "Playnite",
        ))
        .unwrap();
        // Personal state routes to the group for pc paths exactly
        // like ROM paths (one game-state model).
        cat.rom_game_set_favorite("/games/Alpha", true).unwrap();
        let counts = cat.rom_home_counts().unwrap();
        assert_eq!(counts.favorite_groups, 1);
        // Badge starts at not-checked, moves only through its door,
        // and a re-import (presence refresh) never resets it.
        assert!(cat
            .pc_set_launch_badge("/games/Alpha", "boots", Some("ran clean"), "2026-10-17T01:00:00Z")
            .unwrap());
        let mut rec = pc_record(&loc, "/games/Alpha", "Alpha", "playnite", "Playnite");
        rec.present = false;
        cat.upsert_pc_install(&rec).unwrap();
        let sheet = cat.pc_game_sheet("/games/Alpha").unwrap().unwrap();
        assert_eq!(sheet.launch_badge, "boots");
        assert_eq!(sheet.launch_note.as_deref(), Some("ran clean"));
        assert!(!sheet.present, "presence refreshed honestly");
        assert_eq!(sheet.installs.len(), 1);
        assert_eq!(sheet.genres, vec!["Action".to_string()]);
        assert_eq!(sheet.links.len(), 1);
    }

    #[test]
    fn pc_window_walk_at_imported_scale() {
        // The brief's scale: a real imported library (392 groups)
        // walks window by window, each group exactly once, with
        // the TRUE total on every page.
        let cat = Catalog::open_in_memory().unwrap();
        let loc = pc_seed_location(&cat);
        for i in 0..392 {
            let title = format!("Game {i:03}");
            cat.upsert_pc_install(&pc_record(
                &loc,
                &format!("/games/{title}"),
                &title,
                "playnite",
                "Playnite",
            ))
            .unwrap();
        }
        let mut spec = shelf_spec("system");
        spec.platform = Some("pc".to_string());
        spec.limit = 100;
        let mut seen: Vec<String> = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..10 {
            spec.cursor = cursor.clone();
            let page = cat.rom_home_shelf_window(&spec).unwrap();
            assert_eq!(page.total, 392);
            for g in &page.games {
                if !seen.contains(&g.group_key) {
                    seen.push(g.group_key.clone());
                }
            }
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        assert_eq!(seen.len(), 392, "every group exactly once");
    }

    fn shelf_spec(kind: &str) -> RomHomeShelfSpec {
        RomHomeShelfSpec {
            kind: kind.to_string(),
            platform: None,
            query: None,
            search_platforms: vec![],
            member_keys: None,
            pc_source: None,
            filters: RomHomeShelfFilters::default(),
            sort: "title".to_string(),
            cursor: None,
            limit: 240,
        }
    }

    fn page_group_keys(page: &RomHomeShelfPage) -> Vec<String> {
        let mut out: Vec<String> = vec![];
        for g in &page.games {
            if !out.contains(&g.group_key) {
                out.push(g.group_key.clone());
            }
        }
        out
    }

    struct ShelfSeed {
        klax_usa: Item,
        klax_japan: Item,
        smw: Item,
        chrono: Item,
    }

    fn seed_shelf(cat: &Catalog) -> ShelfSeed {
        let loc = LocationId::new();
        cat.insert_location(&sample_location(loc)).unwrap();
        let mk = |cat: &Catalog, name: &str, pk: &str, title: &str, platform: &str, sha1: &str, evidence: &str| {
            let it = rom_item(cat, loc, name, pk);
            cat.upsert_media_identity(
                &it.id,
                "rom",
                Some(title),
                Some(platform),
                Some(sha1),
                evidence,
            )
            .unwrap();
            it
        };
        let g1942 = mk(
            cat,
            "1942 (JU).nes",
            "/roms/nes/1942.nes",
            "1942 (Japan, USA) (En)",
            "nes",
            "aaaa",
            r#"{"matched":true,"also":[]}"#,
        );
        cat.upsert_rom_media(
            &g1942.id.to_string(),
            "boxart",
            "found",
            Some("libretro"),
            Some("/media/nes/1942.png"),
            "{}",
        )
        .unwrap();
        let klax_usa = mk(
            cat,
            "Klax (USA).nes",
            "/roms/nes/klax-usa.nes",
            "Klax (USA)",
            "nes",
            "1111",
            r#"{"matched":true,"also":[],"crc":111}"#,
        );
        let klax_japan = mk(
            cat,
            "Klax (Japan).nes",
            "/roms/nes/klax-japan.nes",
            "Klax (Japan)",
            "nes",
            "2222",
            r#"{"matched":true,"also":[],"crc":222}"#,
        );
        let smw = mk(
            cat,
            "smw.sfc",
            "/roms/snes/smw.sfc",
            "Super Mario World (USA)",
            "snes",
            "bbbb",
            r#"{"matched":true,"also":[]}"#,
        );
        cat.upsert_rom_media(
            &smw.id.to_string(),
            "boxart",
            "missing",
            None,
            None,
            "{}",
        )
        .unwrap();
        let chrono = mk(
            cat,
            "chrono.sfc",
            "/roms/snes/chrono.sfc",
            "Chrono Trigger (USA)",
            "snes",
            "dddd",
            r#"{"matched":true,"also":[]}"#,
        );
        cat.upsert_rom_media(
            &chrono.id.to_string(),
            "boxart",
            "found",
            Some("libretro"),
            Some("/media/snes/chrono.png"),
            "{}",
        )
        .unwrap();
        let _metal = mk(
            cat,
            "mslug.zip",
            "/roms/arcade/mslug.zip",
            "Metal Slug - Super Vehicle-001",
            "arcade",
            "cccc",
            r#"{"matched":true,"also":["neogeo"]}"#,
        );
        ShelfSeed {
            klax_usa,
            klax_japan,
            smw,
            chrono,
        }
    }

    fn set_year(cat: &Catalog, item: &Item, year: i64) {
        cat.conn
            .execute(
                "UPDATE media_identities SET year = ?1 WHERE item_id = ?2",
                rusqlite::params![year, item.id.to_string()],
            )
            .unwrap();
    }

    #[test]
    fn shelf_window_walks_groups_exactly_once_with_true_totals() {
        let cat = Catalog::open_in_memory().unwrap();
        seed_shelf(&cat);
        let mut spec = shelf_spec("all");
        spec.limit = 2;
        let mut seen_groups: Vec<String> = vec![];
        let mut seen_items: Vec<String> = vec![];
        let mut pages = 0;
        loop {
            let page = cat.rom_home_shelf_window(&spec).unwrap();
            assert_eq!(page.total, 5, "total is the full set on every window");
            pages += 1;
            for k in page_group_keys(&page) {
                assert!(!seen_groups.contains(&k), "group repeated across windows: {k}");
                seen_groups.push(k);
            }
            for g in &page.games {
                seen_items.push(g.item.id.to_string());
            }
            match page.next_cursor {
                Some(c) => {
                    spec.cursor = Some(c);
                    assert!(pages < 10, "window chain must terminate");
                }
                None => break,
            }
        }
        assert_eq!(pages, 3);
        assert_eq!(seen_groups.len(), 5);
        // The window walk delivers exactly the full fetch's items.
        let full = cat.rom_home_games(None).unwrap();
        let mut full_ids: Vec<String> =
            full.iter().map(|g| g.item.id.to_string()).collect();
        full_ids.sort();
        seen_items.sort();
        assert_eq!(seen_items, full_ids);
        // One window big enough returns everything, cursor closed.
        let mut big = shelf_spec("all");
        big.limit = 2000;
        let page = cat.rom_home_shelf_window(&big).unwrap();
        assert_eq!(page.total, 5);
        assert_eq!(page.games.len(), 6);
        assert!(page.next_cursor.is_none());
    }

    #[test]
    fn shelf_window_groups_never_split_and_keep_title_order() {
        let cat = Catalog::open_in_memory().unwrap();
        seed_shelf(&cat);
        let mut spec = shelf_spec("all");
        spec.limit = 3;
        let p1 = cat.rom_home_shelf_window(&spec).unwrap();
        let keys1 = page_group_keys(&p1);
        assert_eq!(keys1.len(), 3);
        // Klax is the third group in title order and arrives whole.
        let klax_rows = p1
            .games
            .iter()
            .filter(|g| g.title.starts_with("Klax"))
            .count();
        assert_eq!(klax_rows, 2, "both Klax dumps ride in one window");
        spec.cursor = p1.next_cursor.clone();
        let p2 = cat.rom_home_shelf_window(&spec).unwrap();
        assert_eq!(page_group_keys(&p2).len(), 2);
        assert!(p2.next_cursor.is_none());
        // Title order across the seam: window 1 ends before window 2.
        let titles: Vec<&str> = p1
            .games
            .iter()
            .map(|g| g.title.as_str())
            .collect();
        assert!(titles[0].starts_with("1942"));
    }

    #[test]
    fn shelf_window_system_membership_includes_also_platforms() {
        let cat = Catalog::open_in_memory().unwrap();
        seed_shelf(&cat);
        let mut spec = shelf_spec("system");
        spec.platform = Some("neogeo".to_string());
        let page = cat.rom_home_shelf_window(&spec).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.games[0].platform, "arcade");
        spec.platform = Some("nes".to_string());
        assert_eq!(cat.rom_home_shelf_window(&spec).unwrap().total, 2);
        spec.platform = Some("snes".to_string());
        assert_eq!(cat.rom_home_shelf_window(&spec).unwrap().total, 2);
        spec.platform = Some("genesis".to_string());
        assert_eq!(cat.rom_home_shelf_window(&spec).unwrap().total, 0);
    }

    #[test]
    fn shelf_window_missing_favorites_recent_and_counts_agree() {
        let cat = Catalog::open_in_memory().unwrap();
        let seed = seed_shelf(&cat);
        // Missing = groups with NO member holding found box art.
        let page = cat.rom_home_shelf_window(&shelf_spec("missing")).unwrap();
        assert_eq!(page.total, 3, "Klax, Metal Slug, Super Mario World");
        // Favorite marked on the SMW path lands on its group.
        cat.rom_game_set_favorite("/roms/snes/smw.sfc", true)
            .unwrap();
        let page = cat.rom_home_shelf_window(&shelf_spec("favorites")).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.games[0].item.id, seed.smw.id);
        // Play on one Klax dump makes the GROUP recent + played.
        cat.conn
            .execute(
                "INSERT OR IGNORE INTO rom_game_state (provider_key, updated_at) VALUES ('/roms/nes/klax-japan.nes', '2026-10-17T00:00:00Z')",
                [],
            )
            .unwrap();
        cat.conn
            .execute(
                "UPDATE rom_game_state SET play_count = 2, last_played = '2026-09-01T10:00:00Z' WHERE provider_key = '/roms/nes/klax-japan.nes'",
                [],
            )
            .unwrap();
        let page = cat.rom_home_shelf_window(&shelf_spec("recent")).unwrap();
        assert_eq!(page.total, 1);
        assert!(page.games.iter().all(|g| g.title.starts_with("Klax")));
        // The tallies speak the same definitions.
        let counts = cat.rom_home_counts().unwrap();
        assert_eq!(counts.total, 5);
        assert_eq!(counts.missing_media, 3);
        assert_eq!(counts.favorite_groups, 1);
        assert_eq!(counts.recent_groups, 1);
        let per: std::collections::HashMap<&str, (i64, i64)> = counts
            .per_system
            .iter()
            .map(|(p, g, m)| (p.as_str(), (*g, *m)))
            .collect();
        assert_eq!(per["nes"], (2, 1));
        assert_eq!(per["snes"], (2, 1));
        assert_eq!(per["arcade"], (1, 1));
        assert_eq!(per["neogeo"], (1, 1));
    }

    #[test]
    fn shelf_window_genre_players_filters_and_facets() {
        let cat = Catalog::open_in_memory().unwrap();
        seed_shelf(&cat);
        cat.rom_game_set_overrides(
            "/roms/nes/klax-usa.nes",
            None,
            Some("Puzzle"),
            Some("2"),
            None,
        )
        .unwrap();
        let facets = cat.rom_home_genre_facets().unwrap();
        assert_eq!(facets, vec![("Puzzle".to_string(), 1)]);
        let mut spec = shelf_spec("all");
        spec.filters.genres = vec!["Puzzle".to_string()];
        let page = cat.rom_home_shelf_window(&spec).unwrap();
        assert_eq!(page.total, 1);
        assert!(page.games.iter().all(|g| g.title.starts_with("Klax")));
        let mut spec = shelf_spec("all");
        spec.filters.players = vec!["2".to_string()];
        assert_eq!(cat.rom_home_shelf_window(&spec).unwrap().total, 1);
        spec.filters.players = vec!["1".to_string()];
        assert_eq!(cat.rom_home_shelf_window(&spec).unwrap().total, 0);
        // Media facet at group level: has-art = any member's art.
        let mut spec = shelf_spec("all");
        spec.filters.media = vec!["has-art".to_string()];
        assert_eq!(cat.rom_home_shelf_window(&spec).unwrap().total, 2);
        spec.filters.media = vec!["missing-art".to_string()];
        assert_eq!(cat.rom_home_shelf_window(&spec).unwrap().total, 3);
    }

    #[test]
    fn shelf_window_unplayed_means_no_copy_played() {
        let cat = Catalog::open_in_memory().unwrap();
        seed_shelf(&cat);
        let mut spec = shelf_spec("all");
        spec.filters.flags = vec!["unplayed".to_string()];
        assert_eq!(cat.rom_home_shelf_window(&spec).unwrap().total, 5);
        // One dump played: the GAME has been played (pinned group
        // definition — the old rep-only reading hid this).
        cat.conn
            .execute(
                "INSERT OR IGNORE INTO rom_game_state (provider_key, play_count, updated_at) VALUES ('/roms/nes/klax-usa.nes', 0, '2026-10-17T00:00:00Z')",
                [],
            )
            .unwrap();
        cat.conn
            .execute(
                "UPDATE rom_game_state SET play_count = 1 WHERE provider_key = '/roms/nes/klax-usa.nes'",
                [],
            )
            .unwrap();
        assert_eq!(cat.rom_home_shelf_window(&spec).unwrap().total, 4);
    }

    #[test]
    fn shelf_window_starred_flag_reads_star_or_rating() {
        let cat = Catalog::open_in_memory().unwrap();
        seed_shelf(&cat);
        cat.conn
            .execute(
                "INSERT OR IGNORE INTO path_stars (provider_key, starred_at) VALUES ('/roms/snes/chrono.sfc', '2026-10-17T00:00:00Z')",
                [],
            )
            .unwrap();
        let mut spec = shelf_spec("all");
        spec.filters.flags = vec!["starred".to_string()];
        assert_eq!(cat.rom_home_shelf_window(&spec).unwrap().total, 1);
        // A rating of 1+ on any path of a group also counts.
        cat.rom_game_set_rating("/roms/nes/klax-japan.nes", Some(4))
            .unwrap();
        assert_eq!(cat.rom_home_shelf_window(&spec).unwrap().total, 2);
    }

    #[test]
    fn shelf_window_year_sort_uses_default_version_year() {
        let cat = Catalog::open_in_memory().unwrap();
        let seed = seed_shelf(&cat);
        set_year(&cat, &seed.klax_usa, 1990);
        set_year(&cat, &seed.klax_japan, 1989);
        set_year(&cat, &seed.chrono, 1995);
        set_year(&cat, &seed.smw, 1990);
        // 1942: find its item through the full fetch to set 1987.
        let all = cat.rom_home_games(None).unwrap();
        let g1942 = all.iter().find(|g| g.title.starts_with("1942")).unwrap();
        cat.conn
            .execute(
                "UPDATE media_identities SET year = 1987 WHERE item_id = ?1",
                rusqlite::params![g1942.item.id.to_string()],
            )
            .unwrap();
        let mut spec = shelf_spec("all");
        spec.sort = "year".to_string();
        let page = cat.rom_home_shelf_window(&spec).unwrap();
        let order: Vec<String> = page_group_keys(&page)
            .iter()
            .map(|k| k.rsplit('\u{1}').next().unwrap_or("").to_string())
            .collect();
        assert_eq!(
            order,
            vec!["chrono trigger", "klax", "super mario world", "1942", "metal slug - super vehicle-001"],
            "year desc: 1995, then the 1990 pair by title (Klax's DEFAULT version is USA 1990, not Japan 1989), 1987, yearless last"
        );
        // Most-played sums across the group's copies.
        cat.conn
            .execute(
                "INSERT OR IGNORE INTO rom_game_state (provider_key, play_count, updated_at) VALUES ('/roms/nes/klax-japan.nes', 0, '2026-10-17T00:00:00Z')",
                [],
            )
            .unwrap();
        cat.conn
            .execute(
                "UPDATE rom_game_state SET play_count = 3 WHERE provider_key = '/roms/nes/klax-japan.nes'",
                [],
            )
            .unwrap();
        cat.conn
            .execute(
                "INSERT OR IGNORE INTO rom_game_state (provider_key, play_count, updated_at) VALUES ('/roms/snes/smw.sfc', 0, '2026-10-17T00:00:00Z')",
                [],
            )
            .unwrap();
        cat.conn
            .execute(
                "UPDATE rom_game_state SET play_count = 2 WHERE provider_key = '/roms/snes/smw.sfc'",
                [],
            )
            .unwrap();
        let mut spec = shelf_spec("all");
        spec.sort = "most-played".to_string();
        let page = cat.rom_home_shelf_window(&spec).unwrap();
        let keys = page_group_keys(&page);
        assert!(keys[0].contains("klax"), "3 plays beats 2");
        assert!(keys[1].contains("super mario world"));
    }

    #[test]
    fn shelf_window_cursor_is_stable_under_concurrent_inserts() {
        let cat = Catalog::open_in_memory().unwrap();
        seed_shelf(&cat);
        let mut spec = shelf_spec("all");
        spec.limit = 2;
        let p1 = cat.rom_home_shelf_window(&spec).unwrap();
        assert_eq!(page_group_keys(&p1).len(), 2);
        // Identification lands mid-browse: one game sorting BEFORE
        // the cursor, one after everything.
        let loc = LocationId::new();
        cat.insert_location(&sample_location(loc)).unwrap();
        let aard = rom_item(&cat, loc, "Aardvark (USA).nes", "/roms/nes/aardvark.nes");
        cat.upsert_media_identity(
            &aard.id,
            "rom",
            Some("Aardvark (USA)"),
            Some("nes"),
            Some("eeee"),
            r#"{"matched":true,"also":[]}"#,
        )
        .unwrap();
        let zebra = rom_item(&cat, loc, "Zebra (USA).sfc", "/roms/snes/zebra.sfc");
        cat.upsert_media_identity(
            &zebra.id,
            "rom",
            Some("Zebra (USA)"),
            Some("snes"),
            Some("ffff"),
            r#"{"matched":true,"also":[]}"#,
        )
        .unwrap();
        spec.cursor = p1.next_cursor.clone();
        let p2 = cat.rom_home_shelf_window(&spec).unwrap();
        assert_eq!(p2.total, 7, "totals stay honest as the set grows");
        let k2 = page_group_keys(&p2);
        assert_eq!(k2.len(), 2);
        for k in &k2 {
            assert!(
                !page_group_keys(&p1).contains(k),
                "no group repeats after a mid-chain insert"
            );
            assert!(!k.contains("aardvark"), "the early insert waits for a refresh");
        }
        spec.cursor = p2.next_cursor.clone();
        let p3 = cat.rom_home_shelf_window(&spec).unwrap();
        let k3 = page_group_keys(&p3);
        assert_eq!(k3.len(), 2, "SMW + the late insert close the chain");
        assert!(k3.iter().any(|k| k.contains("zebra")));
        assert!(p3.next_cursor.is_none());
    }

    #[test]
    fn shelf_missing_pairs_cover_every_member_and_type() {
        let cat = Catalog::open_in_memory().unwrap();
        seed_shelf(&cat);
        let pairs = cat.rom_home_missing_pairs(&shelf_spec("all")).unwrap();
        // 1942: 3 (has boxart) · Chrono: 3 · Klax dumps: 4+4 ·
        // SMW: 4 (missing row is not found) · Metal Slug: 4.
        assert_eq!(pairs.len(), 22);
        let boxart_pairs = pairs.iter().filter(|p| p.media_type == "boxart").count();
        assert_eq!(
            boxart_pairs, 4,
            "both Klax dumps + SMW + Metal Slug lack box art; 1942 and Chrono hold it"
        );
        // Spec-scoped: the missing shelf's pairs exclude art-holders.
        let pairs = cat
            .rom_home_missing_pairs(&shelf_spec("missing"))
            .unwrap();
        assert_eq!(pairs.len(), 16, "only the three art-less groups' members");
    }

    #[test]
    fn shelf_window_search_titles_labels_and_platform_names() {
        let cat = Catalog::open_in_memory().unwrap();
        seed_shelf(&cat);
        let mut spec = shelf_spec("search");
        spec.query = Some("klax".to_string());
        assert_eq!(cat.rom_home_shelf_window(&spec).unwrap().total, 1);
        // Platform display-name match arrives as resolved keys.
        spec.query = Some("no title matches this".to_string());
        spec.search_platforms = vec!["snes".to_string()];
        assert_eq!(cat.rom_home_shelf_window(&spec).unwrap().total, 2);
        // Labels match through any member's path.
        cat.conn
            .execute(
                "INSERT OR IGNORE INTO path_labels (provider_key, label) VALUES ('/roms/snes/chrono.sfc', 'rpg-classics')",
                [],
            )
            .unwrap();
        spec.query = Some("rpg-classics".to_string());
        spec.search_platforms = vec![];
        let page = cat.rom_home_shelf_window(&spec).unwrap();
        assert_eq!(page.total, 1);
        assert!(page.games[0].title.starts_with("Chrono"));
    }

    #[test]
    fn shelf_window_list_shelf_restricts_to_members() {
        let cat = Catalog::open_in_memory().unwrap();
        let seed = seed_shelf(&cat);
        let mut spec = shelf_spec("list");
        // By item id: the whole Klax group joins through one dump.
        spec.member_keys = Some(vec![seed.klax_japan.id.to_string()]);
        let page = cat.rom_home_shelf_window(&spec).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.games.len(), 2);
        // By provider key.
        spec.member_keys = Some(vec!["/roms/snes/smw.sfc".to_string()]);
        let page = cat.rom_home_shelf_window(&spec).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.games[0].item.id, seed.smw.id);
        // Empty list: honestly empty.
        spec.member_keys = Some(vec![]);
        assert_eq!(cat.rom_home_shelf_window(&spec).unwrap().total, 0);
    }
    // --------------------------------------------------------
    // Hyperbase #4 SCALE HARNESS (ignored in gates; run with:
    //   cargo test -p ufm-catalog --release shelf_scale_harness
    //     -- --ignored --nocapture
    // Fixture: 103,450 groups over 60 platforms (one platform
    // holds 12,000), ~8% two-dump groups, ~412k items and
    // ~212k identities in the catalog. Measures the BEFORE path
    // (rom_home_games full fetch) against the AFTER path
    // (windowed shelves) and asserts the window walk is exact.
    // --------------------------------------------------------

    #[test]
    #[ignore = "scale harness — run explicitly in release"]
    fn shelf_scale_harness() {
        use std::collections::HashSet;
        use std::time::Instant;
        let dir = std::env::var("SHELF_SCALE_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::temp_dir().join(format!("fable-shelf-scale-{}", std::process::id()))
            });
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cat = Catalog::open(&dir.join("catalog.db")).unwrap();
        let loc = LocationId::new();
        cat.insert_location(&sample_location(loc)).unwrap();
        let loc_s = loc.to_string();

        const PLATFORMS: usize = 60;
        let groups_for = |p: usize| -> usize { if p == 0 { 12_000 } else { 1_550 } };
        let total_groups: usize = (0..PLATFORMS).map(&groups_for).sum();
        assert!(total_groups >= 100_000, "fixture must clear 100k groups: {total_groups}");

        let mut rng: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move || {
            rng = rng
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (rng >> 33) as u32
        };

        let t = Instant::now();
        {
            let conn = &cat.conn;
            conn.execute_batch("BEGIN").unwrap();
            let mut ins_item = conn
                .prepare("INSERT INTO items (id, location_id, name, kind, size_bytes, modified, provider_key) VALUES (?1, ?2, ?3, 'file', ?4, ?5, ?6)")
                .unwrap();
            let mut ins_ident = conn
                .prepare("INSERT INTO media_identities (item_id, media_kind, title, year, platform, sha1, match_evidence_json) VALUES (?1, 'rom', ?2, ?3, ?4, ?5, ?6)")
                .unwrap();
            let mut ins_media = conn
                .prepare("INSERT INTO rom_media (item_id, media_type, status, source, local_path, detail_json, checked_at) VALUES (?1, ?2, 'found', 'libretro', ?3, '{}', '2026-01-01T00:00:00Z')")
                .unwrap();
            let mut ins_state = conn
                .prepare("INSERT INTO rom_game_state (provider_key, rating, favorite, completed, play_count, last_played, title_override, genre, players, notes, updated_at) VALUES (?1, ?2, ?3, 0, ?4, ?5, NULL, ?6, ?7, NULL, '2026-01-01T00:00:00Z')")
                .unwrap();
            let mut member_seq = 0u64;
            let mut mk_id = |seq: u64| format!("{:08x}-0000-4000-8000-{:012x}", seq, seq);
            for p in 0..PLATFORMS {
                let platform = format!("plat{p:02}");
                for i in 0..groups_for(p) {
                    let base = format!("Game {p:02}-{i:05}");
                    let members: Vec<(String, &str)> = {
                        let mut v = vec![(format!("{base} (USA)"), "USA")];
                        if next() % 100 < 8 {
                            v.push((format!("{base} (Japan)"), "Japan"));
                        }
                        v
                    };
                    let year = 1985 + (i % 35) as i64;
                    for (mi, (title, region)) in members.iter().enumerate() {
                        member_seq += 1;
                        let id = mk_id(member_seq);
                        let pk = format!("/roms/{platform}/{base}-{region}.bin");
                        let modified = format!(
                            "2026-{:02}-{:02}T00:00:00Z",
                            (i % 12) + 1,
                            (i % 28) + 1
                        );
                        ins_item
                            .execute(rusqlite::params![
                                id,
                                loc_s,
                                format!("{base}-{region}.bin"),
                                1_000_000i64 + (i as i64 % 5000),
                                modified,
                                pk,
                            ])
                            .unwrap();
                        let evidence = format!(
                            "{{\"matched\":true,\"also\":[],\"crc\":{}}}",
                            10_000 + i
                        );
                        ins_ident
                            .execute(rusqlite::params![
                                id,
                                title,
                                year,
                                platform,
                                format!("{:040x}", member_seq),
                                evidence,
                            ])
                            .unwrap();
                        // Box art on 4 of 5 groups; screenshots on
                        // 1 of 3 (default member only, like a real
                        // scrape pass).
                        if mi == 0 && i % 5 != 0 {
                            ins_media
                                .execute(rusqlite::params![
                                    id,
                                    "boxart",
                                    format!("/media/{platform}/{base}.png"),
                                ])
                                .unwrap();
                        }
                        if mi == 0 && i % 3 == 0 {
                            ins_media
                                .execute(rusqlite::params![
                                    id,
                                    "screenshot",
                                    format!("/media/{platform}/{base}-shot.png"),
                                ])
                                .unwrap();
                        }
                        // Personal state sprinkle (scoped rows on
                        // the default member's path).
                        if mi == 0 {
                            let fav = i % 17 == 0;
                            let played = i % 7 == 0;
                            let rated = i % 23 == 0;
                            if fav || played || rated {
                                ins_state
                                    .execute(rusqlite::params![
                                        pk,
                                        if rated { Some(4i64) } else { None },
                                        fav as i64,
                                        if played { (i % 9 + 1) as i64 } else { 0 },
                                        if played {
                                            Some(format!(
                                                "2026-09-{:02}T20:00:00Z",
                                                (i % 28) + 1
                                            ))
                                        } else {
                                            None
                                        },
                                        if i % 4 == 0 { Some("Action") } else { None },
                                        if i % 3 == 0 { Some("1-2") } else { None },
                                    ])
                                    .unwrap();
                            }
                        }
                    }
                }
            }
            // Catalog ballast: 300k non-ROM files, 100k of them
            // carrying movie identities — the shelf queries must
            // not feel them beyond the join they already pay.
            for i in 0..300_000u64 {
                member_seq += 1;
                let id = mk_id(member_seq);
                ins_item
                    .execute(rusqlite::params![
                        id,
                        loc_s,
                        format!("pad-{i:06}.bin"),
                        10i64,
                        "2026-01-01T00:00:00Z",
                        format!("/pad/{i:06}.bin"),
                    ])
                    .unwrap();
                if i < 100_000 {
                    conn.execute(
                        "INSERT INTO media_identities (item_id, media_kind, title, year, platform, sha1, match_evidence_json) VALUES (?1, 'movie', ?2, 2001, NULL, NULL, '{}')",
                        rusqlite::params![id, format!("Pad Movie {i:06}")],
                    )
                    .unwrap();
                }
            }
            conn.execute_batch("COMMIT").unwrap();
        }
        let seed_secs = t.elapsed().as_secs_f64();

        // ---------- BEFORE: the unbounded full fetch ----------
        if std::env::var("SHELF_SCALE_SEED_ONLY").is_ok() {
            println!("seed-only: fixture kept at {}", dir.display());
            return;
        }
        let t = Instant::now();
        let before_all = cat.rom_home_games(None).unwrap();
        let before_all_ms = t.elapsed().as_millis();
        let before_all_bytes = serde_json::to_vec(&before_all).unwrap().len();
        let before_ids: HashSet<String> = before_all
            .iter()
            .map(|g| g.item.id.to_string())
            .collect();
        let before_groups: HashSet<String> =
            before_all.iter().map(|g| g.group_key.clone()).collect();

        let t = Instant::now();
        let before_big = cat.rom_home_games(Some("plat00")).unwrap();
        let before_big_ms = t.elapsed().as_millis();
        let before_big_bytes = serde_json::to_vec(&before_big).unwrap().len();
        let before_big_ids: HashSet<String> = before_big
            .iter()
            .map(|g| g.item.id.to_string())
            .collect();

        let t = Instant::now();
        let counts = cat.rom_home_counts().unwrap();
        let counts_ms = t.elapsed().as_millis();
        assert_eq!(counts.total as usize, before_groups.len());

        // ---------- AFTER: windowed shelves ----------
        let spec_all = RomHomeShelfSpec {
            kind: "all".into(),
            platform: None,
            query: None,
            search_platforms: vec![],
            member_keys: None,
            pc_source: None,
            filters: RomHomeShelfFilters::default(),
            sort: "title".into(),
            cursor: None,
            limit: 240,
        };
        let t = Instant::now();
        let first = cat.rom_home_shelf_window(&spec_all).unwrap();
        let first_ms = t.elapsed().as_millis();
        let first_bytes = serde_json::to_vec(&first.games).unwrap().len();
        assert_eq!(first.total as usize, before_groups.len());
        assert_eq!(page_group_keys(&first).len(), 240);

        if std::env::var("SHELF_SCALE_PROBE").is_ok() {
            println!("probe mode: counts_ms={counts_ms} first_ms={first_ms} first_bytes={first_bytes} before_all_ms={before_all_ms} before_all_bytes={before_all_bytes}");
            if std::env::var("SHELF_SCALE_DIR").is_err() {
                let _ = std::fs::remove_dir_all(&dir);
            }
            return;
        }
        // Full walk of the all-shelf: exact union, stable total.
        let t = Instant::now();
        let mut walk_ids: HashSet<String> = HashSet::new();
        let mut walk_groups: HashSet<String> = HashSet::new();
        let mut windows = 0usize;
        let mut cursor: Option<String> = None;
        loop {
            let mut spec = spec_all.clone();
            spec.cursor = cursor.clone();
            let page = cat.rom_home_shelf_window(&spec).unwrap();
            assert_eq!(page.total as usize, before_groups.len(), "total drifted mid-walk");
            windows += 1;
            for g in &page.games {
                assert!(walk_ids.insert(g.item.id.to_string()), "duplicate item across windows");
                walk_groups.insert(g.group_key.clone());
            }
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
            assert!(windows < 10_000, "walk runaway");
        }
        let walk_ms = t.elapsed().as_millis();
        assert_eq!(walk_ids, before_ids, "window union != full set");
        assert_eq!(walk_groups.len(), before_groups.len());

        // Bulk walk (Show-all shape): 5,000-group windows must
        // reach the same union far faster in wall time.
        let t = Instant::now();
        let mut bulk_ids: HashSet<String> = HashSet::new();
        let mut bulk_windows = 0usize;
        let mut cursor: Option<String> = None;
        loop {
            let mut spec = spec_all.clone();
            spec.limit = 5000;
            spec.cursor = cursor.clone();
            let page = cat.rom_home_shelf_window(&spec).unwrap();
            assert_eq!(page.total as usize, before_groups.len());
            bulk_windows += 1;
            for g in &page.games {
                assert!(bulk_ids.insert(g.item.id.to_string()), "bulk dup");
            }
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
            assert!(bulk_windows < 100, "bulk walk runaway");
        }
        let bulk_ms = t.elapsed().as_millis();
        assert_eq!(bulk_ids, before_ids, "bulk union != full set");

        // The 12,000-group platform: 50 windows of 240.
        let mut spec_big = spec_all.clone();
        spec_big.kind = "system".into();
        spec_big.platform = Some("plat00".into());
        let mut big_ids: HashSet<String> = HashSet::new();
        let mut big_windows = 0usize;
        let mut cursor: Option<String> = None;
        loop {
            let mut spec = spec_big.clone();
            spec.cursor = cursor.clone();
            let page = cat.rom_home_shelf_window(&spec).unwrap();
            assert_eq!(page.total, 12_000);
            big_windows += 1;
            for g in &page.games {
                assert!(big_ids.insert(g.item.id.to_string()));
            }
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        assert_eq!(big_windows, 50);
        assert_eq!(big_ids, before_big_ids);

        // Filtered totals agree with the tallies.
        let mut spec_fav = spec_all.clone();
        spec_fav.kind = "favorites".into();
        let fav_page = cat.rom_home_shelf_window(&spec_fav).unwrap();
        assert_eq!(fav_page.total, counts.favorite_groups);
        assert!(fav_page.total > 1_000, "sprinkle too thin: {}", fav_page.total);
        let mut spec_missing = spec_all.clone();
        spec_missing.kind = "missing".into();
        let missing_page = cat.rom_home_shelf_window(&spec_missing).unwrap();
        assert_eq!(missing_page.total, counts.missing_media);
        assert!(missing_page.total > 10_000);

        // Year sort: the first window's default-version years
        // descend (ties fall to title, so year is non-increasing).
        let mut spec_year = spec_all.clone();
        spec_year.sort = "year".into();
        let year_page = cat.rom_home_shelf_window(&spec_year).unwrap();
        let years: Vec<i64> = page_group_keys(&year_page)
            .iter()
            .map(|gk| {
                year_page
                    .games
                    .iter()
                    .find(|g| &g.group_key == gk)
                    .and_then(|g| g.year)
                    .unwrap_or(-1)
            })
            .collect();
        assert!(
            years.windows(2).all(|w| w[0] >= w[1]),
            "year window not descending: {years:?}"
        );

        println!("\n================ SHELF SCALE (Hyperbase #4) ================");
        println!("fixture: {total_groups} groups / 60 platforms (big = 12,000), seeded in {seed_secs:.1}s");
        println!("counts: total={} missing={} favorites={} recent={} ({} ms)",
            counts.total, counts.missing_media, counts.favorite_groups, counts.recent_groups, counts_ms);
        println!("BEFORE rom_home_games(None): {} rows, {} bytes JSON, {} ms",
            before_all.len(), before_all_bytes, before_all_ms);
        println!("BEFORE rom_home_games(plat00): {} rows, {} bytes JSON, {} ms",
            before_big.len(), before_big_bytes, before_big_ms);
        println!("AFTER first window (240 groups): {} member rows, {} bytes JSON, {} ms",
            first.games.len(), first_bytes, first_ms);
        println!("AFTER full walk: {} windows, {} ms total (union exact, totals stable)",
            windows, walk_ms);
        println!("AFTER bulk walk (5000/window): {} windows, {} ms total (union exact)",
            bulk_windows, bulk_ms);
        println!("============================================================\n");

        if std::env::var("SHELF_SCALE_DIR").is_err() {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    // -- Ebook P1 — The Library --------------------------------------

    fn book_meta(item: &Item, title: &str) -> BookMetadataRow {
        BookMetadataRow {
            item_id: item.id.to_string(),
            provider_key: item.provider_key.clone().unwrap(),
            format: "epub".into(),
            title: Some(title.into()),
            authors: vec!["Søren Åberg".into()],
            series: None,
            series_index: None,
            publisher: Some("Fixture House".into()),
            published_date: Some("2011-08-23".into()),
            pub_year: Some(2011),
            language: Some("en".into()),
            description: Some("A tale.".into()),
            subjects: vec!["Fiction".into()],
            isbn: Some("9780134685991".into()),
            identifier: Some("urn:fixture:1".into()),
            spine_count: Some(12),
            page_count: None,
            cover_path: None,
            source: "opf".into(),
            size_bytes: item.size_bytes.map(|s| s as i64),
            modified: item.modified.map(|m| m.to_rfc3339()),
            identified_at: "2026-10-17T09:00:00+00:00".into(),
        }
    }

    fn seed_book(cat: &Catalog, loc: LocationId, name: &str) -> Item {
        let item = named_item(loc, name, ItemKind::File);
        cat.insert_item(&item).unwrap();
        item
    }

    #[test]
    fn book_metadata_roundtrip_writes_ebook_identity() {
        let cat = Catalog::open_in_memory().unwrap();
        let loc = LocationId::new();
        cat.insert_location(&sample_location(loc)).unwrap();
        let item = seed_book(&cat, loc, "Cafe Creme.epub");
        let mut meta = book_meta(&item, "Café & Crème");
        meta.authors = vec!["Søren Åberg".into(), "Second Author".into()];
        meta.series = Some("The Alloy Wars".into());
        meta.series_index = Some(3.0);
        cat.upsert_book_metadata(&meta).unwrap();

        let back = cat.book_metadata_for(&item.id.to_string()).unwrap().unwrap();
        assert_eq!(back, meta, "metadata must round-trip exactly");
        // The identity row: MediaKind::Ebook's persisted assignment.
        let ident = cat.media_identity_for(&item.id).unwrap().unwrap();
        assert_eq!(ident.media_kind, "ebook");
        assert_eq!(ident.title.as_deref(), Some("Café & Crème"));
        assert_eq!(ident.year, Some(2011));
        assert_eq!(ident.platform.as_deref(), Some("epub"));
        assert!(ident.evidence_json.contains("9780134685991"), "{}", ident.evidence_json);
        // Fresh snapshot → no longer a candidate.
        assert!(cat.book_candidates(None).unwrap().is_empty());
        assert!(cat.book_unsorted_items().unwrap().is_empty());
    }

    #[test]
    fn book_candidates_extension_and_freshness_rules() {
        let cat = Catalog::open_in_memory().unwrap();
        let loc = LocationId::new();
        cat.insert_location(&sample_location(loc)).unwrap();
        let epub = seed_book(&cat, loc, "A.epub");
        let _pdf = seed_book(&cat, loc, "B.PDF");
        let _txt = seed_book(&cat, loc, "C.txt");
        let cands = cat.book_candidates(None).unwrap();
        let names: Vec<&str> = cands.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["A.epub", "B.PDF"], "book extensions only, case-insensitive");
        // Location scoping: another location sees none of them.
        assert!(cat.book_candidates(Some("no-such-location")).unwrap().is_empty());
        assert_eq!(cat.book_candidates(Some(&loc.to_string())).unwrap().len(), 2);
        // Stale snapshot (size moved) → candidate again.
        let mut meta = book_meta(&epub, "A");
        meta.size_bytes = Some(999_999);
        cat.upsert_book_metadata(&meta).unwrap();
        let cands = cat.book_candidates(None).unwrap();
        assert_eq!(cands.len(), 2, "stale epub + untouched pdf");
        assert!(cands.iter().any(|i| i.name == "A.epub"));
    }

    #[test]
    fn book_candidates_rejudge_pageless_pdf_rows() {
        // The paper-100k scar (2026-10-18, crew Dewey): a PDF row
        // with NO page count was shelved by the old
        // extension-trusting rules and — its size/mtime still
        // matched — would never be judged again. A validated PDF
        // always has a page count, so the row owes a re-judge; a
        // validated one (page count present) stays fresh; an EPUB
        // row (spine_count instead of pages) is never swept up.
        let cat = Catalog::open_in_memory().unwrap();
        let loc = LocationId::new();
        cat.insert_location(&sample_location(loc)).unwrap();
        let fake = seed_book(&cat, loc, "paper-100k.pdf");
        let real = seed_book(&cat, loc, "Real Book.pdf");
        let epub = seed_book(&cat, loc, "Novel.epub");
        let mut fake_meta = book_meta(&fake, "paper-100k");
        fake_meta.format = "pdf".into();
        fake_meta.page_count = None;
        fake_meta.spine_count = None;
        cat.upsert_book_metadata(&fake_meta).unwrap();
        let mut real_meta = book_meta(&real, "Real Book");
        real_meta.format = "pdf".into();
        real_meta.page_count = Some(212);
        real_meta.spine_count = None;
        cat.upsert_book_metadata(&real_meta).unwrap();
        cat.upsert_book_metadata(&book_meta(&epub, "Novel")).unwrap();
        let cands = cat.book_candidates(None).unwrap();
        let names: Vec<&str> = cands.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["paper-100k.pdf"], "only the page-less PDF row re-judges");
    }

    #[test]
    fn delete_book_metadata_withdraws_the_identification() {
        let cat = Catalog::open_in_memory().unwrap();
        let loc = LocationId::new();
        cat.insert_location(&sample_location(loc)).unwrap();
        let item = seed_book(&cat, loc, "paper-100k.pdf");
        let mut meta = book_meta(&item, "paper-100k");
        meta.format = "pdf".into();
        cat.upsert_book_metadata(&meta).unwrap();
        assert!(cat.book_metadata_for(&item.id.to_string()).unwrap().is_some());
        assert!(cat.delete_book_metadata(&item.id.to_string()).unwrap());
        assert!(cat.book_metadata_for(&item.id.to_string()).unwrap().is_none());
        assert!(
            cat.media_identity_for(&item.id).unwrap().is_none(),
            "the 'ebook' media identity is withdrawn with the row"
        );
        // The item itself survives as a plain file, a candidate again.
        assert!(cat
            .book_candidates(None)
            .unwrap()
            .iter()
            .any(|i| i.name == "paper-100k.pdf"));
        // Withdrawing twice is a no-op, not an error.
        assert!(!cat.delete_book_metadata(&item.id.to_string()).unwrap());
    }

    #[test]
    fn book_series_shelf_orders_and_reports_gaps() {
        let cat = Catalog::open_in_memory().unwrap();
        let loc = LocationId::new();
        cat.insert_location(&sample_location(loc)).unwrap();
        for (name, idx) in [("Wars Three.epub", Some(3.0)), ("Wars One.epub", Some(1.0)), ("Wars Novella.epub", Some(0.5)), ("Wars Unnumbered.epub", None)] {
            let item = seed_book(&cat, loc, name);
            let mut meta = book_meta(&item, name);
            meta.series = Some("The Alloy Wars".into());
            meta.series_index = idx;
            cat.upsert_book_metadata(&meta).unwrap();
        }
        let shelves = cat.books_series_shelves().unwrap();
        assert_eq!(shelves.len(), 1);
        let shelf = &shelves[0];
        assert_eq!(shelf.present, vec![1, 3]);
        assert_eq!(shelf.missing, vec![2], "the 0.5 novella neither fills nor creates a gap");
        let order: Vec<Option<f64>> = shelf.books.iter().map(|b| b.meta.series_index).collect();
        assert_eq!(order, vec![Some(0.5), Some(1.0), Some(3.0), None], "reading order, unnumbered last");
    }

    #[test]
    fn series_missing_positions_math() {
        assert_eq!(series_missing_positions(&[]), Vec::<i64>::new());
        assert_eq!(series_missing_positions(&[1.0, 2.0, 3.0]), Vec::<i64>::new());
        assert_eq!(series_missing_positions(&[2.0]), vec![1]);
        assert_eq!(series_missing_positions(&[1.0, 3.0]), vec![2]);
        assert_eq!(series_missing_positions(&[0.5]), Vec::<i64>::new());
        assert_eq!(series_missing_positions(&[1.0, 1.0, 4.0]), vec![2, 3]);
    }

    #[test]
    fn book_author_facets_match_any_credited_author() {
        let cat = Catalog::open_in_memory().unwrap();
        let loc = LocationId::new();
        cat.insert_location(&sample_location(loc)).unwrap();
        let item = seed_book(&cat, loc, "Duo.epub");
        let mut meta = book_meta(&item, "Duo");
        meta.authors = vec!["First Writer".into(), "Second Author".into()];
        meta.series = Some("Duo Saga".into());
        cat.upsert_book_metadata(&meta).unwrap();
        assert_eq!(cat.items_by_book_author("second author").unwrap().len(), 1, "any author, case-insensitive");
        assert_eq!(cat.items_by_book_author("First Writer").unwrap().len(), 1);
        assert!(cat.items_by_book_author("Nobody").unwrap().is_empty());
        assert_eq!(cat.items_by_book_series("duo saga").unwrap().len(), 1);
        let shelves = cat.books_author_shelves().unwrap();
        assert_eq!(shelves.len(), 1);
        assert_eq!(shelves[0].0, "First Writer", "author shelf groups by primary author");
    }

    #[test]
    fn book_state_touch_rules_respect_the_users_word() {
        let cat = Catalog::open_in_memory().unwrap();
        let pk = "/fake/States.epub";
        // No state → progress touch creates auto 'reading'.
        cat.touch_book_reading(pk, "2026-10-17T10:00:00+00:00").unwrap();
        assert_eq!(cat.book_state_for(pk).unwrap().unwrap().0, "reading");
        // User marks Want AFTER (newer timestamp) → a stale progress
        // save (older now) must not override the user's word.
        cat.set_book_state(pk, "want", "user", "2026-10-17T11:00:00+00:00").unwrap();
        cat.touch_book_reading(pk, "2026-10-17T10:30:00+00:00").unwrap();
        assert_eq!(cat.book_state_for(pk).unwrap().unwrap().0, "want");
        // But reading again (newer than the mark) tells the truth.
        cat.touch_book_reading(pk, "2026-10-17T12:00:00+00:00").unwrap();
        assert_eq!(cat.book_state_for(pk).unwrap().unwrap().0, "reading");
        // 'read' is never auto-demoted.
        cat.set_book_state(pk, "read", "user", "2026-10-17T13:00:00+00:00").unwrap();
        cat.touch_book_reading(pk, "2026-10-17T14:00:00+00:00").unwrap();
        assert_eq!(cat.book_state_for(pk).unwrap().unwrap().0, "read");
        // State facet + counts read the same truth.
        assert_eq!(cat.items_by_book_state("read").unwrap().len(), 0, "no item row exists for this path — facet joins items");
        let counts = cat.books_counts().unwrap();
        assert_eq!(counts.read, 1);
    }

    #[test]
    fn book_counts_separate_identified_from_unsorted() {
        let cat = Catalog::open_in_memory().unwrap();
        let loc = LocationId::new();
        cat.insert_location(&sample_location(loc)).unwrap();
        let known = seed_book(&cat, loc, "Known.epub");
        let _unknown = seed_book(&cat, loc, "Unknown.epub");
        let _movie = seed_book(&cat, loc, "Film.mkv");
        cat.upsert_book_metadata(&book_meta(&known, "Known")).unwrap();
        let counts = cat.books_counts().unwrap();
        assert_eq!(counts.identified, 1);
        assert_eq!(counts.unsorted, 1);
        assert_eq!(counts.authors, 1);
        assert_eq!(counts.series, 0);
        let all = cat.books_all().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].meta.title.as_deref(), Some("Known"));
    }
}
