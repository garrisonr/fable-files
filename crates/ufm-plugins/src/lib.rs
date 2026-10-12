//! ufm-plugins: the hot-loaded extension host.
//!
//! Spec §§17–18: dropping a script into the plugins folder hot-loads new
//! capabilities — search providers, preview providers, context actions,
//! organization rules, cloud providers, themes. The capability broker mediates
//! every call; third-party plugins run sandboxed with declared permissions.
//! A community directory/marketplace model follows in P1.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use ufm_core::{PluginId, Result, UfmError};

// ---------------------------------------------------------------------------
// Manifest — what a plugin declares about itself. No executable code runs
// before the manifest validates.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: PluginId,
    pub name: String,
    pub author: String,
    pub version: String,
    pub description: String,
    /// Capabilities the plugin wants: "search.provider", "preview.provider",
    /// "action.context", "organize.rule", "cloud.provider", "theme.pack" …
    pub capabilities: Vec<String>,
    /// Sandbox permissions it requests: "fs.read", "net.fetch", …
    pub permissions: Vec<String>,
    pub entry: String, // relative path of the executable entry point
}

impl PluginManifest {
    pub fn load_from_dir(dir: &std::path::Path) -> Result<Self> {
        let text = std::fs::read_to_string(dir.join("plugin.toml"))
            .map_err(|e| UfmError::Plugin(format!("cannot read plugin.toml: {e}")))?;
        toml::from_str(&text).map_err(|e| UfmError::Plugin(format!("bad plugin.toml: {e}")))
    }

    /// A plugin may only claim capabilities its manifest declares.
    pub fn claims(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|c| c == capability)
    }
}

// ---------------------------------------------------------------------------
// Sandbox policy — what a plugin is allowed to touch. Enforced by the host
// before any plugin code runs; violations unload the plugin.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SandboxPolicy {
    pub allow_fs_read: Vec<String>,  // path prefixes
    pub allow_fs_write: Vec<String>, // path prefixes
    pub allow_network: bool,
    pub allow_subprocess: bool,
    pub max_memory_mb: u64,
    pub max_cpu_seconds: u64,
}

impl SandboxPolicy {
    /// The default: read-only, no network, no subprocesses. Plugins earn more
    /// through explicit user grants (spec §18).
    pub fn locked_down() -> Self {
        Self {
            max_memory_mb: 256,
            max_cpu_seconds: 30,
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Host — watches the plugins folder, loads/unloads on change, brokers
// capability calls. Hot reload: a changed plugin.toml re-validates and
// swaps without restarting the app.
// ---------------------------------------------------------------------------

pub struct PluginHost {
    plugins: HashMap<PluginId, LoadedPlugin>,
    pub watch_dir: std::path::PathBuf,
}

#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    pub manifest: PluginManifest,
    pub policy: SandboxPolicy,
    pub loaded_at: chrono::DateTime<chrono::Utc>,
}

impl PluginHost {
    pub fn new(watch_dir: std::path::PathBuf) -> Self {
        Self {
            plugins: HashMap::new(),
            watch_dir,
        }
    }

    /// Scan the folder and (re)load every valid plugin. Invalid manifests are
    /// skipped with an error — one bad plugin never takes down the host.
    pub fn rescan(&mut self) -> Vec<UfmError> {
        let mut errors = Vec::new();
        let entries = match std::fs::read_dir(&self.watch_dir) {
            Ok(e) => e,
            Err(err) => {
                errors.push(UfmError::Plugin(format!(
                    "cannot read plugins dir: {err}"
                )));
                return errors;
            }
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() || !dir.join("plugin.toml").exists() {
                continue;
            }
            match PluginManifest::load_from_dir(&dir) {
                Ok(manifest) => {
                    let id = manifest.id;
                    self.plugins.insert(
                        id,
                        LoadedPlugin {
                            manifest,
                            policy: SandboxPolicy::locked_down(),
                            loaded_at: chrono::Utc::now(),
                        },
                    );
                }
                Err(e) => errors.push(e),
            }
        }
        errors
    }

    pub fn get(&self, id: &PluginId) -> Option<&LoadedPlugin> {
        self.plugins.get(id)
    }

    /// All loaded plugins claiming a capability (e.g. every extra search
    /// provider, so "super mario" can also find-and-download via a plugin).
    pub fn claiming(&self, capability: &str) -> Vec<&LoadedPlugin> {
        self.plugins
            .values()
            .filter(|p| p.manifest.claims(capability))
            .collect()
    }

    pub fn unload(&mut self, id: &PluginId) -> bool {
        self.plugins.remove(id).is_some()
    }

    pub fn count(&self) -> usize {
        self.plugins.len()
    }
}
