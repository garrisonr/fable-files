//! The unified view's mathematics (amendment §5): desired links
//! from catalog identities, and the delta against what's actually
//! in the farm — create / repoint / retire, never wholesale
//! rebuild, never a dangling link.
//!
//! One link per identity, pointing at the best PRESENT copy.
//! Presence and rank are computed by the app crate from Library
//! Memory's live mount set (internal storage first, then
//! non-removable volumes, then the rest); this module is pure.

use serde::{Deserialize, Serialize};

/// One physical copy of a game, as the catalog knows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FarmCopy {
    /// Absolute real path.
    pub path: String,
    /// Its volume is mounted right now.
    pub present: bool,
    /// Lower wins among present copies (volume preference).
    pub rank: u32,
}

/// One catalog identity and every copy Fable knows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FarmGame {
    /// ES-DE system id (= farm folder name).
    pub system: String,
    /// Stable identity key (platform + sha1/title) — the dedup
    /// truth the shelves use, carried through.
    pub identity: String,
    /// DAT title — the link's display name.
    pub title: String,
    pub copies: Vec<FarmCopy>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesiredLink {
    /// Path relative to the view root ("nes/1942 (Japan, USA).nes").
    pub rel: String,
    /// Absolute real path the link points at.
    pub target: String,
    pub identity: String,
    pub system: String,
}

/// FNV-1a (32-bit) — a tiny deterministic hash for collision
/// suffixes; stability matters, cryptography doesn't.
fn fnv1a(text: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in text.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

fn extension_of(path: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or(path);
    match base.rfind('.') {
        Some(i) if i > 0 => base[i + 1..].to_string(),
        _ => String::new(),
    }
}

/// The best present copy of a game, if any copy is present.
fn best_copy(game: &FarmGame) -> Option<&FarmCopy> {
    game.copies
        .iter()
        .filter(|c| c.present)
        .min_by(|a, b| a.rank.cmp(&b.rank).then_with(|| a.path.cmp(&b.path)))
}

/// The desired link set. Identities with no present copy produce
/// NO link (their old link, if any, retires via the delta) — the
/// view only ever shows what can actually be opened.
pub fn desired_links(games: &[FarmGame]) -> Vec<DesiredLink> {
    let mut out: Vec<DesiredLink> = Vec::new();
    for game in games {
        let Some(copy) = best_copy(game) else {
            continue;
        };
        let title = game.title.replace('/', "-");
        let ext = extension_of(&copy.path);
        let file = if ext.is_empty() {
            title.clone()
        } else {
            format!("{title}.{ext}")
        };
        out.push(DesiredLink {
            rel: format!("{}/{}", game.system, file),
            target: copy.path.clone(),
            identity: game.identity.clone(),
            system: game.system.clone(),
        });
    }
    // Name collisions between DIFFERENT identities in one system
    // (two dumps sharing a DAT name): the first identity (sorted)
    // keeps the clean name; the rest take a stable hash suffix.
    out.sort_by(|a, b| a.rel.cmp(&b.rel).then_with(|| a.identity.cmp(&b.identity)));
    let mut i = 0;
    while i < out.len() {
        let mut j = i + 1;
        while j < out.len() && out[j].rel == out[i].rel {
            j += 1;
        }
        if j - i > 1 {
            for link in out.iter_mut().take(j).skip(i + 1) {
                let suffix = format!(" [{:08x}]", fnv1a(&link.identity));
                if let Some(dot) = link.rel.rfind('.') {
                    link.rel = format!("{}{}{}", &link.rel[..dot], suffix, &link.rel[dot..]);
                } else {
                    link.rel = format!("{}{}", link.rel, suffix);
                }
            }
        }
        i = j;
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

/// One reconciliation step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "op")]
pub enum FarmOp {
    Create { rel: String, target: String },
    Repoint { rel: String, target: String },
    Remove { rel: String },
}

/// Diff the desired set against the farm's actual symlinks
/// (`actual` = (rel, current target) for links that exist; the glue
/// supplies ONLY symlinks — real files a user dropped in the view
/// are never touched, and a desired link blocked by one is reported
/// by the glue, not forced).
pub fn farm_delta(desired: &[DesiredLink], actual: &[(String, String)]) -> Vec<FarmOp> {
    use std::collections::HashMap;
    let actual_map: HashMap<&str, &str> = actual
        .iter()
        .map(|(rel, target)| (rel.as_str(), target.as_str()))
        .collect();
    let mut ops = Vec::new();
    let mut desired_rels: Vec<&str> = Vec::with_capacity(desired.len());
    for link in desired {
        desired_rels.push(link.rel.as_str());
        match actual_map.get(link.rel.as_str()) {
            None => ops.push(FarmOp::Create {
                rel: link.rel.clone(),
                target: link.target.clone(),
            }),
            Some(cur) if *cur != link.target => ops.push(FarmOp::Repoint {
                rel: link.rel.clone(),
                target: link.target.clone(),
            }),
            _ => {}
        }
    }
    for (rel, _) in actual {
        if !desired_rels.contains(&rel.as_str()) {
            ops.push(FarmOp::Remove { rel: rel.clone() });
        }
    }
    ops.sort_by(|a, b| op_rel(a).cmp(op_rel(b)));
    ops
}

fn op_rel(op: &FarmOp) -> &str {
    match op {
        FarmOp::Create { rel, .. } | FarmOp::Repoint { rel, .. } | FarmOp::Remove { rel } => rel,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(system: &str, id: &str, title: &str, copies: Vec<(&str, bool, u32)>) -> FarmGame {
        FarmGame {
            system: system.to_string(),
            identity: id.to_string(),
            title: title.to_string(),
            copies: copies
                .into_iter()
                .map(|(path, present, rank)| FarmCopy {
                    path: path.to_string(),
                    present,
                    rank,
                })
                .collect(),
        }
    }

    #[test]
    fn best_present_copy_wins_and_absent_games_get_no_link() {
        let games = vec![
            game(
                "nes",
                "nes|aaa",
                "1942 (Japan, USA)",
                vec![
                    ("/media/Desk SSD/Games/1942.nes", false, 10), // unplugged
                    ("/home/richard/Games/1942.nes", true, 0),       // internal
                ],
            ),
            game(
                "nes",
                "nes|bbb",
                "Ghost Game",
                vec![("/media/Desk SSD/Games/ghost.nes", false, 10)],
            ),
        ];
        let links = desired_links(&games);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].rel, "nes/1942 (Japan, USA).nes");
        assert_eq!(links[0].target, "/home/richard/Games/1942.nes");
    }

    #[test]
    fn unplug_repoints_then_retires_never_dangles() {
        // Both drives plugged in: link sits on the internal copy.
        let games = vec![game(
            "snes",
            "snes|ccc",
            "ActRaiser (USA)",
            vec![
                ("/home/richard/SNES/ActRaiser.zip", true, 0),
                ("/media/Desk SSD/SNES/ActRaiser.zip", true, 10),
            ],
        )];
        let d1 = desired_links(&games);
        let actual0: Vec<(String, String)> = Vec::new();
        let ops = farm_delta(&d1, &actual0);
        assert_eq!(ops.len(), 1);
        assert!(matches!(ops[0], FarmOp::Create { .. }));

        // Internal drive's copy vanishes (deleted): repoint to the
        // surviving Desk SSD copy.
        let games2 = vec![game(
            "snes",
            "snes|ccc",
            "ActRaiser (USA)",
            vec![("/media/Desk SSD/SNES/ActRaiser.zip", true, 10)],
        )];
        let d2 = desired_links(&games2);
        let actual1 = vec![(d1[0].rel.clone(), d1[0].target.clone())];
        let ops2 = farm_delta(&d2, &actual1);
        assert_eq!(
            ops2,
            vec![FarmOp::Repoint {
                rel: d1[0].rel.clone(),
                target: "/media/Desk SSD/SNES/ActRaiser.zip".to_string()
            }]
        );

        // Desk SSD unplugs too: the link retires. No dangling.
        let games3 = vec![game(
            "snes",
            "snes|ccc",
            "ActRaiser (USA)",
            vec![("/media/Desk SSD/SNES/ActRaiser.zip", false, 10)],
        )];
        let d3 = desired_links(&games3);
        assert!(d3.is_empty());
        let actual2 = vec![(d1[0].rel.clone(), "/media/Desk SSD/SNES/ActRaiser.zip".to_string())];
        let ops3 = farm_delta(&d3, &actual2);
        assert_eq!(
            ops3,
            vec![FarmOp::Remove {
                rel: d1[0].rel.clone()
            }]
        );

        // Steady state: no ops.
        assert!(farm_delta(&d2, &actual2).is_empty());
    }

    #[test]
    fn name_collisions_get_stable_suffixes() {
        let games = vec![
            game("nes", "nes|one", "Same Name", vec![("/a/same.nes", true, 0)]),
            game("nes", "nes|two", "Same Name", vec![("/b/same.nes", true, 0)]),
        ];
        let links = desired_links(&games);
        assert_eq!(links.len(), 2);
        assert_ne!(links[0].rel, links[1].rel);
        assert!(links.iter().any(|l| l.rel == "nes/Same Name.nes"));
        assert!(links
            .iter()
            .any(|l| l.rel.starts_with("nes/Same Name [") && l.rel.ends_with("].nes")));
        // Deterministic across runs.
        assert_eq!(links, desired_links(&games));
    }
}
