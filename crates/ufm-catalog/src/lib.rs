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
                    location_id TEXT PRIMARY KEY REFERENCES locations(id) ON DELETE CASCADE,
                    show_extensions INTEGER NOT NULL DEFAULT 1,
                    view_mode TEXT NOT NULL DEFAULT 'list',
                    sort_json TEXT NOT NULL DEFAULT '{}'
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
    /// (item, has_identity_row). ROM-context scoping (which folders
    /// count as ROM territory) is the caller's job — it needs path
    /// evidence, applied in commands.rs with `rom_identified_folders`.
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

    pub fn view_profile(&self, location: &LocationId) -> Result<Option<ViewProfileRow>> {
        use rusqlite::OptionalExtension;
        self.conn
            .query_row(
                "SELECT view_mode, sort_json, show_extensions FROM view_profiles WHERE location_id = ?1",
                params![location.to_string()],
                |r| {
                    Ok(ViewProfileRow {
                        view_mode: r.get(0)?,
                        sort_json: r.get(1)?,
                        show_extensions: r.get::<_, i64>(2)? != 0,
                    })
                },
            )
            .optional()
            .map_err(|e| UfmError::Database(e.to_string()))
    }

    pub fn set_view_profile(
        &self,
        location: &LocationId,
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
                "INSERT OR REPLACE INTO view_profiles (location_id, show_extensions, view_mode, sort_json)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    location.to_string(),
                    profile.show_extensions as i64,
                    profile.view_mode,
                    profile.sort_json
                ],
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        Ok(())
    }

    // -- Path re-keying (in-app rename/move) --------------------------

    /// A file or folder was renamed/moved inside Fable: carry its
    /// stars and collection memberships to the new path. Prefix-aware:
    /// descendants of a renamed folder follow. (Labels deliberately
    /// keep their existing behavior — this serves the Phase 2 stores.)
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

/// One row of the ROM Home shelf queries: everything the room
/// renders for a game.
#[derive(Debug, Clone)]
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
        let mut sql = String::from(
            "SELECT i.id, i.location_id, i.name, i.kind, i.size_bytes,
                    i.modified, i.provider_key, mi.title, mi.platform,
                    mi.year, mi.sha1, mi.match_evidence_json
             FROM media_identities AS mi
             JOIN items AS i ON i.id = mi.item_id
             WHERE mi.media_kind = 'rom' AND mi.title IS NOT NULL
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
        let raw: Vec<(
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
        )> = match platform {
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
            let also_platforms: Vec<String> = serde_json::from_str::<serde_json::Value>(&evidence)
                .ok()
                .and_then(|v| v.get("also").cloned())
                .and_then(|a| serde_json::from_value(a).ok())
                .unwrap_or_default();
            let labels = match &provider_key {
                Some(pk) => self.labels_for_path(pk).unwrap_or_default(),
                None => Vec::new(),
            };
            let is_starred = provider_key
                .as_ref()
                .map(|pk| starred.contains(pk))
                .unwrap_or(false);
            let group_key = ufm_media::romdat::rom_group_key(&platform_s, &title);
            let variant = ufm_media::romdat::variant_of_title(&title);
            let crc: Option<String> =
                serde_json::from_str::<serde_json::Value>(&evidence)
                    .ok()
                    .and_then(|v| v.get("crc").and_then(|c| c.as_u64()))
                    .map(|n| format!("{n:08x}"));
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
            });
        }
        Ok(out)
    }

    /// The room's tallies, from the catalog alone — counted in
    /// GAMES (Phase 3 groups), not files: total = distinct group
    /// keys; a group counts as missing media when NO member item
    /// has found box art; the per-system split uses the same
    /// membership rule as `rom_home_games` (primary + `also`
    /// platforms), counting each group once per shelf it sits on.
    pub fn rom_home_counts(&self) -> Result<RomHomeCounts> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT mi.platform, mi.title, mi.match_evidence_json,
                        EXISTS (SELECT 1 FROM rom_media AS rm
                                WHERE rm.item_id = mi.item_id
                                  AND rm.media_type = 'boxart'
                                  AND rm.status = 'found'
                                  AND rm.local_path IS NOT NULL)
                 FROM media_identities AS mi
                 WHERE mi.media_kind = 'rom' AND mi.title IS NOT NULL
                   AND mi.platform IS NOT NULL",
            )
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .map_err(|e| UfmError::Database(e.to_string()))?;
        let mut counts = RomHomeCounts::default();
        // group key -> any member has box art (global, and per shelf).
        let mut art_by_key: std::collections::HashMap<String, bool> =
            std::collections::HashMap::new();
        let mut art_by_shelf: std::collections::HashMap<
            String,
            std::collections::HashMap<String, bool>,
        > = std::collections::HashMap::new();
        for r in rows {
            let (platform, title, evidence, has_art) =
                r.map_err(|e| UfmError::Database(e.to_string()))?;
            let key = ufm_media::romdat::rom_group_key(&platform, &title);
            let art = has_art != 0;
            *art_by_key.entry(key.clone()).or_insert(false) |= art;
            let mut shelves = vec![platform];
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&evidence) {
                if let Some(also) = v.get("also").and_then(|a| a.as_array()) {
                    for a in also {
                        if let Some(s) = a.as_str() {
                            if !shelves.iter().any(|x| x == s) {
                                shelves.push(s.to_string());
                            }
                        }
                    }
                }
            }
            for s in shelves {
                *art_by_shelf
                    .entry(s)
                    .or_default()
                    .entry(key.clone())
                    .or_insert(false) |= art;
            }
        }
        counts.total = art_by_key.len() as i64;
        counts.missing_media =
            art_by_key.values().filter(|art| !**art).count() as i64;
        let mut per_system: Vec<(String, i64, i64)> = art_by_shelf
            .into_iter()
            .map(|(p, keys)| {
                let missing = keys.values().filter(|art| !**art).count() as i64;
                (p, keys.len() as i64, missing)
            })
            .collect();
        per_system.sort_by(|a, b| a.0.cmp(&b.0));
        counts.per_system = per_system;
        Ok(counts)
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
                 WHERE i.provider_key = ?1 AND mi.media_kind = 'rom'
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
    fn view_profile_roundtrip() {
        let cat = Catalog::open_in_memory().unwrap();
        let loc = LocationId::new();
        assert!(cat.view_profile(&loc).unwrap().is_none());
        let profile = ViewProfileRow {
            view_mode: "list".into(),
            sort_json: "{\"key\":\"modified\",\"dir\":-1}".into(),
            show_extensions: false,
        };
        cat.set_view_profile(&loc, &profile).unwrap();
        let got = cat.view_profile(&loc).unwrap().unwrap();
        assert_eq!(got.view_mode, "list");
        assert_eq!(got.sort_json, profile.sort_json);
        assert!(!got.show_extensions);
        // Overwrite wins.
        let p2 = ViewProfileRow {
            view_mode: "grid".into(),
            ..profile
        };
        cat.set_view_profile(&loc, &p2).unwrap();
        assert_eq!(cat.view_profile(&loc).unwrap().unwrap().view_mode, "grid");
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
}
