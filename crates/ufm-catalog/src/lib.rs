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
        let existing: Option<(String, i64, String)> = match item.provider_key.as_deref() {
            Some(pk) => self
                .conn
                .query_row(
                    "SELECT id, rowid, name FROM items WHERE location_id = ?1 AND provider_key = ?2",
                    params![item.location.to_string(), pk],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .ok(),
            None => None,
        };
        let stable_id: String = existing
            .as_ref()
            .map(|(id, _, _)| id.clone())
            .unwrap_or_else(|| item.id.to_string());
        // items_fts is external-content: the REPLACE below deletes the
        // items row and re-inserts it under a NEW rowid, which would
        // orphan the old FTS entry (ghost hits, and worse, a recycled
        // rowid can join the ghost to an unrelated item). Retire the
        // old entry with the FTS 'delete' command first, exactly as
        // delete_unseen does.
        if let Some((_, old_rowid, old_name)) = &existing {
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
                 (id, location_id, name, kind, size_bytes, modified, provider_key)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    stable_id,
                    item.location.to_string(),
                    item.name,
                    format!("{:?}", item.kind).to_lowercase(),
                    item.size_bytes.map(|s| s as i64),
                    item.modified.map(|m| m.to_rfc3339()),
                    item.provider_key,
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
                "SELECT item_id, media_kind, title, platform, sha1, match_evidence_json
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
}
