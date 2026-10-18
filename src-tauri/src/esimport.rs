//! The EmulationStation import door (Hyperbase #5, 2026-10-17).
//!
//! Two surfaces, one engine:
//! - The add-location census reports ES data when it sees it
//!   (`census_location` carries `EsDetection` — metadata only, the
//!   sheet names the import; nothing is imported by looking).
//! - `es_import` does the work: for every registered location
//!   root (or one given root), detect the ES layout and hand each
//!   system to the catalog's import, which attaches gamelist
//!   metadata to the indexed items by path under the precedence
//!   contract in ufm-catalog (DAT truth first, his edits above
//!   all, merges never replacements).
//!
//! The import is one click and re-runnable: a second run over the
//! same lists reconciles to refreshes and no fills (the catalog
//! tests pin that), so there is no separate "already imported"
//! state to keep honest.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{Emitter, Manager};

use crate::commands::{Cmd, CommandError};
use crate::AppState;
use ufm_core::UfmError;

static ES_IMPORT_RUNNING: AtomicBool = AtomicBool::new(false);

/// Import progress, emitted per system from the blocking task —
/// Rust-side events, never a webview timer (Forge law).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct EsImportProgress {
    system: String,
    done: u32,
    total: u32,
}

fn run_es_import(
    app: &tauri::AppHandle,
    state: &AppState,
    root: Option<String>,
) -> Cmd<ufm_catalog::EsImportReport> {
    let roots: Vec<PathBuf> = match &root {
        Some(p) => vec![PathBuf::from(p)],
        None => state
            .local_roots
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect(),
    };
    let mut report = ufm_catalog::EsImportReport {
        roots_searched: roots.len() as u64,
        ..Default::default()
    };
    // Detect everywhere first (metadata only), so progress totals
    // are true before the first system starts.
    let mut systems: Vec<ufm_media::esgamelist::EsSystemDir> = Vec::new();
    for r in &roots {
        if let Some(det) = ufm_media::esgamelist::detect_es_root(r) {
            report.roots_with_es += 1;
            systems.extend(det.systems);
        }
    }
    systems.sort_by(|a, b| a.dir.cmp(&b.dir));
    let total = systems.len() as u32;
    for (i, sys) in systems.iter().enumerate() {
        let _ = app.emit(
            "es-import-progress",
            EsImportProgress {
                system: sys.es_name.clone(),
                done: i as u32,
                total,
            },
        );
        // The catalog lock is per-system: the rest of the app
        // breathes between systems on a long import.
        let outcome = {
            let catalog = state.catalog.lock().unwrap();
            catalog.import_es_system(sys)
        };
        match outcome {
            Ok(rep) => {
                report.totals.add(&rep.counts);
                report.systems.push(rep);
            }
            Err(e) => {
                // One system's failure is that system's row in the
                // report — never a silent hole, never a dead run.
                report.systems.push(ufm_catalog::EsSystemReport {
                    es_system: sys.es_name.clone(),
                    platform: ufm_media::esgamelist::fable_platform(&sys.es_name),
                    gamelist_path: sys.gamelist_path.to_string_lossy().into_owned(),
                    parse_error: Some(format!("import failed: {e}")),
                    counts: ufm_catalog::EsCounters::default(),
                });
            }
        }
    }
    let _ = app.emit(
        "es-import-progress",
        EsImportProgress {
            system: String::new(),
            done: total,
            total,
        },
    );
    Ok(report)
}

/// Import EmulationStation metadata into the catalog. With no
/// `root`, every registered location root is searched for ES
/// layouts; with one, only that root. Single-flight: a second
/// call while one runs reports `alreadyRunning` and changes
/// nothing.
#[tauri::command]
pub async fn es_import(
    app: tauri::AppHandle,
    root: Option<String>,
) -> Cmd<ufm_catalog::EsImportReport> {
    if ES_IMPORT_RUNNING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Ok(ufm_catalog::EsImportReport {
            already_running: true,
            ..Default::default()
        });
    }
    let app2 = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let guard = app2.state::<AppState>();
        let state: &AppState = &guard;
        run_es_import(&app2, state, root)
    })
    .await;
    ES_IMPORT_RUNNING.store(false, Ordering::SeqCst);
    match result {
        Ok(r) => r,
        Err(e) => Err(CommandError::from(UfmError::Internal(format!(
            "es import task failed: {e}"
        )))),
    }
}

/// The stored ES artwork for one game (ES artwork display — Peep
/// Show, 2026-10-18): the display half of the Hyperbase #5 import.
/// `es_metadata` has held marquee / fanart / manual paths since the
/// import shipped, with no surface reading them; the Games Home
/// inspector reads them through here. Paths only — the webview
/// loads the files itself, and a game with no ES row, or a gamelist
/// that never carried a ref, comes back with None fields, never an
/// error: absence renders as the inspector always has.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EsArtSheet {
    pub marquee: Option<String>,
    pub fanart: Option<String>,
    pub manual: Option<String>,
}

fn art_sheet_from_row(row: &ufm_catalog::EsMetadataRow) -> EsArtSheet {
    EsArtSheet {
        marquee: row.marquee_path.clone(),
        fanart: row.fanart_path.clone(),
        manual: row.manual_path.clone(),
    }
}

#[tauri::command]
pub fn es_art_sheet(
    state: tauri::State<'_, AppState>,
    provider_key: String,
) -> Cmd<Option<EsArtSheet>> {
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    let row = catalog
        .es_metadata_for(&provider_key)
        .map_err(CommandError::from)?;
    Ok(row.map(|r| art_sheet_from_row(&r)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn art_sheet_carries_all_three_refs() {
        let row = ufm_catalog::EsMetadataRow {
            marquee_path: Some("/roms/psx/images/Ape Escape-marquee.png".into()),
            fanart_path: Some("/roms/psx/images/Ape Escape-fanart.jpg".into()),
            manual_path: Some("/roms/psx/manuals/Ape Escape.pdf".into()),
            ..Default::default()
        };
        let sheet = art_sheet_from_row(&row);
        assert_eq!(
            sheet.marquee.as_deref(),
            Some("/roms/psx/images/Ape Escape-marquee.png")
        );
        assert_eq!(
            sheet.fanart.as_deref(),
            Some("/roms/psx/images/Ape Escape-fanart.jpg")
        );
        assert_eq!(
            sheet.manual.as_deref(),
            Some("/roms/psx/manuals/Ape Escape.pdf")
        );
    }

    #[test]
    fn art_sheet_keeps_absence_absent() {
        // A gamelist that carried only a marquee: the other two
        // stay None — the UI must never see a placeholder path.
        let row = ufm_catalog::EsMetadataRow {
            marquee_path: Some("/roms/gc/images/1080-marquee.png".into()),
            ..Default::default()
        };
        let sheet = art_sheet_from_row(&row);
        assert!(sheet.marquee.is_some());
        assert!(sheet.fanart.is_none());
        assert!(sheet.manual.is_none());
    }
}
