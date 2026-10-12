//! Fable Files — Tauri v2 application entry.
//!
//! The Rust core owns truth; this shell owns presentation. The web UI gets
//! typed commands and domain events, nothing more.

mod commands;

use std::sync::Mutex;
use tauri::Manager;

/// Shared application state: the catalog, registries, and services.
/// Built once at startup; commands borrow it.
pub struct AppState {
    // TODO: Catalog, PreviewRegistry, PluginHost, McpServer, ThemeService.
    // Wired during the first feature milestone; the command surface above
    // already compiles against this shape.
    _guard: Mutex<()>,
}

impl AppState {
    fn new() -> Self {
        Self {
            _guard: Mutex::new(()),
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter("ufm=debug")
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![
            commands::list_locations,
            commands::list_dir,
            commands::start_copy,
            commands::pause_job,
            commands::resume_job,
            commands::cancel_job,
            commands::search,
            commands::list_themes,
            commands::apply_theme,
            commands::theme_contrast_report,
            commands::tag_item,
            commands::toggle_star,
            commands::list_collections,
            commands::preview,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Fable Files");
}
