//! Phase 6 — WASM plugin host wiring (spec §§17–18).
//!
//! The runtime lives in `ufm_plugins::runtime` (wasmi, fuel + memory
//! limited, no WASI). This module owns the shell side: where plugins
//! live on disk, enable/disable state, the seeded example, the item
//! metadata a plugin may read, the invocation audit line, and the
//! confirm path that turns a proposal into a journaled job.
//!
//! Trust shape (mirrors the Phase 3 MCP model): plugins run locked
//! down (`SandboxPolicy::locked_down` — no grants UI raises privileges
//! in v1); a plugin may only *propose* a `create_text_file`; the
//! proposal becomes a job only when the user confirms it in the UI,
//! and the confirm path re-validates everything (`check_proposal`)
//! because the UI round trip is never trusted.
//!
//! State lives with the app's other sidecars under
//! `~/.local/share/fable-files/` (the `fable_data_dir` root):
//! `plugins/` (drop-in folders), `plugins-settings.json`,
//! `plugin-log.jsonl` (append-only invocation record).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::State;
use ufm_core::{ItemId, LocationId};
use ufm_plugins::runtime::{
    check_proposal, discover_plugins, invoke_plugin, DiscoveredPlugin,
};
use ufm_plugins::SandboxPolicy;
use ufm_vfs::ItemKind;

use crate::commands::{job_item, start_job, JobStart};
use crate::AppState;

const EXAMPLE_KEY: &str = "metadata-note";
const EXAMPLE_TOML: &str = include_str!("../assets/metadata-note.plugin.toml");
const EXAMPLE_WASM: &[u8] = include_bytes!("../assets/metadata-note.wasm");

// ---------------------------------------------------------------------------
// Paths + settings sidecar.
// ---------------------------------------------------------------------------

fn data_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".local/share/fable-files")
}

fn plugins_root() -> PathBuf {
    data_dir().join("plugins")
}

fn settings_path() -> PathBuf {
    data_dir().join("plugins-settings.json")
}

fn log_path() -> PathBuf {
    data_dir().join("plugin-log.jsonl")
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PluginSettings {
    #[serde(default)]
    disabled: Vec<String>,
    #[serde(default)]
    seeded_example: bool,
}

fn load_settings() -> PluginSettings {
    std::fs::read_to_string(settings_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_settings(settings: &PluginSettings) -> Result<(), String> {
    let dir = data_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let tmp = dir.join("plugins-settings.json.tmp");
    let text = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, settings_path()).map_err(|e| e.to_string())
}

/// Seed the bundled example plugin once, so the feature is explorable
/// (and removable) from first launch. Deleting the folder afterwards
/// is respected — seeding never resurrects it.
fn ensure_seeded(settings: &mut PluginSettings) {
    if settings.seeded_example {
        return;
    }
    settings.seeded_example = true;
    let dir = plugins_root().join(EXAMPLE_KEY);
    if !dir.exists() {
        if std::fs::create_dir_all(&dir).is_ok() {
            let _ = std::fs::write(dir.join("plugin.toml"), EXAMPLE_TOML);
            let _ = std::fs::write(dir.join("main.wasm"), EXAMPLE_WASM);
        }
    }
    let _ = save_settings(settings);
}

fn append_log(record: &serde_json::Value) {
    let line = record.to_string();
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path())
    {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
    }
}

// ---------------------------------------------------------------------------
// Views + commands.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginView {
    pub key: String,
    pub name: String,
    pub author: String,
    pub version: String,
    pub description: String,
    pub capabilities: Vec<String>,
    pub permissions: Vec<String>,
    pub enabled: bool,
    pub valid: bool,
    pub error: Option<String>,
    /// True when this build can actually dispatch the plugin
    /// (v1 wires `action.context`; other capabilities are declared
    /// and displayed, not dispatched).
    pub wired: bool,
}

fn view_of(p: &DiscoveredPlugin, settings: &PluginSettings) -> PluginView {
    let enabled = !settings.disabled.iter().any(|k| k == &p.key);
    match &p.manifest {
        Some(m) => PluginView {
            key: p.key.clone(),
            name: m.name.clone(),
            author: m.author.clone(),
            version: m.version.clone(),
            description: m.description.clone(),
            capabilities: m.capabilities.clone(),
            permissions: m.permissions.clone(),
            enabled,
            valid: true,
            error: None,
            wired: m.claims(ufm_plugins::runtime::CAPABILITY_ACTION_CONTEXT),
        },
        None => PluginView {
            key: p.key.clone(),
            name: p.key.clone(),
            author: String::new(),
            version: String::new(),
            description: String::new(),
            capabilities: Vec::new(),
            permissions: Vec::new(),
            enabled,
            valid: false,
            error: p.error.clone(),
            wired: false,
        },
    }
}

/// List every plugin folder in the plugins directory, valid or not —
/// dropping a folder in is the install (rescan happens on every call,
/// so edits and new drops apply without a restart).
#[tauri::command]
pub fn list_plugins() -> Result<Vec<PluginView>, String> {
    let mut settings = load_settings();
    ensure_seeded(&mut settings);
    let found = discover_plugins(&plugins_root());
    Ok(found.iter().map(|p| view_of(p, &settings)).collect())
}

#[tauri::command]
pub fn set_plugin_enabled(key: String, enabled: bool) -> Result<(), String> {
    let mut settings = load_settings();
    settings.disabled.retain(|k| k != &key);
    if !enabled {
        settings.disabled.push(key);
    }
    save_settings(&settings)
}

/// Reveal the plugins folder in the file manager (for dropping in a
/// new plugin by hand).
#[tauri::command]
pub fn plugins_folder() -> Result<String, String> {
    let root = plugins_root();
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    Ok(root.to_string_lossy().into_owned())
}

// ---------------------------------------------------------------------------
// Invocation.
// ---------------------------------------------------------------------------

/// The metadata a plugin may see for the item it was invoked on:
/// filesystem facts + the catalog's labels/star state for the path.
/// This JSON is also what `fable.read_item` answers with (scope is
/// enforced in the runtime).
fn item_json(state: &AppState, path: &Path) -> String {
    let md = std::fs::metadata(path).ok();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let modified = md.as_ref().and_then(|m| m.modified().ok()).map(|t| {
        chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339()
    });
    let (labels, starred) = {
        let catalog = state.catalog.lock().unwrap();
        let key = path.to_string_lossy();
        (
            catalog.labels_for_path(&key).unwrap_or_default(),
            catalog.is_starred_path(&key).unwrap_or(false),
        )
    };
    serde_json::json!({
        "path": path.to_string_lossy(),
        "name": name,
        "kind": if md.as_ref().map(|m| m.is_dir()).unwrap_or(false) { "folder" } else { "file" },
        "size_bytes": md.as_ref().map(|m| m.len()),
        "modified": modified,
        "labels": labels,
        "starred": starred,
    })
    .to_string()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposalView {
    pub plugin_key: String,
    pub plugin_name: String,
    pub name: String,
    pub contents: String,
    /// Where the file will be written if confirmed (the item itself
    /// when it is a folder, else its parent folder).
    pub target_dir: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRunView {
    pub logs: Vec<String>,
    pub proposals: Vec<ProposalView>,
}

#[tauri::command]
pub async fn run_plugin_action(
    state: State<'_, AppState>,
    key: String,
    path: String,
) -> Result<PluginRunView, String> {
    let settings = load_settings();
    if settings.disabled.iter().any(|k| k == &key) {
        return Err("That plugin is disabled.".into());
    }
    let found = discover_plugins(&plugins_root());
    let plugin = found
        .iter()
        .find(|p| p.key == key)
        .ok_or_else(|| "That plugin is no longer in the plugins folder.".to_string())?;
    let manifest = plugin
        .manifest
        .clone()
        .ok_or_else(|| plugin.error.clone().unwrap_or_else(|| "Invalid plugin.".into()))?;
    let dir = plugin.dir.clone();
    let item_path = PathBuf::from(&path);
    if !item_path.exists() {
        return Err("That item no longer exists.".into());
    }
    let json = item_json(&state, &item_path);
    let target_dir = if item_path.is_dir() {
        item_path.clone()
    } else {
        item_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("/"))
    };
    let plugin_name = manifest.name.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        invoke_plugin(
            &dir,
            &manifest,
            &SandboxPolicy::locked_down(),
            &path,
            &json,
            "action",
            "context",
        )
    })
    .await
    .map_err(|e| format!("plugin task failed: {e}"))?
    .map_err(|e| e.to_string())?;

    append_log(&serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "plugin": key,
        "path": item_path.to_string_lossy(),
        "logs": outcome.logs.len(),
        "proposals": outcome.proposals.len(),
    }));
    Ok(PluginRunView {
        logs: outcome.logs,
        proposals: outcome
            .proposals
            .into_iter()
            .map(|p| ProposalView {
                plugin_key: key.clone(),
                plugin_name: plugin_name.clone(),
                name: p.name,
                contents: p.contents,
                target_dir: target_dir.to_string_lossy().into_owned(),
            })
            .collect(),
    })
}

/// Confirm a proposal: it becomes a journaled WriteFile job. The
/// name/contents are re-validated here — the values round-tripped
/// through the UI and are not trusted.
#[tauri::command]
pub async fn confirm_plugin_proposal(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    plugin_key: String,
    plugin_name: String,
    target_dir: String,
    name: String,
    contents: String,
) -> Result<JobStart, String> {
    check_proposal(&name, &contents).map_err(|e| e.to_string())?;
    let dir = PathBuf::from(&target_dir);
    if !dir.is_dir() {
        return Err("The destination folder no longer exists.".into());
    }
    if dir.join(&name).exists() {
        return Err(format!("\"{name}\" already exists in that folder."));
    }
    let mut job = ufm_jobs::Job::new(
        format!("{plugin_name}: write {name}"),
        ufm_jobs::JobPreview {
            files_affected: 1,
            bytes_to_move: contents.len() as u64,
            conflicts: Vec::new(),
        },
    );
    job.add_step(ufm_jobs::Step {
        id: "s0001".into(),
        label: format!("write {name}"),
        op: ufm_jobs::StepOp::WriteFile {
            parent: job_item(ItemId::new(), LocationId::default(), &dir, ItemKind::Folder, None),
            name,
            contents,
        },
        depends_on: Vec::new(),
        state: ufm_jobs::StepState::Pending,
        compensation: None,
        outcome: false,
        outcome_path: None,
    });
    append_log(&serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "plugin": plugin_key,
        "confirmed": true,
        "job": job.id.to_string(),
    }));
    start_job(app, &state, job).map_err(|e| e.message)
}
