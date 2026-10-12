//! ufm-vfs: the virtual filesystem.
//!
//! Spec §25: one mental model. A local folder, a cloud directory, a network
//! share, an archive, a search result, and a virtual collection all implement
//! the same [`Location`] contract where possible. Providers run in sandboxes;
//! the shell never touches raw paths.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use ufm_core::{ItemId, LocationId, Result};

// ---------------------------------------------------------------------------
// Item — the common currency of every view (list, grid, search, collection).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: ItemId,
    pub location: LocationId,
    /// Display name. The extension-visibility toggle (spec §7, P0 view option)
    /// is a *view profile* concern — the item always knows its true name.
    pub name: String,
    pub kind: ItemKind,
    pub size_bytes: Option<u64>,
    pub modified: Option<chrono::DateTime<chrono::Utc>>,
    /// Stable provider-side identity when the provider offers one
    /// (inode+device, cloud id, archive offset…). Used for move detection.
    pub provider_key: Option<String>,
    pub labels: Vec<String>,
    pub starred: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    File,
    Folder,
    Symlink,
    Archive,
    Volume,
}

// ---------------------------------------------------------------------------
// Location — one mounted root behind the location contract.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Location {
    pub id: LocationId,
    pub provider: String, // "local", "pcloud", "sftp", "zip", "search", …
    pub display_name: String,
    pub capabilities: Capabilities,
    pub protected: ProtectionTier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectionTier {
    None,
    Caution,       // warn before destructive ops
    AuthRequired,  // companion-device confirmation
    Blocked,       // policy block (system paths, boot partitions)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Capabilities {
    pub read: bool,
    pub write: bool,
    pub delete: bool,
    pub watch: bool,       // filesystem watcher / change feed available
    pub stream: bool,       // random-access byte streams (for preview)
    pub trash: bool,        // soft-delete available
    pub extended_attrs: bool,
}

// ---------------------------------------------------------------------------
// Provider contract — every backend implements this. Local today; cloud,
// SFTP/WebDAV, archives, and search-results-as-locations follow the same
// trait, which is what makes "one mental model" real.
// ---------------------------------------------------------------------------

#[async_trait]
pub trait VfsProvider: Send + Sync {
    fn provider_name(&self) -> &'static str;

    async fn list(&self, location: &Location, dir: &Item) -> Result<Vec<Item>>;

    async fn metadata(&self, item: &Item) -> Result<Item>;

    /// Byte stream for preview/playback. Never hands a raw path to the UI.
    async fn open_stream(
        &self,
        item: &Item,
        offset: u64,
        len: Option<u64>,
    ) -> Result<tokio::io::DuplexStream>;

    async fn rename(&self, item: &Item, new_name: &str) -> Result<Item>;
}

// ---------------------------------------------------------------------------
// Local provider — the reference implementation. Everything else is measured
// against it.
// ---------------------------------------------------------------------------

pub struct LocalProvider;

#[async_trait]
impl VfsProvider for LocalProvider {
    fn provider_name(&self) -> &'static str {
        "local"
    }

    async fn list(&self, _location: &Location, _dir: &Item) -> Result<Vec<Item>> {
        // TODO(spec §25): walkdir-backed listing with watcher integration.
        // Protected-tier check happens in the job layer, not here.
        todo!("local listing")
    }

    async fn metadata(&self, _item: &Item) -> Result<Item> {
        todo!("local metadata")
    }

    async fn open_stream(
        &self,
        _item: &Item,
        _offset: u64,
        _len: Option<u64>,
    ) -> Result<tokio::io::DuplexStream> {
        todo!("local stream")
    }

    async fn rename(&self, _item: &Item, _new_name: &str) -> Result<Item> {
        todo!("local rename")
    }
}

/// Mount a plain directory as a location. The UI never sees the [`PathBuf`].
pub fn mount_local(display_name: impl Into<String>, _root: PathBuf) -> Location {
    Location {
        id: LocationId::new(),
        provider: "local".into(),
        display_name: display_name.into(),
        capabilities: Capabilities {
            read: true,
            write: true,
            delete: true,
            watch: true,
            stream: true,
            trash: true,
            extended_attrs: true,
        },
        protected: ProtectionTier::None,
    }
}
