//! Per-location index scopes (huge-drive safety gate, 2026-10-16).
//!
//! When a location is added through the census gate, the choices
//! made in that sheet — the folder pick and the ROM-focus flag —
//! are the catalog contract for the location: `start_indexing`
//! walks only what the scope admits. Scopes live in a JSON sidecar
//! beside the other app settings files (the library.rs /
//! rommedia.rs pattern: atomic tmp+rename, defaults on any read
//! failure — a corrupt sidecar must never cost anyone their
//! library; it just means "no scope", i.e. the historic behavior).
//!
//! The map is keyed by root path string, exactly as the root is
//! registered in AppState's browse maps. Root paths are the stable
//! identity here on purpose: runtime locations are session-scoped
//! and re-registered on boot, so an id-keyed store would lose the
//! scope on every restart while the folder pick is a durable
//! decision about a durable path.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// One location's scope, as the census sheet produced it and the
/// sidecar stores it. Mirrors `ufm_search::IndexScope` with the
/// pick as a list (JSON-friendly); conversion is in
/// [`LocationScope::to_index_scope`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocationScope {
    /// ROM focus: index only ROM-identification candidate files
    /// (plus folder rows). Everything else stays browsable live.
    #[serde(default)]
    pub rom_focus: bool,
    /// Folder pick: admitted top-level directory names.
    /// None = the whole tree.
    #[serde(default)]
    pub include_dirs: Option<Vec<String>>,
    /// With a pick in force: files at the root itself admitted?
    #[serde(default = "default_true")]
    pub include_root_files: bool,
}

fn default_true() -> bool {
    true
}

impl LocationScope {
    pub fn to_index_scope(&self) -> ufm_search::IndexScope {
        ufm_search::IndexScope {
            rom_focus: self.rom_focus,
            include_dirs: self
                .include_dirs
                .as_ref()
                .map(|dirs| dirs.iter().cloned().collect()),
            include_root_files: self.include_root_files,
        }
    }
}

pub type ScopeMap = HashMap<String, LocationScope>;

pub fn scopes_path(base: &Path) -> std::path::PathBuf {
    base.join("location-scopes.json")
}

pub fn load_scopes(base: &Path) -> ScopeMap {
    match std::fs::read(scopes_path(base)) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => ScopeMap::new(),
    }
}

fn save_scopes(base: &Path, scopes: &ScopeMap) -> Result<(), String> {
    let path = scopes_path(base);
    let tmp = path.with_extension("json.tmp");
    let data = serde_json::to_vec_pretty(scopes).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, data).map_err(|e| format!("write location scopes: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("save location scopes: {e}"))?;
    Ok(())
}

/// Record (or replace) the scope for a root path.
pub fn save_scope(base: &Path, root: &Path, scope: &LocationScope) -> Result<(), String> {
    let mut scopes = load_scopes(base);
    scopes.insert(root.to_string_lossy().into_owned(), scope.clone());
    save_scopes(base, &scopes)
}

/// The scope in force for a root, if the gate ever wrote one.
pub fn scope_for_root(base: &Path, root: &Path) -> Option<LocationScope> {
    load_scopes(base)
        .get(&root.to_string_lossy().into_owned())
        .cloned()
}

/// Forget a root's scope (its location was removed: the pick was a
/// decision about that registration, and a future re-add starts
/// from the census like any other add).
pub fn forget_scope(base: &Path, root: &Path) {
    let mut scopes = load_scopes(base);
    if scopes.remove(&root.to_string_lossy().into_owned()).is_some() {
        let _ = save_scopes(base, &scopes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ufm-locscope-test-{tag}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn scope_round_trips_and_forgets() {
        let base = scratch("roundtrip");
        let root = Path::new("/media/richard/Hyper base PN ultra");
        assert!(scope_for_root(&base, root).is_none(), "no file, no scope");
        let scope = LocationScope {
            rom_focus: true,
            include_dirs: Some(vec!["A gift to you".to_string()]),
            include_root_files: false,
        };
        save_scope(&base, root, &scope).unwrap();
        let back = scope_for_root(&base, root).unwrap();
        assert_eq!(back, scope);
        let idx = back.to_index_scope();
        assert!(idx.rom_focus);
        assert!(!idx.include_root_files);
        assert_eq!(idx.include_dirs.unwrap().len(), 1);
        forget_scope(&base, root);
        assert!(scope_for_root(&base, root).is_none());
        // A corrupt sidecar reads as "no scopes", never an error.
        std::fs::write(scopes_path(&base), b"{not json").unwrap();
        assert!(scope_for_root(&base, root).is_none());
        std::fs::remove_dir_all(&base).ok();
    }
}
