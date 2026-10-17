//! PC Games Library, Phase 1 (2026-10-17): the Playnite import
//! door and the folder-scan fallback for Games Home.
//!
//! The import reads a Playnite library (LiteDB v4, via the
//! READ-ONLY reader in ufm-media — his database is never opened
//! writable; the importer parses a staged copy and never touches
//! the original again) and writes Fable identities:
//! `media_kind='pc-game'`, one shallow folder item per install,
//! under the volume location the install lives on. Recorded
//! Windows paths are rebased onto the mounted volume (drive
//! letter stripped, matched exactly, then case-insensitively);
//! what cannot be verified on disk is imported as RECORDED —
//! present in the library, state shown truthfully, no fake
//! "installed" claims.
//!
//! Both doors follow the ES import precedent: single-flight,
//! `spawn_blocking`, Rust progress events, per-game catalog
//! transactions under a per-game lock, per-game failures as rows
//! in the report. Re-running either door reconciles; nothing
//! duplicates (precedence: playnite > folder-scan, enforced in
//! the catalog write).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{Emitter, Manager};

use crate::commands::{current_filesystems, volume_uuid, Cmd, CommandError, MountedVolume};
use crate::AppState;
use ufm_core::UfmError;
use ufm_media::playnite::{PlayniteGame, PlayniteLibrary};

static PC_WORK_RUNNING: AtomicBool = AtomicBool::new(false);

const VOLUME_CAPS_JSON: &str = "{\"read\":true,\"write\":true,\"delete\":true,\"watch\":false,\"stream\":true,\"trash\":true,\"extended_attrs\":false}";
const BADGES: &[&str] = &[
    "not-checked",
    "boots",
    "boots-with-notes",
    "wont-boot",
    "windows-only",
];

// ---------------------------------------------------------------------------
// Reports / wire types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PcImportReport {
    pub already_running: bool,
    /// The folder the library was found in (the `library/` dir).
    pub library_path: String,
    /// The root the library was detected under.
    pub root: String,
    pub records_seen: i64,
    pub imported: i64,
    pub refreshed: i64,
    pub upgraded: i64,
    /// Records whose install could not be verified on a mounted
    /// volume — imported as recorded, not present.
    pub recorded_not_present: i64,
    /// Two records, one folder, same game: merged onto one
    /// identity (e.g. "The Evil Within 2" recorded twice).
    pub duplicates_merged: i64,
    pub art_adopted: i64,
    pub art_missing: i64,
    /// Media folders under library/files/ that no game claims.
    pub orphan_media_folders: i64,
    pub mislabel_notes: i64,
    pub skipped_precedence: i64,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PcImportPreview {
    pub found: bool,
    pub library_path: String,
    pub root: String,
    pub records_seen: i64,
    pub would_import_present: i64,
    pub recorded_not_present: i64,
    pub duplicates_merged: i64,
    pub art_available: i64,
    pub art_missing: i64,
    pub orphan_media_folders: i64,
    /// Human lines: mislabels, duplicates, oddities — the sheet
    /// shows them before Dickles confirms.
    pub notes: Vec<String>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PcScanReport {
    pub already_running: bool,
    pub root: String,
    pub folders_seen: i64,
    pub imported: i64,
    pub refreshed: i64,
    pub covered_by_playnite: i64,
    pub no_exe_found: i64,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PcProgress {
    pub phase: String,
    pub done: u32,
    pub total: u32,
    pub title: String,
}

// ---------------------------------------------------------------------------
// Detection + planning (shared by preview and apply)
// ---------------------------------------------------------------------------

struct Detected {
    root: PathBuf,
    library_dir: PathBuf,
    staged_dir: PathBuf,
    lib: PlayniteLibrary,
    volumes: Vec<MountedVolume>,
}

/// Locate the Playnite library under one root: `<root>/Playnite/
/// library`, `<root>/library`, or the root itself when it IS the
/// library dir (it holds games.db). First hit wins.
fn detect_library_dir(root: &Path) -> Option<PathBuf> {
    for cand in [
        root.join("Playnite").join("library"),
        root.join("library"),
        root.to_path_buf(),
    ] {
        if cand.join("games.db").is_file() {
            return Some(cand);
        }
    }
    None
}

/// Stage the library's `.db` files into a fresh temp dir and parse
/// from there. The originals are opened once, read-only, for the
/// copy — iteration never re-reads the source drive (drive
/// discipline), and the reader itself is read-only by law.
fn stage_and_load(library_dir: &Path) -> Result<(PathBuf, PlayniteLibrary), String> {
    let staged = std::env::temp_dir().join(format!(
        "fable-pcgames-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&staged).map_err(|e| format!("staging dir: {e}"))?;
    let mut copied = 0usize;
    let entries = std::fs::read_dir(library_dir)
        .map_err(|e| format!("reading {}: {e}", library_dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map(|x| x == "db").unwrap_or(false) {
            let dest = staged.join(entry.file_name());
            std::fs::copy(&path, &dest)
                .map_err(|e| format!("staging {}: {e}", path.display()))?;
            copied += 1;
        }
    }
    if copied == 0 {
        return Err(format!(
            "no .db files in {} — not a Playnite library folder",
            library_dir.display()
        ));
    }
    let lib = ufm_media::playnite::load_library(&staged)
        .map_err(|e| format!("reading the Playnite library: {e}"))?;
    Ok((staged, lib))
}

/// Where one recorded Windows path lands on this machine.
struct PlannedInstall {
    /// Absolute path of the install folder (rebased onto the
    /// volume the library was found on; for recorded-only games
    /// this is the path it WOULD have under that root).
    abs_dir: PathBuf,
    /// Volume-relative path (forward slashes) for evidence.
    rel: String,
    present: bool,
    exe_name: Option<String>,
    exe_rel: Option<String>,
    /// The location the item belongs to + the fs facts.
    location_id: String,
    location_provider: String,
    location_name: String,
    fs_uuid: Option<String>,
    fs_label: Option<String>,
}

/// Split a recorded Windows path into components (any drive
/// letter, backslash or forward slash separators).
fn windows_components(recorded: &str) -> Vec<String> {
    let s = recorded.trim();
    let s = if s.len() > 2 && s.as_bytes()[1] == b':' {
        &s[2..]
    } else {
        s
    };
    s.split(['\\', '/'])
        .filter(|c| !c.is_empty())
        .map(|c| c.to_string())
        .collect()
}

/// Find `base/rel...` on disk: exact first, then component-wise
/// case-insensitive. Returns the on-disk path when it exists.
fn resolve_on_disk(base: &Path, rel: &[String]) -> Option<PathBuf> {
    let exact: PathBuf = rel.iter().collect::<PathBuf>();
    let exact = base.join(&exact);
    if exact.exists() {
        return Some(exact);
    }
    let mut cur = base.to_path_buf();
    for comp in rel {
        let entries = std::fs::read_dir(&cur).ok()?;
        let mut found = None;
        for e in entries.flatten() {
            if e.file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(comp)
            {
                found = Some(e.path());
                break;
            }
        }
        cur = found?;
    }
    if cur.exists() {
        Some(cur)
    } else {
        None
    }
}

/// The volume a path lives on: longest mount-prefix match.
fn volume_for<'a>(volumes: &'a [MountedVolume], path: &Path) -> Option<&'a MountedVolume> {
    let mut best: Option<&MountedVolume> = None;
    for v in volumes {
        if path.starts_with(&v.path) {
            if best.map(|b| v.path.len() > b.path.len()).unwrap_or(true) {
                best = Some(v);
            }
        }
    }
    best
}

fn location_for(
    volumes: &[MountedVolume],
    path: &Path,
    fallback_seed_path: &Path,
) -> (String, String, String, Option<String>, Option<String>) {
    if let Some(v) = volume_for(volumes, path) {
        return (
            volume_uuid(&format!("volume:{}", v.path)).to_string(),
            "volume".to_string(),
            v.name.clone(),
            v.uuid.clone(),
            v.label.clone(),
        );
    }
    (
        volume_uuid(&format!(
            "pc-library:{}",
            fallback_seed_path.to_string_lossy()
        ))
        .to_string(),
        "local".to_string(),
        "PC Games".to_string(),
        None,
        None,
    )
}

/// The play action's exe: substitute {InstallDir} against the
/// RECORDED dir, then split — the exe's path relative to the
/// install dir and its file name.
fn plan_exe(game: &PlayniteGame) -> Option<(String, String)> {
    let action = game
        .play_actions
        .iter()
        .find(|a| a.is_play_action && a.path.is_some())
        .or_else(|| game.play_actions.iter().find(|a| a.path.is_some()))?;
    let raw = action.path.as_ref()?;
    let recorded_dir = game.install_directory.as_deref().unwrap_or("");
    let substituted = raw.replace("{InstallDir}", recorded_dir);
    let exe_comps = windows_components(&substituted);
    let dir_comps = windows_components(recorded_dir);
    let name = exe_comps.last()?.clone();
    let rel = if exe_comps.len() > dir_comps.len()
        && exe_comps[..dir_comps.len()]
            .iter()
            .zip(&dir_comps)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
    {
        exe_comps[dir_comps.len()..].join("/")
    } else {
        name.clone()
    };
    Some((name, rel))
}

/// Storefront labels from the record's links (the design's §1
/// precedence: Playnite's own library = the source; storefronts
/// arrive per-game from links, with the source name first).
fn storefronts_of(game: &PlayniteGame) -> Vec<String> {
    const KNOWN: &[(&str, &str)] = &[
        ("store.steampowered.com", "Steam"),
        ("gog.com", "GOG"),
        ("epicgames.com", "Epic"),
        ("itch.io", "itch.io"),
        ("humblebundle.com", "Humble"),
        ("origin.com", "EA"),
        ("ea.com", "EA"),
        ("battle.net", "Battle.net"),
        ("ubisoft.com", "Ubisoft"),
        ("xbox.com", "Xbox"),
        ("microsoft.com", "Microsoft Store"),
        ("amazon.", "Amazon"),
    ];
    let mut out: Vec<String> = Vec::new();
    if let Some(source) = &game.source_name {
        if !source.is_empty() {
            out.push(source.clone());
        }
    }
    for (_, url) in &game.links {
        let lower = url.to_lowercase();
        for (needle, label) in KNOWN {
            if lower.contains(needle) && !out.iter().any(|s| s == label) {
                out.push(label.to_string());
            }
        }
    }
    out
}

/// Meaningful lowercase tokens for the mislabel check.
fn name_tokens(s: &str) -> HashSet<String> {
    const STOP: &[&str] = &["the", "of", "and", "for", "edition", "game", "year"];
    let mut out = HashSet::new();
    // Whole-word forms ride along with the split forms, so a
    // stylized title ("RepliCant") still matches its plain
    // folder ("Replicant") through the unsplit token.
    for w in s.split(|c: char| !c.is_alphanumeric()) {
        let w = w.to_lowercase();
        if w.chars().count() >= 4 && !STOP.contains(&w.as_str()) {
            out.insert(w);
        }
    }
    let mut cur = String::new();
    let mut prev_lower = false;
    let mut flush = |cur: &mut String, out: &mut HashSet<String>| {
        if cur.chars().count() >= 4 && !STOP.contains(&cur.as_str()) {
            let w = std::mem::take(cur);
            out.insert(w.clone());
            // The stemmed variant rides along (never replaces):
            // "crysis" the title token must match "crysi" split
            // out of the glued disk form "Crysis2", and back.
            if w.chars().count() > 4 && w.ends_with('s') {
                out.insert(w[..w.len() - 1].to_string());
            }
        } else {
            cur.clear();
        }
    };
    let mut prev_digit = false;
    for c in s.chars() {
        if c.is_alphanumeric() {
            // Split CamelCase humps and letter/digit seams
            // ("Cyberpunk2077" -> cyberpunk + 2077).
            if !cur.is_empty()
                && ((prev_lower && c.is_uppercase())
                    || (prev_digit != c.is_ascii_digit()))
            {
                flush(&mut cur, &mut out);
            }
            prev_lower = c.is_lowercase();
            prev_digit = c.is_ascii_digit();
            cur.extend(c.to_lowercase());
        } else {
            flush(&mut cur, &mut out);
            prev_lower = false;
            prev_digit = false;
        }
    }
    flush(&mut cur, &mut out);
    out
}

/// Does the install folder / exe naming contradict the title?
/// (The corpus's known case: "A Plague Tale: Innocence" pointing
/// at the Requiem folder and exe.)
fn mislabel_note(
    game: &PlayniteGame,
    exe_name: Option<&str>,
    abs_dir: &Path,
    rel_depth: usize,
) -> Option<String> {
    let title_tokens = name_tokens(&game.name);
    if title_tokens.is_empty() {
        return None;
    }
    let folder = abs_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // The vendor nests games in version-code subfolders and
    // deep binary paths (bin/x64), so the naming evidence is
    // the whole RECORDED path: every component of the install
    // dir below the volume root (abs_dir's last rel_depth
    // components).
    let mut named_path = name_tokens(&folder);
    let comps: Vec<String> = abs_dir
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let start = comps.len().saturating_sub(rel_depth.max(1));
    for comp in &comps[start..] {
        named_path.extend(name_tokens(comp));
    }
    let mut exe_tokens: HashSet<String> = HashSet::new();
    if let Some(exe) = exe_name {
        let stem = exe.rsplit_once('.').map(|(s, _)| s).unwrap_or(exe);
        exe_tokens = name_tokens(stem);
    }
    let mut named = named_path.clone();
    named.extend(exe_tokens.iter().cloned());
    if named.is_empty() {
        return None;
    }
    // Gate 1 — total disjointness: the name matches nothing.
    // Gate 2 — series swap: the title carries a distinguishing
    // token absent from disk, and the exe carries one the title
    // lacks that the FOLDER corroborates (title "Innocence" vs
    // exe + folder both saying "Requiem"). An uncorroborated
    // exe extra ("Offline", build tags) is a launcher variant,
    // not a different game.
    // A glued compound counts as a match: a title token inside
    // a disk token ("2k21" in "nba2k21") or the reverse is real
    // evidence of relation, not disjointness.
    let glued = title_tokens.iter().any(|t| {
        named.iter().any(|n| n.contains(t.as_str()) || t.contains(n.as_str()))
    });
    let disjoint = title_tokens.is_disjoint(&named) && !glued;
    let swap = !exe_tokens.is_disjoint(&title_tokens)
        && title_tokens.difference(&named).next().is_some()
        && exe_tokens
            .difference(&title_tokens)
            // The corroborating word must be a WORD — a version
            // number sitting in a folder name corroborates
            // nothing.
            .any(|t| t.len() >= 4 && t.chars().all(|c| c.is_alphabetic()) && named_path.contains(t));
    if !disjoint && !swap {
        return None;
    }
    Some(format!(
        "Folder/exe name differs from the Playnite title (\"{}\" vs folder \"{}\")",
        game.name, folder
    ))
}

struct Planned {
    game: PlayniteGame,
    install: PlannedInstall,
    storefronts: Vec<String>,
    mislabel: Option<String>,
    /// (media_type, source file under library/files/) that exist.
    art: Vec<(String, PathBuf)>,
    art_missing: usize,
}

fn plan_library(
    det: &Detected,
    library_files: &Path,
) -> (Vec<Planned>, i64, Vec<String>) {
    let mut notes: Vec<String> = Vec::new();
    let mut planned: Vec<Planned> = Vec::new();
    // Media folders claimed by no record = orphans (never adopted).
    let mut claimed_folders: HashSet<String> = HashSet::new();
    for g in &det.lib.games {
        claimed_folders.insert(g.id.clone());
    }
    let mut orphans = 0i64;
    if library_files.is_dir() {
        if let Ok(entries) = std::fs::read_dir(library_files) {
            for e in entries.flatten() {
                if e.path().is_dir() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if !claimed_folders.contains(&name) {
                        orphans += 1;
                    }
                }
            }
        }
    }
    for game in &det.lib.games {
        let recorded = game.install_directory.clone().unwrap_or_default();
        let rel_comps = windows_components(&recorded);
        let rel = rel_comps.join("/");
        // Resolution bases, most specific first: the detection
        // root, then ancestors of the library dir (Playnite root,
        // volume root).
        let mut bases: Vec<PathBuf> = vec![det.root.clone()];
        let mut anc = det.library_dir.parent();
        while let Some(p) = anc {
            if !bases.contains(&p.to_path_buf()) {
                bases.push(p.to_path_buf());
            }
            if bases.len() >= 4 {
                break;
            }
            anc = p.parent();
        }
        let mut on_disk: Option<PathBuf> = None;
        for base in &bases {
            if let Some(found) = resolve_on_disk(base, &rel_comps) {
                on_disk = Some(found);
                break;
            }
        }
        let (abs_dir, present_dir) = match &on_disk {
            Some(p) => (p.clone(), true),
            None => (det.root.join(&rel), false),
        };
        let (exe_name, exe_rel) = plan_exe(game).map(|(n, r)| (Some(n), Some(r))).unwrap_or((None, None));
        let exe_present = match (&exe_rel, present_dir) {
            (Some(rel), true) => abs_dir.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR)).exists(),
            _ => false,
        };
        let present = present_dir && (exe_rel.is_none() || exe_present);
        let (location_id, location_provider, location_name, fs_uuid, fs_label) =
            location_for(&det.volumes, &abs_dir, &det.root);
        let install = PlannedInstall {
            abs_dir: abs_dir.clone(),
            rel,
            present,
            exe_name: exe_name.clone(),
            exe_rel: exe_rel.clone(),
            location_id,
            location_provider,
            location_name,
            fs_uuid,
            fs_label,
        };
        // Art: the record's media paths under library/files/.
        let mut art = Vec::new();
        let mut art_missing = 0usize;
        for (media_type, rel_path) in [
            ("cover", &game.cover_image),
            ("background", &game.background_image),
            ("icon", &game.icon),
        ] {
            match rel_path {
                Some(rp) => {
                    let src = library_files.join(rp.replace('\\', "/"));
                    if src.is_file() {
                        art.push((media_type.to_string(), src));
                    } else {
                        art_missing += 1;
                    }
                }
                None => {}
            }
        }
        let mislabel = mislabel_note(game, exe_name.as_deref(), &abs_dir, rel_comps.len());
        if let Some(note) = &mislabel {
            notes.push(format!("{}: {note}", game.name));
        }
        planned.push(Planned {
            storefronts: storefronts_of(game),
            game: game.clone(),
            install,
            mislabel,
            art,
            art_missing,
        });
    }
    (planned, orphans, notes)
}

fn roots_from_state(state: &AppState, root: Option<String>) -> Vec<PathBuf> {
    match &root {
        Some(p) => vec![PathBuf::from(p)],
        None => state
            .local_roots
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect(),
    }
}

fn detect_and_plan(
    roots: Vec<PathBuf>,
) -> Result<(Detected, PathBuf, Vec<Planned>, i64, Vec<String>), String> {
    let mut found: Option<(PathBuf, PathBuf)> = None;
    for r in &roots {
        if let Some(lib_dir) = detect_library_dir(r) {
            found = Some((r.clone(), lib_dir));
            break;
        }
    }
    let (root, library_dir) = found.ok_or_else(|| {
        format!(
            "no Playnite library found under {} searched root(s) \
             (looked for Playnite/library/games.db)",
            roots.len()
        )
    })?;
    let (staged_dir, lib) = stage_and_load(&library_dir)?;
    let volumes = current_filesystems();
    let det = Detected {
        root: root.clone(),
        library_dir: library_dir.clone(),
        staged_dir,
        lib,
        volumes,
    };
    let library_files = library_dir.join("files");
    let (planned, orphans, notes) = plan_library(&det, &library_files);
    Ok((det, library_files, planned, orphans, notes))
}

// ---------------------------------------------------------------------------
// Art adoption (copy-on-import, magic-sniffed extension)
// ---------------------------------------------------------------------------

fn sniff_image_ext(path: &Path) -> Option<&'static str> {
    let mut buf = [0u8; 12];
    let n = std::fs::File::open(path)
        .and_then(|mut f| {
            use std::io::Read;
            f.read(&mut buf)
        })
        .ok()?;
    let b = &buf[..n];
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if b.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        Some("png")
    } else if b.starts_with(&[0x00, 0x00, 0x01, 0x00]) {
        Some("ico")
    } else if b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("webp")
    } else if b.starts_with(b"GIF8") {
        Some("gif")
    } else if b.starts_with(b"BM") {
        Some("bmp")
    } else {
        None
    }
}

/// Copy one adopted art file into the media store (idempotent:
/// same size = already there). Returns the destination path.
fn adopt_art(src: &Path, dest_dir: &Path, media_type: &str) -> Result<PathBuf, String> {
    let ext = sniff_image_ext(src).ok_or_else(|| {
        format!("{}: unrecognized image format", src.display())
    })?;
    std::fs::create_dir_all(dest_dir).map_err(|e| format!("media dir: {e}"))?;
    let dest = dest_dir.join(format!("{media_type}.{ext}"));
    let src_len = std::fs::metadata(src).map(|m| m.len()).unwrap_or(0);
    if dest.exists()
        && std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0) == src_len
    {
        return Ok(dest);
    }
    std::fs::copy(src, &dest).map_err(|e| format!("copying {}: {e}", src.display()))?;
    Ok(dest)
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn pc_import_preview(
    app: tauri::AppHandle,
    root: Option<String>,
) -> Cmd<PcImportPreview> {
    let result = tauri::async_runtime::spawn_blocking(move || {
        let guard = app.state::<AppState>();
        let state: &AppState = &guard;
        detect_and_plan(roots_from_state(state, root))
    })
    .await;
    match result {
        Ok(Ok((det, _files, planned, orphans, notes))) => {
            let mut preview = PcImportPreview {
                found: true,
                library_path: det.library_dir.to_string_lossy().into_owned(),
                root: det.root.to_string_lossy().into_owned(),
                records_seen: planned.len() as i64,
                orphan_media_folders: orphans,
                notes,
                ..Default::default()
            };
            let mut seen_keys: HashSet<String> = HashSet::new();
            for p in &planned {
                let key = p.install.abs_dir.to_string_lossy().into_owned();
                if !seen_keys.insert(key) {
                    preview.duplicates_merged += 1;
                    continue;
                }
                if p.install.present {
                    preview.would_import_present += 1;
                } else {
                    preview.recorded_not_present += 1;
                }
                preview.art_available += p.art.len() as i64;
                preview.art_missing += p.art_missing as i64;
            }
            let _ = std::fs::remove_dir_all(&det.staged_dir);
            Ok(preview)
        }
        Ok(Err(e)) => Ok(PcImportPreview {
            found: false,
            errors: vec![e],
            ..Default::default()
        }),
        Err(e) => Err(CommandError::from(UfmError::Internal(format!(
            "pc preview task failed: {e}"
        )))),
    }
}

fn run_pc_import(
    app: &tauri::AppHandle,
    state: &AppState,
    root: Option<String>,
) -> Cmd<PcImportReport> {
    let roots = roots_from_state(state, root);
    let media_root = crate::rommedia::default_media_root();
    let app2 = app.clone();
    execute_pc_import(&state.catalog, roots, &media_root, &move |p| {
        let _ = app2.emit("pc-import-progress", p);
    })
    .map_err(|e| CommandError::from(UfmError::Internal(e)))
}

/// The import engine, free of the Tauri state so the proof
/// harness (src-tauri/examples/pc_import_proof.rs) drives the
/// exact code the shipped command runs.
pub fn execute_pc_import(
    catalog: &std::sync::Mutex<ufm_catalog::Catalog>,
    roots: Vec<PathBuf>,
    media_root: &Path,
    emit: &dyn Fn(PcProgress),
) -> Result<PcImportReport, String> {
    let (det, _files, planned, orphans, _notes) = detect_and_plan(roots)?;
    let mut report = PcImportReport {
        library_path: det.library_dir.to_string_lossy().into_owned(),
        root: det.root.to_string_lossy().into_owned(),
        records_seen: planned.len() as i64,
        orphan_media_folders: orphans,
        ..Default::default()
    };
    let total = planned.len() as u32;
    let now = chrono::Utc::now().to_rfc3339();
    let mut seen_keys: HashSet<String> = HashSet::new();
    let mut ensured_locations: HashSet<String> = HashSet::new();
    for (i, p) in planned.iter().enumerate() {
        emit(PcProgress {
            phase: "import".to_string(),
            done: i as u32,
            total,
            title: p.game.name.clone(),
        });
        let key = p.install.abs_dir.to_string_lossy().into_owned();
        if !seen_keys.insert(key.clone()) {
            report.duplicates_merged += 1;
            continue;
        }
        if !p.install.present {
            report.recorded_not_present += 1;
        }
        if p.mislabel.is_some() {
            report.mislabel_notes += 1;
        }
        report.art_missing += p.art_missing as i64;
        let evidence = serde_json::json!({
            "source": "playnite",
            "library": p.install.fs_uuid.clone().unwrap_or_else(|| "local".to_string()),
            "library_name": "Playnite",
            "playnite_id": p.game.id,
            "game_id": p.game.game_id,
            "volume_relative_path": p.install.rel,
            "recorded_path": p.game.install_directory,
            "imported_at": now,
            "db_version": 4,
            "litedb": "4.1.4",
            "vendor_playtime_secs": p.game.playtime_secs,
            "vendor_play_count": p.game.play_count,
        });
        let record = ufm_catalog::PcInstallRecord {
            location_id: p.install.location_id.clone(),
            provider_key: key.clone(),
            folder_name: p
                .install
                .abs_dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.game.name.clone()),
            folder_modified: std::fs::metadata(&p.install.abs_dir)
                .and_then(|m| m.modified())
                .ok()
                .map(chrono::DateTime::<chrono::Utc>::from),
            title: p.game.name.clone(),
            year: p.game.release_year,
            evidence_json: evidence.to_string(),
            source_kind: "playnite".to_string(),
            library_name: "Playnite".to_string(),
            playnite_id: Some(p.game.id.clone()),
            game_guid: p.game.game_id.clone(),
            description: p.game.description.clone(),
            genres: p.game.genres.clone(),
            developers: p.game.developers.clone(),
            publishers: p.game.publishers.clone(),
            series: p.game.series.clone(),
            features: p.game.features.clone(),
            age_ratings: p.game.age_ratings.clone(),
            links: p
                .game
                .links
                .iter()
                .map(|(name, url)| ufm_catalog::PcLink {
                    name: name.clone(),
                    url: url.clone(),
                })
                .collect(),
            storefronts: p.storefronts.clone(),
            critic_score: p.game.critic_score,
            community_score: p.game.community_score,
            install_size: p.game.install_size,
            version: p.game.version.clone(),
            exe_name: p.install.exe_name.clone(),
            exe_rel: p.install.exe_rel.clone(),
            release_raw: p.game.release_raw.clone(),
            mislabel_note: p.mislabel.clone(),
            present: p.install.present,
            imported_at: now.clone(),
        };
        // Per-game catalog unit: the rest of the app breathes
        // between games on a long import (ES precedent).
        let outcome = {
            let catalog = catalog.lock().unwrap();
            if ensured_locations.insert(p.install.location_id.clone()) {
                let _ = catalog.pc_ensure_location(
                    &p.install.location_id,
                    &p.install.location_provider,
                    &p.install.location_name,
                    VOLUME_CAPS_JSON,
                );
            }
            let _ = crate::commands::record_volume_sightings(&catalog);
            catalog.upsert_pc_install(&record)
        };
        match outcome {
            Ok(out) => {
                if out.skipped_precedence {
                    report.skipped_precedence += 1;
                    continue;
                }
                if out.created {
                    report.imported += 1;
                } else {
                    report.refreshed += 1;
                }
                if out.upgraded {
                    report.upgraded += 1;
                }
                // Art adoption against the written item.
                if !p.art.is_empty() {
                    let dest_dir = media_root.join("pcgames").join(&p.game.id);
                    let catalog = catalog.lock().unwrap();
                    for (media_type, src) in &p.art {
                        match adopt_art(src, &dest_dir, media_type) {
                            Ok(dest) => {
                                let _ = catalog.upsert_rom_media(
                                    &out.item_id,
                                    media_type,
                                    "found",
                                    Some("playnite"),
                                    Some(&dest.to_string_lossy()),
                                    "{}",
                                );
                                report.art_adopted += 1;
                            }
                            Err(e) => report.errors.push(format!("{}: {e}", p.game.name)),
                        }
                    }
                }
            }
            Err(e) => report
                .errors
                .push(format!("{}: import failed: {e}", p.game.name)),
        }
    }
    emit(PcProgress {
        phase: "import".to_string(),
        done: total,
        total,
        title: String::new(),
    });
    let _ = std::fs::remove_dir_all(&det.staged_dir);
    Ok(report)
}

#[tauri::command]
pub async fn pc_import_playnite(
    app: tauri::AppHandle,
    root: Option<String>,
) -> Cmd<PcImportReport> {
    if PC_WORK_RUNNING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Ok(PcImportReport {
            already_running: true,
            ..Default::default()
        });
    }
    let app2 = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let guard = app2.state::<AppState>();
        let state: &AppState = &guard;
        run_pc_import(&app2, state, root)
    })
    .await;
    PC_WORK_RUNNING.store(false, Ordering::SeqCst);
    match result {
        Ok(r) => r,
        Err(e) => Err(CommandError::from(UfmError::Internal(format!(
            "pc import task failed: {e}"
        )))),
    }
}

// ---------------------------------------------------------------------------
// Folder-scan fallback
// ---------------------------------------------------------------------------

/// Clean a folder name into a working title: strip trailing
/// bracket/paren groups and version suffixes ("W25A v1.0.0.GOG"
/// tails, "v1.2" tails). Honest heuristics — the sheet carries
/// the folder name verbatim either way.
pub fn clean_folder_title(name: &str) -> String {
    let mut s = name.trim().to_string();
    loop {
        let t = s.trim_end().to_string();
        if (t.ends_with(']') && t.contains('[')) || (t.ends_with(')') && t.contains('(')) {
            let open = t.rfind(['[', '(']).unwrap();
            s = t[..open].trim_end().to_string();
            continue;
        }
        s = t;
        break;
    }
    // A trailing " v<digits…>" version tag.
    if let Some(idx) = s.rfind(" v") {
        let tail = &s[idx + 2..];
        if tail
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
            && tail.chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
        {
            s = s[..idx].trim_end().to_string();
        }
    }
    if s.is_empty() {
        name.trim().to_string()
    } else {
        s
    }
}

/// Probe one candidate folder for its play exe: breadth-first to
/// depth 4, capped entries; the shallowest depth wins, then the
/// largest exe. Returns (abs path, rel path with '/').
fn probe_exe(dir: &Path) -> Option<(PathBuf, String)> {
    const MAX_DEPTH: usize = 4;
    const MAX_ENTRIES: usize = 20_000;
    let mut entries_seen = 0usize;
    let mut level: Vec<PathBuf> = vec![dir.to_path_buf()];
    let mut depth = 0usize;
    let mut best: Option<(u64, PathBuf)> = None;
    while !level.is_empty() && depth <= MAX_DEPTH {
        let mut next: Vec<PathBuf> = Vec::new();
        for d in &level {
            let Ok(read) = std::fs::read_dir(d) else {
                continue;
            };
            for e in read.flatten() {
                entries_seen += 1;
                if entries_seen > MAX_ENTRIES {
                    break;
                }
                let p = e.path();
                if p.is_dir() {
                    if depth < MAX_DEPTH {
                        next.push(p);
                    }
                } else if p
                    .extension()
                    .map(|x| x.eq_ignore_ascii_case("exe"))
                    .unwrap_or(false)
                {
                    let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                    if best.as_ref().map(|(s, _)| size > *s).unwrap_or(true) {
                        best = Some((size, p.clone()));
                    }
                }
            }
            if entries_seen > MAX_ENTRIES {
                break;
            }
        }
        if best.is_some() {
            break; // shallowest depth with an exe wins
        }
        level = next;
        depth += 1;
    }
    let (_, exe) = best?;
    let rel = exe
        .strip_prefix(dir)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    Some((exe, rel))
}

fn best_local_image(dir: &Path) -> Option<PathBuf> {
    const NAMES: &[&str] = &["cover", "folder", "boxart", "front"];
    const EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp"];
    let Ok(read) = std::fs::read_dir(dir) else {
        return None;
    };
    let mut images: Vec<(String, u64, PathBuf)> = Vec::new();
    for e in read.flatten() {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let ext_ok = p
            .extension()
            .map(|x| EXTS.iter().any(|e2| x.eq_ignore_ascii_case(e2)))
            .unwrap_or(false);
        if ext_ok {
            let stem = p
                .file_stem()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            images.push((stem, size, p));
        }
    }
    for want in NAMES {
        if let Some((_, _, p)) = images.iter().find(|(stem, _, _)| stem == want) {
            return Some(p.clone());
        }
    }
    images.sort_by(|a, b| b.1.cmp(&a.1));
    images.first().map(|(_, _, p)| p.clone())
}

fn run_pc_scan(app: &tauri::AppHandle, state: &AppState, root: String) -> Cmd<PcScanReport> {
    let media_root = crate::rommedia::default_media_root();
    let app2 = app.clone();
    Ok(execute_pc_scan(&state.catalog, root, &media_root, &move |p| {
        let _ = app2.emit("pc-import-progress", p);
    }))
}

/// The folder-scan engine, free of the Tauri state (same
/// proof-harness door as the import).
pub fn execute_pc_scan(
    catalog: &std::sync::Mutex<ufm_catalog::Catalog>,
    root: String,
    media_root: &Path,
    emit: &dyn Fn(PcProgress),
) -> PcScanReport {
    let root_path = PathBuf::from(&root);
    let mut report = PcScanReport {
        root: root.clone(),
        ..Default::default()
    };
    if !root_path.is_dir() {
        report.errors.push(format!("{root} is not a folder"));
        return report;
    }
    let volumes = current_filesystems();
    let now = chrono::Utc::now().to_rfc3339();
    // Candidates: the root itself + its immediate subfolders.
    let mut candidates: Vec<PathBuf> = vec![root_path.clone()];
    if let Ok(read) = std::fs::read_dir(&root_path) {
        for e in read.flatten() {
            let p = e.path();
            if p.is_dir() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with('.')
                    || name.eq_ignore_ascii_case("$RECYCLE.BIN")
                    || name.eq_ignore_ascii_case("System Volume Information")
                {
                    continue;
                }
                candidates.push(p);
            }
        }
    }
    candidates.sort();
    let total = candidates.len() as u32;
    let mut ensured = false;
    // Playnite-owned folders defer to the richer record — a
    // candidate that IS a recorded install folder, or CONTAINS
    // one (the vendor nests installs in version subfolders, and
    // container folders hold whole collections), is covered.
    let playnite_keys: Vec<String> = {
        let catalog = catalog.lock().unwrap();
        catalog
            .pc_playnite_keys()
            .unwrap_or_default()
            .iter()
            .map(|k| k.to_lowercase())
            .collect()
    };
    // Covered flags first (string math — no disk), probes only
    // for the uncovered, each exactly once.
    let covered_flags: Vec<bool> = candidates
        .iter()
        .map(|cand| {
            let cand_key = cand.to_string_lossy().to_lowercase();
            playnite_keys.iter().any(|k| {
                k.as_str() == cand_key
                    || k.starts_with(&format!("{cand_key}/"))
            })
        })
        .collect();
    let probes: Vec<Option<(PathBuf, String)>> = candidates
        .iter()
        .enumerate()
        .map(|(i, c)| {
            if covered_flags[i] {
                None
            } else {
                probe_exe(c)
            }
        })
        .collect();
    // Settle the root: it is a CONTAINER when a subfolder holds
    // a game — unless every positive subfolder is a generic
    // binary dir (Binaries/Win64/...), in which case the root
    // IS the game and the binary dirs defer up to it.
    const GENERIC_SUBS: &[&str] = &[
        "bin", "bin32", "bin64", "binaries", "win64", "win32", "x64", "x86",
        "win_x64", "win_x86",
    ];
    let mut suppressed: std::collections::HashSet<usize> = Default::default();
    let root_positive = probes.first().map(|p| p.is_some()).unwrap_or(false);
    let positive_subs: Vec<usize> = (1..candidates.len())
        .filter(|&i| probes[i].is_some())
        .collect();
    if root_positive && !positive_subs.is_empty() {
        let all_generic = positive_subs.iter().all(|&i| {
            candidates[i]
                .file_name()
                .map(|n| {
                    GENERIC_SUBS
                        .iter()
                        .any(|g| n.to_string_lossy().eq_ignore_ascii_case(g))
                })
                .unwrap_or(false)
        });
        if all_generic {
            suppressed.extend(positive_subs.iter().copied());
        } else {
            suppressed.insert(0);
        }
    }
    for (i, cand) in candidates.iter().enumerate() {
        emit(PcProgress {
            phase: "scan".to_string(),
            done: i as u32,
            total,
            title: cand
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        });
        report.folders_seen += 1;
        if covered_flags[i] {
            report.covered_by_playnite += 1;
            continue;
        }
        if suppressed.contains(&i) {
            continue;
        }
        let Some((exe_abs, exe_rel)) = &probes[i] else {
            report.no_exe_found += 1;
            continue;
        };
        let (exe_abs, exe_rel) = (exe_abs.clone(), exe_rel.clone());
        let key = cand.to_string_lossy().into_owned();
        // A Playnite import owns the folder if one exists —
        // checked before writing (the catalog enforces it too).
        let (location_id, location_provider, location_name, fs_uuid, _label) =
            location_for(&volumes, cand, &root_path);
        let folder_name = cand
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.clone());
        let title = clean_folder_title(&folder_name);
        let exe_name = exe_abs
            .file_name()
            .map(|n| n.to_string_lossy().into_owned());
        let evidence = serde_json::json!({
            "source": "folder-scan",
            "library": fs_uuid.clone().unwrap_or_else(|| "local".to_string()),
            "confidence": "heuristic",
            "exe": exe_rel,
            "imported_at": now,
        });
        let record = ufm_catalog::PcInstallRecord {
            location_id: location_id.clone(),
            provider_key: key.clone(),
            folder_name: folder_name.clone(),
            folder_modified: std::fs::metadata(cand)
                .and_then(|m| m.modified())
                .ok()
                .map(chrono::DateTime::<chrono::Utc>::from),
            title,
            year: None,
            evidence_json: evidence.to_string(),
            source_kind: "folder-scan".to_string(),
            library_name: "Folder finds".to_string(),
            playnite_id: None,
            game_guid: None,
            description: None,
            genres: vec![],
            developers: vec![],
            publishers: vec![],
            series: vec![],
            features: vec![],
            age_ratings: vec![],
            links: vec![],
            storefronts: vec![],
            critic_score: None,
            community_score: None,
            install_size: None,
            version: None,
            exe_name,
            exe_rel: Some(exe_rel),
            release_raw: None,
            mislabel_note: None,
            present: true,
            imported_at: now.clone(),
        };
        let outcome = {
            let catalog = catalog.lock().unwrap();
            if !ensured {
                let _ = catalog.pc_ensure_location(
                    &location_id,
                    &location_provider,
                    &location_name,
                    VOLUME_CAPS_JSON,
                );
                let _ = crate::commands::record_volume_sightings(&catalog);
                ensured = true;
            }
            catalog.upsert_pc_install(&record)
        };
        match outcome {
            Ok(out) => {
                if out.skipped_precedence {
                    report.covered_by_playnite += 1;
                    continue;
                }
                if out.created {
                    report.imported += 1;
                } else {
                    report.refreshed += 1;
                }
                if let Some(img) = best_local_image(cand) {
                    let slug = format!(
                        "scan-{}",
                        volume_uuid(&format!("folder-scan:{key}")).simple()
                    );
                    let dest_dir = media_root.join("pcgames").join(slug);
                    if let Ok(dest) = adopt_art(&img, &dest_dir, "cover") {
                        let catalog = catalog.lock().unwrap();
                        let _ = catalog.upsert_rom_media(
                            &out.item_id,
                            "cover",
                            "found",
                            Some("folder-scan"),
                            Some(&dest.to_string_lossy()),
                            "{}",
                        );
                    }
                }
            }
            Err(e) => report.errors.push(format!("{key}: scan failed: {e}")),
        }
    }
    report
}

#[tauri::command]
pub async fn pc_scan_folder(app: tauri::AppHandle, root: String) -> Cmd<PcScanReport> {
    if PC_WORK_RUNNING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Ok(PcScanReport {
            already_running: true,
            root,
            ..Default::default()
        });
    }
    let app2 = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let guard = app2.state::<AppState>();
        let state: &AppState = &guard;
        run_pc_scan(&app2, state, root)
    })
    .await;
    PC_WORK_RUNNING.store(false, Ordering::SeqCst);
    match result {
        Ok(r) => r,
        Err(e) => Err(CommandError::from(UfmError::Internal(format!(
            "pc scan task failed: {e}"
        )))),
    }
}

// ---------------------------------------------------------------------------
// Sheet + badge doors
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn pc_game_sheet(
    state: tauri::State<'_, AppState>,
    provider_key: String,
) -> Cmd<Option<ufm_catalog::PcGameSheet>> {
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    catalog.pc_game_sheet(&provider_key).map_err(CommandError::from)
}

#[tauri::command]
pub fn pc_set_launch_badge(
    state: tauri::State<'_, AppState>,
    provider_key: String,
    badge: String,
    note: Option<String>,
) -> Cmd<bool> {
    if !BADGES.contains(&badge.as_str()) {
        return Err(CommandError::from(UfmError::InvalidInput(format!(
            "unknown launch badge {badge:?}"
        ))));
    }
    let catalog = ufm_catalog::Catalog::open(&state.catalog_path)
        .map_err(CommandError::from)?;
    let now = chrono::Utc::now().to_rfc3339();
    catalog
        .pc_set_launch_badge(&provider_key, &badge, note.as_deref(), &now)
        .map_err(CommandError::from)
}
