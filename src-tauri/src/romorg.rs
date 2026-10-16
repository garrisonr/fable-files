//! ROM Organize (Organize parity, Dickles 2026-10-16): file the
//! identified games by platform + DAT title.
//!
//! Law-for-law a sibling of `mediares::plan_organize`:
//! - STORED identities only — the plan the user previews is exactly
//!   what the job will do, computed from `media_identities` rows the
//!   identification pass wrote (media_kind "rom": title = DAT display
//!   name, platform = DAT system key). Nothing is renamed on a
//!   filename guess; an unidentified file is excluded with the reason.
//! - Collisions skip with a report; Fable never overwrites. Two
//!   sources wanting one destination are BOTH excluded — never a
//!   silent winner.
//! - A file that is one piece of a loose disc set (its identity names
//!   a member image sitting beside it on disk) is excluded: moving
//!   half a set would break it. Archives move whole — for a container
//!   match the file itself is the game.
//!
//! Layout: `<root>/<Platform Display>/<DAT Title>.<ext>` — pure
//! layout lives in `ufm_media::romorg`; platform display names come
//! from the bundled DAT dataset (`RomDataset::systems()`), resolved
//! by the command layer and passed in.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use ufm_catalog::Catalog;

/// Expansion cap for a menu-invoked organize (the mediares scan caps
/// at 500 because every row may cost a TMDB lookup; a plan read is
/// local-only, so the ceiling can be honest about a big folder).
pub const MAX_ORGANIZE_FILES: usize = 2000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RomOrganizeAction {
    pub src: String,
    pub dst: String,
    pub rel: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RomOrganizeExclusion {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RomOrganizePlan {
    pub actions: Vec<RomOrganizeAction>,
    pub excluded: Vec<RomOrganizeExclusion>,
    /// True when the input expansion hit MAX_ORGANIZE_FILES — the
    /// preview says so rather than quietly covering part of a folder.
    pub truncated: bool,
}

/// Expand inputs the way the Media Resolution scan does: directory
/// inputs walk recursively; file inputs pass through. No extension
/// filter — the identity decides what is a game; everything else is
/// excluded by the plan with its reason.
pub fn expand_inputs(paths: &[String]) -> (Vec<PathBuf>, bool) {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut truncated = false;
    let mut queue: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    queue.sort();
    while let Some(p) = queue.pop() {
        if p.is_dir() {
            let mut entries: Vec<PathBuf> = std::fs::read_dir(&p)
                .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
                .unwrap_or_default();
            entries.sort();
            queue.extend(entries);
        } else if p.is_file() {
            if out.len() >= MAX_ORGANIZE_FILES {
                truncated = true;
            } else {
                out.push(p);
            }
        }
    }
    out.sort();
    out.dedup();
    (out, truncated)
}

fn ext_of(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// True when the identity's `member` names a file that sits BESIDE
/// this one on disk (a loose cue/image disc set). Members inside an
/// archive ("Game (World)/Game.img") resolve to nothing on disk —
/// the archive itself is the game and moves whole.
fn member_sibling_on_disk(src: &Path, evidence_json: &str) -> bool {
    let Ok(ev) = serde_json::from_str::<serde_json::Value>(evidence_json) else {
        return false;
    };
    let Some(member) = ev.get("member").and_then(|m| m.as_str()) else {
        return false;
    };
    if member.is_empty() {
        return false;
    }
    let dir = src.parent().unwrap_or_else(|| Path::new("/"));
    let member_path = Path::new(member);
    let mut candidates = vec![dir.join(member_path)];
    if member_path.is_absolute() {
        candidates.push(member_path.to_path_buf());
    }
    if let Some(name) = member_path.file_name() {
        candidates.push(dir.join(name));
    }
    candidates
        .iter()
        .any(|c| c != src && c.is_file())
}

/// Compute the organize plan for a set of files/folders under
/// `root`, from STORED ROM identities only. `systems` is the DAT
/// dataset's (key, display) list, used for platform folder names.
pub fn plan_rom_organize(
    catalog: &Catalog,
    root: &Path,
    paths: &[String],
    systems: &[(String, String)],
) -> Result<RomOrganizePlan, String> {
    let (files, truncated) = expand_inputs(paths);
    let mut actions: Vec<RomOrganizeAction> = Vec::new();
    let mut excluded: Vec<RomOrganizeExclusion> = Vec::new();
    let mut seen_targets: HashMap<String, String> = HashMap::new();
    let mut dup_targets: Vec<String> = Vec::new();

    for path in &files {
        let pstr = path.to_string_lossy().into_owned();
        let exclude = |reason: &str| RomOrganizeExclusion {
            path: pstr.clone(),
            reason: reason.into(),
        };
        let item = catalog
            .item_by_provider_key(&pstr)
            .map_err(|e| e.to_string())?;
        let Some(item) = item else {
            excluded.push(exclude("not in the catalog yet"));
            continue;
        };
        let row = catalog
            .media_identity_for(&item.id)
            .map_err(|e| e.to_string())?
            .filter(|r| r.media_kind == "rom");
        let Some(row) = row else {
            excluded.push(exclude(
                "not identified as a game — identification happens in ROM Home",
            ));
            continue;
        };
        let (Some(title), Some(platform)) = (row.title.as_deref(), row.platform.as_deref()) else {
            if row.title.is_none() {
                excluded.push(exclude(
                    "not identified yet — run identification in ROM Home first",
                ));
            } else {
                excluded.push(exclude(
                    "identified without a system — Fable can't file it yet",
                ));
            }
            continue;
        };
        if member_sibling_on_disk(path, &row.evidence_json) {
            excluded.push(exclude(
                "part of a disc set — Fable won't split a set",
            ));
            continue;
        }
        let display = ufm_media::romorg::platform_display_for(platform, systems);
        let rel = ufm_media::romorg::rom_target_rel(&display, title, &ext_of(path));
        let dst = root.join(&rel);
        if dst == *path {
            excluded.push(exclude("already in its place"));
            continue;
        }
        if std::fs::symlink_metadata(&dst).is_ok() {
            excluded.push(exclude(
                "a file already exists at the destination — skipped; Fable never overwrites",
            ));
            continue;
        }
        let key = dst.to_string_lossy().into_owned();
        if seen_targets.contains_key(&key) {
            dup_targets.push(key.clone());
        }
        seen_targets.insert(key, pstr.clone());
        actions.push(RomOrganizeAction {
            src: pstr.clone(),
            dst: dst.to_string_lossy().into_owned(),
            rel: rel.to_string_lossy().into_owned(),
        });
    }
    // Two sources wanting one destination: exclude BOTH (never pick
    // a winner silently) — same law as the media-server organize.
    if !dup_targets.is_empty() {
        let dup: std::collections::HashSet<&String> = dup_targets.iter().collect();
        let mut kept = Vec::new();
        for a in actions {
            if dup.contains(&a.dst) {
                excluded.push(RomOrganizeExclusion {
                    path: a.src.clone(),
                    reason: "another file in this set wants the same destination".into(),
                });
            } else {
                kept.push(a);
            }
        }
        actions = kept;
    }
    Ok(RomOrganizePlan {
        actions,
        excluded,
        truncated,
    })
}
