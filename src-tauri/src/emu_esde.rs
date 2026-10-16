//! The ES-DE door (SPEC-AMENDMENT §1/§6, v2).
//!
//! ES-DE is a *companion*: Fable offers it as an installed
//! front-end that reads the same unified view (~/ROMs — which is
//! ES-DE's own default ROM root, so usually zero path
//! configuration) and Fable's canonical media tree (its
//! MediaDirectory setting). Fable never reads ES-DE's gamelists
//! or play statistics back — those are ES-DE's business, not a
//! truth source (research directive).
//!
//! Install channel: ES-DE's own GitLab package registry. The
//! packages API names every release's files WITH the sha256 the
//! publisher recorded for each, so the AppImage lands in
//! ~/Applications checksum-verified against the publisher's own
//! hash — never a pinned file id (those rotate per release), the
//! API is the resolver.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use tauri::{Emitter, Manager};

use crate::commands::{Cmd, CommandError};
use crate::emu;
use crate::AppState;
use ufm_core::UfmError;

/// The managed file name in ~/Applications (the publisher's own
/// stable asset name).
pub const ESDE_FILE_NAME: &str = "ES-DE_x64.AppImage";
const GITLAB_PROJECT: &str = "es-de%2Femulationstation-de";
const GITLAB_PROJECT_PATH: &str = "es-de/emulationstation-de";
const PACKAGE_NAME: &str = "ES-DE_Stable";
const ASSET_NAME: &str = "ES-DE_x64.AppImage";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EsDeStateWire {
    pub installed: bool,
    /// The version Fable installed (from its own record), when
    /// the managed AppImage is the one sitting there.
    pub version: Option<String>,
    /// Where it lives, home-relative or as the PATH name.
    pub path: String,
}

pub fn esde_appimage_path() -> PathBuf {
    emu::applications_dir().join(ESDE_FILE_NAME)
}

/// How ES-DE is reachable here: the managed AppImage Fable
/// installed, else an `es-de` on PATH (his own install — the
/// door drives it just the same).
pub fn esde_command() -> Option<Vec<String>> {
    if let Some(p) = emu::managed_appimage(ESDE_FILE_NAME) {
        return Some(vec![p.to_string_lossy().into_owned()]);
    }
    if crate::deps::tool_present("es-de") {
        return Some(vec!["es-de".to_string()]);
    }
    None
}

pub fn esde_state(base: &Path) -> EsDeStateWire {
    let settings = emu::load_settings(base);
    match esde_command() {
        Some(argv) => {
            let managed = emu::managed_appimage(ESDE_FILE_NAME).is_some();
            EsDeStateWire {
                installed: true,
                version: if managed && !settings.esde_version.is_empty() {
                    Some(settings.esde_version.clone())
                } else {
                    None
                },
                path: emu::home_relative(Path::new(&argv[0])),
            }
        }
        None => EsDeStateWire {
            installed: false,
            version: None,
            path: "~/Applications/ES-DE_x64.AppImage".to_string(),
        },
    }
}

/// A dotted version key ("3.5.0" → (3, 5, 0, 0)).
fn version_key(v: &str) -> (u64, u64, u64, u64) {
    emu::appimage_version_key(&format!("v{v}"))
}

/// Resolve the current official download from ES-DE's own
/// package registry: (download URL, published sha256, version).
pub fn resolve_esde() -> Result<(String, String, String), String> {
    let api = format!(
        "https://gitlab.com/api/v4/projects/{GITLAB_PROJECT}/packages?per_page=30&order_by=created_at&sort=desc"
    );
    let body = ureq::get(&api)
        .call()
        .map_err(|e| format!("Fable couldn't check ES-DE's releases: {e}"))?
        .into_string()
        .map_err(|e| format!("ES-DE's release list couldn't be read: {e}"))?;
    let packages: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("ES-DE's release list was unreadable: {e}"))?;
    let mut best: Option<((u64, u64, u64, u64), u64, String)> = None;
    for pkg in packages.as_array().cloned().unwrap_or_default() {
        if pkg["name"].as_str() != Some(PACKAGE_NAME) {
            continue;
        }
        let version = pkg["version"].as_str().unwrap_or_default().to_string();
        let Some(id) = pkg["id"].as_u64() else { continue };
        let key = version_key(&version);
        if best.as_ref().map(|b| key > b.0).unwrap_or(true) {
            best = Some((key, id, version));
        }
    }
    let (_, pkg_id, version) =
        best.ok_or_else(|| "ES-DE's release list didn't name a stable release".to_string())?;
    let files_api = format!(
        "https://gitlab.com/api/v4/projects/{GITLAB_PROJECT}/packages/{pkg_id}/package_files"
    );
    let body = ureq::get(&files_api)
        .call()
        .map_err(|e| format!("Fable couldn't read ES-DE's release files: {e}"))?
        .into_string()
        .map_err(|e| format!("ES-DE's release files couldn't be read: {e}"))?;
    let files: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("ES-DE's release files were unreadable: {e}"))?;
    for f in files.as_array().cloned().unwrap_or_default() {
        if f["file_name"].as_str() == Some(ASSET_NAME) {
            let Some(file_id) = f["id"].as_u64() else { continue };
            let sha = f["file_sha256"].as_str().unwrap_or_default().to_string();
            if sha.is_empty() {
                return Err("ES-DE's release didn't publish a checksum for the download".into());
            }
            let url = format!(
                "https://gitlab.com/{GITLAB_PROJECT_PATH}/-/package_files/{file_id}/download"
            );
            return Ok((url, sha, version));
        }
    }
    Err("ES-DE's latest release didn't include the Linux AppImage".into())
}

/// Install ES-DE through the door's own path (resolve → verify →
/// managed file → runnable proof), and record what was
/// installed in the emu settings.
pub fn install_esde(base: &Path, progress: &emu::Progress) -> Result<EsDeStateWire, String> {
    progress("Checking for the latest ES-DE…", None);
    let (url, sha, version) = resolve_esde()?;
    emu::install_appimage(&url, Some(&sha), ESDE_FILE_NAME, "ES-DE", progress)?;
    let mut settings = emu::load_settings(base);
    settings.esde_version = version;
    settings.esde_sha256 = sha;
    emu::save_settings(base, &settings)?;
    Ok(esde_state(base))
}

/// ES-DE's application data home (it creates this itself on
/// first startup).
pub fn esde_home() -> PathBuf {
    emu::home_dir().join("ES-DE")
}

pub fn esde_settings_path() -> PathBuf {
    esde_home().join("settings").join("es_settings.xml")
}

/// Set one `<string name="…" value="…">` entry in ES-DE's
/// settings text: replace the existing entry's value when the
/// name is already there; insert before the document's last
/// closing tag otherwise. Pure string surgery — ES-DE owns every
/// other byte of the file.
fn set_xml_string(text: &str, name: &str, value: &str) -> String {
    let escaped = value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    let entry = format!("<string name=\"{name}\" value=\"{escaped}\" />");
    let needle = format!("name=\"{name}\"");
    if let Some(at) = text.find(&needle) {
        // The entry element runs from the preceding '<' to its '/>'.
        let start = text[..at].rfind('<').unwrap_or(at);
        if let Some(end_rel) = text[at..].find("/>") {
            let end = at + end_rel + 2;
            let mut out = String::with_capacity(text.len() + entry.len());
            out.push_str(&text[..start]);
            out.push_str(&entry);
            out.push_str(&text[end..]);
            return out;
        }
    }
    // Insert before the final closing tag, else append.
    if let Some(close) = text.rfind("</") {
        let mut out = String::with_capacity(text.len() + entry.len() + 1);
        out.push_str(&text[..close]);
        out.push_str(&entry);
        out.push('\n');
        out.push_str(&text[close..]);
        return out;
    }
    format!("{text}{entry}\n")
}

/// Point ES-DE at Fable's two trees, in ES-DE's own settings
/// file: MediaDirectory always (Fable's canonical media tree is
/// the door's promise); ROMDirectory only when Fable's view
/// lives somewhere other than ES-DE's default ~/ROMs (there the
/// truthful pointer is no pointer at all). Returns false when
/// ES-DE hasn't generated its settings file yet — the caller
/// runs its first-start initialization first.
pub fn write_esde_settings(view_root: &Path, media_root: &Path) -> Result<bool, String> {
    let file = esde_settings_path();
    if !file.is_file() {
        return Ok(false);
    }
    let text = std::fs::read_to_string(&file)
        .map_err(|e| format!("couldn't read ES-DE's settings: {e}"))?;
    let mut text = set_xml_string(&text, "MediaDirectory", &media_root.to_string_lossy());
    if view_root != emu::default_view_root() {
        text = set_xml_string(&text, "ROMDirectory", &view_root.to_string_lossy());
    }
    let tmp = file.with_extension("xml.part");
    std::fs::write(&tmp, text).map_err(|e| format!("couldn't write ES-DE's settings: {e}"))?;
    std::fs::rename(&tmp, &file).map_err(|e| format!("couldn't save ES-DE's settings: {e}"))?;
    Ok(true)
}

/// Spawn ES-DE (no liveness verdict — staying open is its
/// normal state; a dead-on-arrival spawn still errors here).
/// Returns the child so the first-run initialization can stop
/// exactly the instance it started, by handle.
fn spawn_esde(argv: &[String]) -> Result<std::process::Child, String> {
    use std::os::unix::process::CommandExt;
    std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("couldn't start ES-DE: {e}"))
}

/// Open ES-DE, companion-style: installed first (through the
/// install path when it isn't), configured for Fable's trees,
/// then launched. First-ever open runs ES-DE once briefly so IT
/// generates its own settings/home (the file is ES-DE-shaped
/// because ES-DE wrote it), patches the two entries, and only
/// then opens for real.
pub fn open_esde(state: &AppState, progress: &emu::Progress) -> Result<(), String> {
    if esde_command().is_none() {
        install_esde(&state.data_base, progress)?;
    }
    let argv = esde_command()
        .ok_or_else(|| "ES-DE's install finished, but ES-DE still isn't on this system — nothing else is affected, and you can try again from the same place.".to_string())?;
    let settings = emu::load_settings(&state.data_base);
    let view_root = settings.view_root_path();
    let media_root = crate::rommedia::load_settings(&state.data_base).root_path();
    if !write_esde_settings(&view_root, &media_root)? {
        // ES-DE has never run: let it initialize its own home.
        let mut init = spawn_esde(&argv)?;
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !esde_settings_path().is_file() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(400));
        }
        // The initialization instance has done its job; the
        // configured open below replaces it. Killed by handle —
        // never a pattern kill that could meet another process.
        let _ = init.kill();
        let _ = init.wait();
        let _ = write_esde_settings(&view_root, &media_root)?;
        spawn_esde(&argv)?;
        return Ok(());
    }
    spawn_esde(&argv).map(|_| ())
}

// ---------------------------------------------------------------------------
// Commands.
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn emu_esde_state(app: tauri::AppHandle) -> Cmd<EsDeStateWire> {
    tauri::async_runtime::spawn_blocking(move || {
        let guard = app.state::<AppState>();
        let state: &AppState = &guard;
        Ok(esde_state(&state.data_base))
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("es-de state task failed: {e}"))))?
}

#[tauri::command]
pub async fn emu_esde_install(app: tauri::AppHandle) -> Cmd<EsDeStateWire> {
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let guard = app.state::<AppState>();
        let state: &AppState = &guard;
        let progress = |label: &str, pct: Option<u32>| {
            let _ = app2.emit(
                "emu-install-progress",
                serde_json::json!({ "label": label, "pct": pct }),
            );
        };
        install_esde(&state.data_base, &progress)
            .map_err(|e| CommandError::from(UfmError::Internal(e)))
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("es-de install task failed: {e}"))))?
}

#[tauri::command]
pub async fn emu_esde_open(app: tauri::AppHandle) -> Cmd<()> {
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let guard = app.state::<AppState>();
        let state: &AppState = &guard;
        let progress = |label: &str, pct: Option<u32>| {
            let _ = app2.emit(
                "emu-install-progress",
                serde_json::json!({ "label": label, "pct": pct }),
            );
        };
        open_esde(state, &progress).map_err(|e| CommandError::from(UfmError::Internal(e)))
    })
    .await
    .map_err(|e| CommandError::from(UfmError::Internal(format!("es-de open task failed: {e}"))))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_string_surgery_replaces_and_inserts() {
        let doc = "<?xml version=\"1.0\"?>\n<root>\n<string name=\"MediaDirectory\" value=\"/old\" />\n<string name=\"Other\" value=\"x\" />\n</root>\n";
        let out = set_xml_string(doc, "MediaDirectory", "/home/richard/Fable Media");
        assert!(out.contains("name=\"MediaDirectory\" value=\"/home/richard/Fable Media\""), "{out}");
        assert!(out.contains("name=\"Other\" value=\"x\""), "other entries untouched");
        assert_eq!(out.matches("MediaDirectory").count(), 1, "no duplicates");
        let out2 = set_xml_string(&out, "ROMDirectory", "/media/Desk SSD/ROMs");
        assert!(out2.contains("name=\"ROMDirectory\" value=\"/media/Desk SSD/ROMs\""));
        assert!(out2.trim_end().ends_with("</root>"), "inserted inside the root");
    }

    #[test]
    fn version_keys_order_releases() {
        assert!(version_key("3.5.0") > version_key("3.4.1"));
        assert!(version_key("3.10.0") > version_key("3.9.9"));
    }
}
