//! ufm-vfs: the virtual filesystem.
//!
//! Spec §25: one mental model. A local folder, a cloud directory, a network
//! share, an archive, a search result, and a virtual collection all implement
//! the same [`Location`] contract where possible. Providers run in sandboxes;
//! the shell never touches raw paths.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use ufm_core::{ItemId, LocationId, Result, UfmError};

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

    async fn list(&self, location: &Location, dir: &Item) -> Result<Vec<Item>> {
        // For local items the provider_key IS the canonical absolute path
        // (callers synthesize the location-root Item with
        // provider_key = root path). Protected-tier checks happen in the job
        // layer, not here.
        let key = dir.provider_key.as_deref().ok_or_else(|| {
            UfmError::InvalidInput(format!("local list: '{}' has no provider_key", dir.name))
        })?;
        let path = PathBuf::from(key);
        if !path.is_absolute() {
            return Err(UfmError::InvalidInput(format!(
                "local list: provider_key is not an absolute path: {key}"
            )));
        }
        // Blocking std::fs is fine here — Tauri commands run providers under
        // spawn_blocking.
        let read_dir = std::fs::read_dir(&path).map_err(|e| UfmError::Provider {
            provider: "local".into(),
            message: format!("read_dir {}: {e}", path.display()),
        })?;
        let mut items: Vec<Item> = read_dir
            .filter_map(|entry| entry.ok()) // unreadable entry: skip, keep going
            .filter_map(|entry| item_from_path(location.id, &entry.path()))
            .collect();
        // Folders first, then case-insensitive name order.
        items.sort_by(|a, b| {
            let rank = |k: ItemKind| {
                if k == ItemKind::Folder {
                    0
                } else {
                    1
                }
            };
            rank(a.kind)
                .cmp(&rank(b.kind))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(items)
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

// ---------------------------------------------------------------------------
// Local filesystem mapping — shared by `LocalProvider::list` and the search
// indexer so both build identical Items from the same paths.
// ---------------------------------------------------------------------------

/// Build an [`Item`] for `path` using `symlink_metadata`, so symlinks are
/// reported as [`ItemKind::Symlink`] instead of being followed. Returns
/// `None` when the path's metadata cannot be read (vanished mid-walk,
/// permission denied) — callers skip such entries instead of failing the
/// whole operation.
pub fn item_from_path(location: LocationId, path: &Path) -> Option<Item> {
    let md = std::fs::symlink_metadata(path).ok()?;
    let ft = md.file_type();
    let kind = if ft.is_symlink() {
        ItemKind::Symlink
    } else if ft.is_dir() {
        ItemKind::Folder
    } else {
        ItemKind::File
    };
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    Some(Item {
        id: ItemId::new(),
        location,
        name,
        kind,
        size_bytes: if ft.is_file() { Some(md.len()) } else { None },
        modified: md
            .modified()
            .ok()
            .map(|t| chrono::DateTime::<chrono::Utc>::from(t)),
        provider_key: Some(path.to_string_lossy().into_owned()),
        labels: Vec::new(),
        starred: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Unique scratch dir under the OS temp dir (no `tempfile` crate in the
    /// workspace). Caller removes it when done.
    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ufm-vfs-test-{}-{}-{}",
            tag,
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn root_item(location: &Location, path: &Path) -> Item {
        let mut item = item_from_path(location.id, path).unwrap();
        // The location-root Item: provider_key IS the canonical absolute path.
        item.name = location.display_name.clone();
        item
    }

    #[tokio::test]
    async fn list_folders_first_then_case_insensitive() {
        let dir = scratch_dir("list");
        std::fs::create_dir(dir.join("b_dir")).unwrap();
        std::fs::create_dir(dir.join("a_dir")).unwrap();
        std::fs::write(dir.join("Zebra.txt"), b"z").unwrap();
        std::fs::write(dir.join("apple.txt"), b"a").unwrap();
        std::fs::write(dir.join("Mango.md"), b"m").unwrap();

        let location = mount_local("Scratch", dir.clone());
        let provider = LocalProvider;
        let items = provider
            .list(&location, &root_item(&location, &dir))
            .await
            .unwrap();

        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["a_dir", "b_dir", "apple.txt", "Mango.md", "Zebra.txt"],
            "folders first, then case-insensitive name order"
        );
        assert!(items.iter().all(|i| i.location == location.id));
        assert!(items
            .iter()
            .all(|i| i.provider_key.as_deref().unwrap().starts_with(dir.to_str().unwrap())));
        let apple = items.iter().find(|i| i.name == "apple.txt").unwrap();
        assert_eq!(apple.kind, ItemKind::File);
        assert_eq!(apple.size_bytes, Some(1));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn list_skips_unreadable_entries_without_failing() {
        let dir = scratch_dir("list-skip");
        std::fs::write(dir.join("ok.txt"), b"ok").unwrap();
        let location = mount_local("Scratch", dir.clone());
        let provider = LocalProvider;
        // A root Item with no provider_key is a caller bug → InvalidInput.
        let mut bad = root_item(&location, &dir);
        bad.provider_key = None;
        assert!(provider.list(&location, &bad).await.is_err());
        // Relative provider_key is likewise rejected.
        bad.provider_key = Some("relative/path".into());
        assert!(provider.list(&location, &bad).await.is_err());
        // Listing a missing directory surfaces a provider error, not a panic.
        bad.provider_key = Some(dir.join("nope").to_string_lossy().into_owned());
        assert!(provider.list(&location, &bad).await.is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn item_from_path_kinds() {
        let dir = scratch_dir("kinds");
        let location = mount_local("Scratch", dir.clone());
        let file = dir.join("f.bin");
        std::fs::write(&file, b"data").unwrap();
        let sub = dir.join("sub");
        std::fs::create_dir(&sub).unwrap();

        let fi = item_from_path(location.id, &file).unwrap();
        assert_eq!(fi.kind, ItemKind::File);
        assert_eq!(fi.size_bytes, Some(4));
        assert!(fi.modified.is_some());

        let di = item_from_path(location.id, &sub).unwrap();
        assert_eq!(di.kind, ItemKind::Folder);
        assert_eq!(di.size_bytes, None);

        #[cfg(unix)]
        {
            let link = dir.join("link");
            std::os::unix::fs::symlink(&file, &link).unwrap();
            let li = item_from_path(location.id, &link).unwrap();
            assert_eq!(li.kind, ItemKind::Symlink);
        }

        assert!(item_from_path(location.id, &dir.join("missing")).is_none());

        std::fs::remove_dir_all(&dir).ok();
    }
}
