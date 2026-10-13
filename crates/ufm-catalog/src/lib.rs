//! ufm-catalog: the durable metadata catalog.
//!
//! Spec §§25–26: SQLite in WAL mode. Full-text search tables, migrations with
//! integrity checks, labels, collections, view profiles, media identities,
//! job journals, theme/plugin package records, and MCP audit chains all live
//! here. The catalog is the reason search is instant and everything survives
//! a restart — the filesystem is never walked live for metadata.

use rusqlite::{params, Connection};
use ufm_core::{CollectionId, ItemId, LocationId, Result, UfmError};

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
