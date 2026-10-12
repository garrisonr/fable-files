//! Typed command surface — the ONLY bridge between the web shell and the
//! Rust core. No broad filesystem exposure reaches web content (spec §25).
//!
//! Every command takes/returns serializable types from the domain crates.
//! Commands are thin: validate, delegate to the core, emit a DomainEvent.

use serde::{Deserialize, Serialize};
use tauri::State;
use ufm_core::{CollectionId, ItemId, JobId, LocationId, ThemeId, UfmError};
use ufm_vfs::Item;

use crate::AppState;

// ---------------------------------------------------------------------------
// Error rendering — one shape for the whole UI.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct CommandError {
    pub code: String,
    pub message: String,
}

impl From<UfmError> for CommandError {
    fn from(e: UfmError) -> Self {
        let code = match &e {
            UfmError::NotFound(_) => "not_found",
            UfmError::PermissionDenied(_) => "permission_denied",
            UfmError::Conflict(_) => "conflict",
            UfmError::Provider { .. } => "provider",
            UfmError::JobFailed { .. } => "job_failed",
            UfmError::Theme(_) => "theme",
            UfmError::Plugin(_) => "plugin",
            UfmError::Mcp(_) => "mcp",
            UfmError::Database(_) => "database",
            UfmError::InvalidInput(_) => "invalid_input",
            UfmError::Internal(_) => "internal",
        };
        Self {
            code: code.into(),
            message: e.to_string(),
        }
    }
}

pub type Cmd<T> = std::result::Result<T, CommandError>;

// ---------------------------------------------------------------------------
// Locations & browsing
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct LocationSummary {
    pub id: LocationId,
    pub provider: String,
    pub display_name: String,
}

#[tauri::command]
pub async fn list_locations(_state: State<'_, AppState>) -> Cmd<Vec<LocationSummary>> {
    // TODO: read from the catalog; seed with mounted locals on first run.
    Ok(vec![])
}

#[tauri::command]
pub async fn list_dir(
    _state: State<'_, AppState>,
    _location: LocationId,
    _dir: ItemId,
) -> Cmd<Vec<Item>> {
    // TODO: delegate to the VFS provider behind the location contract.
    Ok(vec![])
}

// ---------------------------------------------------------------------------
// Jobs — the transfer center
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CopyRequest {
    pub sources: Vec<ItemId>,
    pub dst_parent: ItemId,
}

#[tauri::command]
pub async fn start_copy(
    _state: State<'_, AppState>,
    _req: CopyRequest,
) -> Cmd<JobId> {
    // TODO: build the DAG (verify-then-skip steps), journal it, queue it.
    Err(CommandError {
        code: "not_implemented".into(),
        message: "job engine wiring comes with the first feature milestone".into(),
    })
}

#[tauri::command]
pub async fn pause_job(_state: State<'_, AppState>, _job: JobId) -> Cmd<()> {
    Ok(())
}

#[tauri::command]
pub async fn resume_job(_state: State<'_, AppState>, _job: JobId) -> Cmd<()> {
    Ok(())
}

#[tauri::command]
pub async fn cancel_job(_state: State<'_, AppState>, _job: JobId) -> Cmd<()> {
    Ok(())
}

// ---------------------------------------------------------------------------
// Search — one language everywhere
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn search(
    _state: State<'_, AppState>,
    _query_text: String,
    _scope: Option<LocationId>,
    _limit: Option<u32>,
) -> Cmd<Vec<Item>> {
    // TODO: ufm_search::parse → catalog FTS → ranked items.
    Ok(vec![])
}

// ---------------------------------------------------------------------------
// Themes — P0. The shell renders ONLY from tokens.
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn list_themes(_state: State<'_, AppState>) -> Cmd<Vec<ThemeId>> {
    Ok(vec![])
}

#[tauri::command]
pub async fn apply_theme(_state: State<'_, AppState>, _theme: ThemeId) -> Cmd<String> {
    // Returns the theme's CSS custom properties for the shell to install.
    // TODO: resolve from the theme service; emit DomainEvent::ThemeChanged.
    Ok(String::new())
}

#[tauri::command]
pub async fn theme_contrast_report(
    _state: State<'_, AppState>,
    _theme: ThemeId,
) -> Cmd<Vec<ufm_themes::ContrastIssue>> {
    Ok(vec![])
}

// ---------------------------------------------------------------------------
// Organization — tags, stars, collections
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn tag_item(
    _state: State<'_, AppState>,
    _item: ItemId,
    _label: String,
) -> Cmd<()> {
    Ok(())
}

#[tauri::command]
pub async fn toggle_star(_state: State<'_, AppState>, _item: ItemId) -> Cmd<bool> {
    Ok(false)
}

#[tauri::command]
pub async fn list_collections(_state: State<'_, AppState>) -> Cmd<Vec<CollectionId>> {
    Ok(vec![])
}

// ---------------------------------------------------------------------------
// Preview — spacebar Quick Look
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn preview(
    _state: State<'_, AppState>,
    _item: ItemId,
) -> Cmd<ufm_preview::PreviewKind> {
    // TODO: registry.best_kind → render with byte budget → stream handle.
    Err(CommandError {
        code: "not_implemented".into(),
        message: "preview providers land with the first feature milestone".into(),
    })
}
