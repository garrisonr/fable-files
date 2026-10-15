//! Phase 6 — folder templates (spec §22).
//!
//! A template is a real folder tree (folders + small placeholder
//! files) kept in the app's template store under
//! `~/.local/share/fable-files/folder-templates/`, with an
//! `index.json` of display metadata. (The office "new file" templates
//! in `commands::office_template` are a different feature sharing the
//! word; the stores are deliberately separate.)
//!
//! Applying a template is a **journaled job**: a root `Mkdir`, one
//! `Mkdir` per subfolder chained by dependency, and one `Copy` per
//! placeholder file — planned here, executed by the shared engine via
//! `commands::start_job`, visible in the transfer center like any
//! other job. Creating a template from an existing folder copies
//! files up to 1 MiB; larger files are skipped and reported, never
//! silently dropped.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tauri::State;
use ufm_core::{ItemId, LocationId};
use ufm_vfs::ItemKind;

use crate::commands::{job_item, start_job, JobStart};
use crate::AppState;

/// Placeholder files larger than this are not captured into a
/// template (reported as skipped instead).
const MAX_PLACEHOLDER_BYTES: u64 = 1024 * 1024;
/// Walk rails for hostile or accidental huge trees.
const MAX_TEMPLATE_ENTRIES: usize = 10_000;
const MAX_TEMPLATE_DEPTH: usize = 32;

// ---------------------------------------------------------------------------
// Store.
// ---------------------------------------------------------------------------

fn data_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".local/share/fable-files")
}

fn store_root() -> PathBuf {
    data_dir().join("folder-templates")
}

fn index_path() -> PathBuf {
    store_root().join("index.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TemplateMeta {
    id: String,
    name: String,
    #[serde(default)]
    starter: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TemplateIndex {
    #[serde(default)]
    templates: Vec<TemplateMeta>,
    #[serde(default)]
    seeded_starters: bool,
}

fn load_index() -> TemplateIndex {
    std::fs::read_to_string(index_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_index(index: &TemplateIndex) -> Result<(), String> {
    let root = store_root();
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let tmp = root.join("index.json.tmp");
    let text = serde_json::to_string_pretty(index).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, index_path()).map_err(|e| e.to_string())
}

fn slugify(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "template".into()
    } else {
        out
    }
}

fn unique_id(index: &TemplateIndex, base: &str) -> String {
    if !index.templates.iter().any(|t| t.id == base) {
        return base.to_string();
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base}-{n}");
        if !index.templates.iter().any(|t| t.id == candidate) {
            return candidate;
        }
        n += 1;
    }
}

// ---------------------------------------------------------------------------
// Tree walking (shared by capture + apply + stats).
// ---------------------------------------------------------------------------

struct TreeWalk {
    /// Subfolder paths relative to the root, parents before children.
    dirs: Vec<PathBuf>,
    /// (relative path, absolute path, size) for each file.
    files: Vec<(PathBuf, PathBuf, u64)>,
    /// Files skipped for exceeding the capture cap (capture only).
    skipped_large: usize,
}

fn walk_tree(root: &Path, capture_cap: Option<u64>) -> Result<TreeWalk, String> {
    let mut out = TreeWalk {
        dirs: Vec::new(),
        files: Vec::new(),
        skipped_large: 0,
    };
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > MAX_TEMPLATE_DEPTH {
            return Err("That folder tree is too deep to use as a template.".into());
        }
        let entries = std::fs::read_dir(&dir).map_err(|e| e.to_string())?;
        let mut names: Vec<_> = entries.flatten().collect();
        names.sort_by_key(|e| e.file_name());
        for entry in names {
            if out.dirs.len() + out.files.len() >= MAX_TEMPLATE_ENTRIES {
                return Err("That folder tree is too large to use as a template.".into());
            }
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_path_buf();
            let ft = entry.file_type().map_err(|e| e.to_string())?;
            if ft.is_dir() {
                out.dirs.push(rel);
                stack.push((path, depth + 1));
            } else if ft.is_file() {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                if let Some(cap) = capture_cap {
                    if size > cap {
                        out.skipped_large += 1;
                        continue;
                    }
                }
                out.files.push((rel, path, size));
            }
            // Symlinks and other kinds are not template material.
        }
    }
    // Parents before children (depth order), stable for planning.
    out.dirs.sort_by_key(|d| d.components().count());
    Ok(out)
}

fn seed_starters(index: &mut TemplateIndex) {
    if index.seeded_starters {
        return;
    }
    index.seeded_starters = true;
    let starters: [(&str, &[&str], &[(&str, &str)]); 2] = [
        (
            "Project",
            &["docs", "assets", "exports"],
            &[(
                "README.txt",
                "# Project\n\nCreated with Fable Files from the Project template.\n",
            )],
        ),
        (
            "Photo Shoot",
            &["RAW", "Selects", "Exports"],
            &[("notes.txt", "Shoot notes\n\n")],
        ),
    ];
    for (name, dirs, files) in starters {
        let id = unique_id(index, &slugify(name));
        let root = store_root().join(&id);
        let mut ok = std::fs::create_dir_all(&root).is_ok();
        for d in dirs {
            ok &= std::fs::create_dir_all(root.join(d)).is_ok();
        }
        for (f, contents) in files {
            ok &= std::fs::write(root.join(f), contents).is_ok();
        }
        if ok {
            index.templates.push(TemplateMeta {
                id,
                name: name.into(),
                starter: true,
            });
        }
    }
    let _ = save_index(index);
}

// ---------------------------------------------------------------------------
// Views + commands.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateView {
    pub id: String,
    pub name: String,
    pub starter: bool,
    pub folders: usize,
    pub files: usize,
    pub bytes: u64,
}

fn view_of(meta: &TemplateMeta) -> TemplateView {
    let root = store_root().join(&meta.id);
    let (folders, files, bytes) = match walk_tree(&root, None) {
        Ok(w) => (
            w.dirs.len(),
            w.files.len(),
            w.files.iter().map(|f| f.2).sum(),
        ),
        Err(_) => (0, 0, 0),
    };
    TemplateView {
        id: meta.id.clone(),
        name: meta.name.clone(),
        starter: meta.starter,
        folders,
        files,
        bytes,
    }
}

#[tauri::command]
pub fn list_folder_templates() -> Result<Vec<TemplateView>, String> {
    let mut index = load_index();
    seed_starters(&mut index);
    Ok(index.templates.iter().map(view_of).collect())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTemplateResult {
    pub template: TemplateView,
    /// Files in the source folder left out for exceeding the
    /// placeholder size cap (0 when created blank).
    pub skipped_large_files: usize,
}

#[tauri::command]
pub async fn create_folder_template(
    name: String,
    from_path: Option<String>,
) -> Result<CreateTemplateResult, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("Give the template a name.".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let mut index = load_index();
        seed_starters(&mut index);
        let id = unique_id(&index, &slugify(&name));
        let root = store_root().join(&id);
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let mut skipped = 0usize;
        if let Some(src) = from_path {
            let src = PathBuf::from(src);
            if !src.is_dir() {
                return Err("That folder no longer exists.".into());
            }
            let walk = walk_tree(&src, Some(MAX_PLACEHOLDER_BYTES))?;
            skipped = walk.skipped_large;
            for d in &walk.dirs {
                std::fs::create_dir_all(root.join(d)).map_err(|e| e.to_string())?;
            }
            for (rel, abs, _) in &walk.files {
                if let Some(parent) = root.join(rel).parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                std::fs::copy(abs, root.join(rel)).map_err(|e| e.to_string())?;
            }
        }
        let meta = TemplateMeta {
            id: id.clone(),
            name,
            starter: false,
        };
        index.templates.push(meta.clone());
        save_index(&index)?;
        Ok(CreateTemplateResult {
            template: view_of(&meta),
            skipped_large_files: skipped,
        })
    })
    .await
    .map_err(|e| format!("template task failed: {e}"))?
}

#[tauri::command]
pub fn rename_folder_template(id: String, name: String) -> Result<(), String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("Give the template a name.".into());
    }
    let mut index = load_index();
    let meta = index
        .templates
        .iter_mut()
        .find(|t| t.id == id)
        .ok_or("That template no longer exists.")?;
    meta.name = name;
    save_index(&index)
}

#[tauri::command]
pub fn delete_folder_template(id: String) -> Result<(), String> {
    let mut index = load_index();
    let pos = index
        .templates
        .iter()
        .position(|t| t.id == id)
        .ok_or("That template no longer exists.")?;
    index.templates.remove(pos);
    save_index(&index)?;
    let root = store_root().join(&id);
    if root.exists() {
        std::fs::remove_dir_all(&root).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Apply a template inside `parent_path` as a new folder named
/// `name`, planned as a journaled job (Mkdir DAG + Copy steps).
#[tauri::command]
pub async fn apply_folder_template(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    template_id: String,
    parent_path: String,
    name: String,
) -> Result<JobStart, String> {
    let name = name.trim().to_string();
    if name.is_empty() || name.contains('/') || name.contains('\\') || name == "." || name == ".." {
        return Err("That folder name can't be used.".into());
    }
    let index = load_index();
    let meta = index
        .templates
        .iter()
        .find(|t| t.id == template_id)
        .ok_or("That template no longer exists.")?
        .clone();
    let parent = PathBuf::from(&parent_path);
    if !parent.is_dir() {
        return Err("The destination folder no longer exists.".into());
    }
    let dest = parent.join(&name);
    if dest.exists() {
        return Err(format!("\"{name}\" already exists there."));
    }
    let store_dir = store_root().join(&meta.id);
    let walk = walk_tree(&store_dir, None)?;

    let mut job = ufm_jobs::Job::new(
        format!("New folder from template \"{}\"", meta.name),
        ufm_jobs::JobPreview {
            files_affected: (walk.dirs.len() + walk.files.len() + 1) as u64,
            bytes_to_move: walk.files.iter().map(|f| f.2).sum(),
            conflicts: Vec::new(),
        },
    );
    let mut seq = 0u32;
    let mut next_id = || {
        seq += 1;
        format!("s{seq:04}")
    };
    let mut step_for_dir: HashMap<PathBuf, String> = HashMap::new();
    let root_step = next_id();
    job.add_step(ufm_jobs::Step {
        id: root_step.clone(),
        label: format!("create folder {name}"),
        op: ufm_jobs::StepOp::Mkdir {
            parent: job_item(
                ItemId::new(),
                LocationId::default(),
                &parent,
                ItemKind::Folder,
                None,
            ),
            name: name.clone(),
        },
        depends_on: Vec::new(),
        state: ufm_jobs::StepState::Pending,
        compensation: None,
        outcome: false,
        outcome_path: None,
    });
    step_for_dir.insert(PathBuf::new(), root_step);
    // Subfolders, parents first (walk order), each depending on the
    // step that created its parent.
    for rel in &walk.dirs {
        let parent_rel = rel.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let dep = step_for_dir.get(&parent_rel).cloned();
        let leaf = rel
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let parent_abs = if parent_rel.as_os_str().is_empty() {
            dest.clone()
        } else {
            dest.join(&parent_rel)
        };
        let id = next_id();
        job.add_step(ufm_jobs::Step {
            id: id.clone(),
            label: format!("create folder {}", rel.display()),
            op: ufm_jobs::StepOp::Mkdir {
                parent: job_item(
                    ItemId::new(),
                    LocationId::default(),
                    &parent_abs,
                    ItemKind::Folder,
                    None,
                ),
                name: leaf,
            },
            depends_on: dep.into_iter().collect(),
            state: ufm_jobs::StepState::Pending,
            compensation: None,
            outcome: false,
            outcome_path: None,
        });
        step_for_dir.insert(rel.clone(), id);
    }
    // Placeholder files, each depending on its folder's step.
    for (rel, abs, size) in &walk.files {
        let parent_rel = rel.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let dep = step_for_dir.get(&parent_rel).cloned();
        let dst_parent_abs = if parent_rel.as_os_str().is_empty() {
            dest.clone()
        } else {
            dest.join(&parent_rel)
        };
        let leaf = rel
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let id = next_id();
        job.add_step(ufm_jobs::Step {
            id,
            label: format!("copy template file {}", rel.display()),
            op: ufm_jobs::StepOp::Copy {
                src: job_item(
                    ItemId::new(),
                    LocationId::default(),
                    abs,
                    ItemKind::File,
                    Some(*size),
                ),
                dst_parent: job_item(
                    ItemId::new(),
                    LocationId::default(),
                    &dst_parent_abs,
                    ItemKind::Folder,
                    None,
                ),
                new_name: leaf,
            },
            depends_on: dep.into_iter().collect(),
            state: ufm_jobs::StepState::Pending,
            compensation: None,
            outcome: false,
            outcome_path: None,
        });
    }
    start_job(app, &state, job).map_err(|e| e.message)
}
