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

impl Catalog {
    /// Open (or create) the catalog. WAL mode + foreign keys, always.
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let conn = Connection::open(path).map_err(|e| UfmError::Database(e.to_string()))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
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
        let stable_id: String = match item.provider_key.as_deref() {
            Some(pk) => self
                .conn
                .query_row(
                    "SELECT id FROM items WHERE location_id = ?1 AND provider_key = ?2",
                    params![item.location.to_string(), pk],
                    |r| r.get(0),
                )
                .unwrap_or_else(|_| item.id.to_string()),
            None => item.id.to_string(),
        };
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
}

#[cfg(test)]
mod tests {
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
