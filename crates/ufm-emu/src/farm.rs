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
    /// The files of a multi-file game set this copy belongs to
    /// (amendment §5.3 — bin/cue pairs, cue sheets with their
    /// tracks, .ccd+.img+.sub triples), as absolute paths,
    /// INCLUDING `path` itself, when the glue's scan found more
    /// than the launch file alone. Empty = a single-file game.
    /// The farm represents a set as a FOLDER named for the launch
    /// file, with one link per member inside it.
    #[serde(default)]
    pub set_files: Vec<String>,
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

fn basename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// One game's planned view presence: the name it takes in the
/// farm (after the collision pass) and whether that name is a
/// FOLDER (a multi-file set) or a link (a single file). Computed
/// once and shared by the link set and the multi-disc playlists,
/// so an .m3u's lines always reference the names that exist.
struct PlannedGame {
    identity: String,
    system: String,
    /// The launch file's real path (the best present copy).
    target: String,
    /// Set members (absolute paths incl. `target`); empty when
    /// the game is a single file.
    set: Vec<String>,
    /// "{title}.{ext}" — the farm entry's name.
    view_name: String,
}

impl PlannedGame {
    fn is_set(&self) -> bool {
        !self.set.is_empty()
    }
}

fn plan(games: &[FarmGame]) -> Vec<PlannedGame> {
    let mut out: Vec<PlannedGame> = Vec::new();
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
        // A set only counts when the scan actually found the
        // launch file among the members; otherwise treat the copy
        // as the single file it provably is.
        let set = if copy.set_files.len() >= 2 && copy.set_files.iter().any(|f| f == &copy.path) {
            copy.set_files.clone()
        } else {
            Vec::new()
        };
        out.push(PlannedGame {
            identity: game.identity.clone(),
            system: game.system.clone(),
            target: copy.path.clone(),
            set,
            view_name: file,
        });
    }
    // Name collisions between DIFFERENT identities in one system
    // (two dumps sharing a DAT name): the first identity (sorted)
    // keeps the clean name; the rest take a stable hash suffix.
    out.sort_by(|a, b| {
        format!("{}/{}", a.system, a.view_name)
            .cmp(&format!("{}/{}", b.system, b.view_name))
            .then_with(|| a.identity.cmp(&b.identity))
    });
    let mut i = 0;
    while i < out.len() {
        let mut j = i + 1;
        while j < out.len()
            && out[j].system == out[i].system
            && out[j].view_name == out[i].view_name
        {
            j += 1;
        }
        if j - i > 1 {
            for planned in out.iter_mut().take(j).skip(i + 1) {
                let suffix = format!(" [{:08x}]", fnv1a(&planned.identity));
                if let Some(dot) = planned.view_name.rfind('.') {
                    planned.view_name = format!(
                        "{}{}{}",
                        &planned.view_name[..dot],
                        suffix,
                        &planned.view_name[dot..]
                    );
                } else {
                    planned.view_name = format!("{}{}", planned.view_name, suffix);
                }
            }
        }
        i = j;
    }
    out
}

/// The desired link set. Identities with no present copy produce
/// NO link (their old link, if any, retires via the delta) — the
/// view only ever shows what can actually be opened. A multi-file
/// set (§5.3) expands to one link per member inside a folder named
/// for the launch file; inner links carry the members' REAL base
/// names, so a cue sheet's relative FILE references resolve
/// within the folder exactly as they do at home.
pub fn desired_links(games: &[FarmGame]) -> Vec<DesiredLink> {
    let mut out: Vec<DesiredLink> = Vec::new();
    for planned in plan(games) {
        if planned.is_set() {
            for member in &planned.set {
                out.push(DesiredLink {
                    rel: format!(
                        "{}/{}/{}",
                        planned.system,
                        planned.view_name,
                        basename(member)
                    ),
                    target: member.clone(),
                    identity: planned.identity.clone(),
                    system: planned.system.clone(),
                });
            }
        } else {
            out.push(DesiredLink {
                rel: format!("{}/{}", planned.system, planned.view_name),
                target: planned.target.clone(),
                identity: planned.identity.clone(),
                system: planned.system.clone(),
            });
        }
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

/// The disc marker in a DAT title: "Game (USA) (Disc 2)" →
/// ("Game (USA)", 2); "(Disk 1 of 2)" reads the same. None when
/// the title carries no disc marker.
pub fn disc_base_title(title: &str) -> Option<(String, u32)> {
    let lower = title.to_lowercase();
    for marker in ["(disc ", "(disk "] {
        let Some(pos) = lower.find(marker) else {
            continue;
        };
        let num_start = pos + marker.len();
        let rest = &title[num_start..];
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            continue;
        }
        let after = &rest[digits.len()..];
        let closes = after.starts_with(')') || {
            // "(Disc 1 of 2)": skip " of N" and require the close.
            after
                .strip_prefix(" of ")
                .map(|tail| {
                    let d: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
                    !d.is_empty() && tail[d.len()..].starts_with(')')
                })
                .unwrap_or(false)
        };
        if !closes {
            continue;
        }
        let close = title[pos..].find(')')? + pos + 1;
        let joined = format!("{}{}", &title[..pos], &title[close..]);
        let base = joined.split_whitespace().collect::<Vec<_>>().join(" ");
        if base.is_empty() {
            return None;
        }
        return Some((base, digits.parse().ok()?));
    }
    None
}

/// A desired multi-disc playlist file (§5.3): one per (system,
/// disc-base title) group with ≥2 distinct discs present. `lines`
/// are farm-relative entry names (folders for sets, link names
/// for single files) — the glue writes them verbatim after the
/// header and recognizes its own files by a marker comment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesiredM3u {
    /// Path relative to the view root ("psx/Game (USA).m3u").
    pub rel: String,
    pub system: String,
    /// The base title — the playlist entry's label.
    pub title: String,
    pub lines: Vec<String>,
    /// Member identities (the playlist layer suppresses their
    /// individual entries in favor of this one handle).
    pub identities: Vec<String>,
}

pub fn desired_m3us(games: &[FarmGame]) -> Vec<DesiredM3u> {
    use std::collections::BTreeMap;
    let planned = plan(games);
    let by_identity: std::collections::HashMap<&str, &PlannedGame> = planned
        .iter()
        .map(|p| (p.identity.as_str(), p))
        .collect();
    // (system, base) → [(disc no, identity)]
    let mut groups: BTreeMap<(String, String), Vec<(u32, String)>> = BTreeMap::new();
    for game in games {
        let Some((base, disc)) = disc_base_title(&game.title) else {
            continue;
        };
        if !by_identity.contains_key(game.identity.as_str()) {
            continue; // no present copy → no line → doesn't count
        }
        groups
            .entry((game.system.clone(), base))
            .or_default()
            .push((disc, game.identity.clone()));
    }
    let mut out: Vec<DesiredM3u> = Vec::new();
    for ((system, base), mut members) in groups {
        let distinct: std::collections::HashSet<u32> =
            members.iter().map(|(d, _)| *d).collect();
        if distinct.len() < 2 {
            continue; // a lone disc gets no playlist file
        }
        members.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let mut identities = Vec::new();
        let mut lines = Vec::new();
        for (_, identity) in &members {
            let p = by_identity[identity.as_str()];
            identities.push(identity.clone());
            lines.push(if p.is_set() {
                format!("{}/{}", p.view_name, basename(&p.target))
            } else {
                p.view_name.clone()
            });
        }
        out.push(DesiredM3u {
            rel: format!("{system}/{base}.m3u"),
            system,
            title: base,
            lines,
            identities,
        });
    }
    // Two groups in one system could share a base (different
    // editions) — keep rels unique with the same stable suffix.
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for m in &mut out {
        if !seen.insert(m.rel.clone()) {
            let suffix = format!(" [{:08x}]", fnv1a(&m.identities.join("+")));
            let stem = m.rel.strip_suffix(".m3u").unwrap_or(&m.rel).to_string();
            m.rel = format!("{stem}{suffix}.m3u");
            seen.insert(m.rel.clone());
        }
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

/// One multi-disc playlist reconciliation step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "op")]
pub enum M3uOp {
    /// Create or rewrite the file with exactly these disc lines.
    Write { rel: String, lines: Vec<String> },
    Remove { rel: String },
}

/// Diff desired .m3u files against the farm's actual ones. The
/// glue supplies ONLY files it recognizes as Fable-managed (the
/// marker comment) — a foreign file squatting on a desired name
/// makes the glue report the m3u blocked, never overwritten.
pub fn m3u_delta(desired: &[DesiredM3u], actual: &[(String, Vec<String>)]) -> Vec<M3uOp> {
    use std::collections::HashMap;
    let actual_map: HashMap<&str, &Vec<String>> = actual
        .iter()
        .map(|(rel, lines)| (rel.as_str(), lines))
        .collect();
    let mut ops = Vec::new();
    for m in desired {
        match actual_map.get(m.rel.as_str()) {
            Some(lines) if *lines == &m.lines => {}
            _ => ops.push(M3uOp::Write {
                rel: m.rel.clone(),
                lines: m.lines.clone(),
            }),
        }
    }
    for (rel, _) in actual {
        if !desired.iter().any(|m| &m.rel == rel) {
            ops.push(M3uOp::Remove { rel: rel.clone() });
        }
    }
    ops.sort_by(|a, b| m3u_op_rel(a).cmp(m3u_op_rel(b)));
    ops
}

fn m3u_op_rel(op: &M3uOp) -> &str {
    match op {
        M3uOp::Write { rel, .. } | M3uOp::Remove { rel } => rel,
    }
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
                    set_files: Vec::new(),
                })
                .collect(),
        }
    }

    fn set_game(
        system: &str,
        id: &str,
        title: &str,
        launch: &str,
        members: &[&str],
    ) -> FarmGame {
        FarmGame {
            system: system.to_string(),
            identity: id.to_string(),
            title: title.to_string(),
            copies: vec![FarmCopy {
                path: launch.to_string(),
                present: true,
                rank: 0,
                set_files: members.iter().map(|s| s.to_string()).collect(),
            }],
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
    fn sets_expand_into_a_folder_of_inner_links() {
        let games = vec![set_game(
            "psx",
            "psx|set1",
            "Bin Cue Game (USA)",
            "/roms/Bin Cue Game (USA).cue",
            &["/roms/Bin Cue Game (USA).cue", "/roms/Bin Cue Game (USA).bin"],
        )];
        let links = desired_links(&games);
        assert_eq!(links.len(), 2);
        // Folder named for the LAUNCH file (DAT title + ext); inner
        // links keep the members' real names so the cue's relative
        // FILE lines resolve inside the folder.
        assert_eq!(links[0].rel, "psx/Bin Cue Game (USA).cue/Bin Cue Game (USA).bin");
        assert_eq!(links[1].rel, "psx/Bin Cue Game (USA).cue/Bin Cue Game (USA).cue");
        assert_eq!(links[1].target, "/roms/Bin Cue Game (USA).cue");
    }

    #[test]
    fn a_set_whose_launch_file_is_missing_is_not_a_set() {
        // Defensive: a stale set list that doesn't contain the copy
        // itself degrades to the plain single-file link.
        let mut g = set_game("psx", "psx|odd", "Odd", "/roms/odd.cue", &["/roms/a.bin", "/roms/b.bin"]);
        g.copies[0].set_files.retain(|f| f != "/roms/odd.cue");
        let links = desired_links(&[g]);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].rel, "psx/Odd.cue");
    }

    #[test]
    fn disc_markers_parse() {
        assert_eq!(
            disc_base_title("Game (USA) (Disc 2)"),
            Some(("Game (USA)".to_string(), 2))
        );
        assert_eq!(
            disc_base_title("Game (Disk 1 of 3)"),
            Some(("Game".to_string(), 1))
        );
        assert_eq!(
            disc_base_title("Game (Disc 1) (Rev 1)"),
            Some(("Game (Rev 1)".to_string(), 1))
        );
        assert_eq!(disc_base_title("Discworld"), None);
        assert_eq!(disc_base_title("Game (Disc)"), None);
        assert_eq!(disc_base_title("Game (Disc 2"), None);
        assert_eq!(disc_base_title("Just A Game"), None);
    }

    #[test]
    fn multi_disc_groups_get_one_m3u_with_final_names() {
        let games = vec![
            set_game(
                "psx",
                "psx|d1",
                "Saga (USA) (Disc 1)",
                "/roms/Saga (USA) (Disc 1).cue",
                &["/roms/Saga (USA) (Disc 1).cue", "/roms/Saga (USA) (Disc 1).bin"],
            ),
            game(
                "psx",
                "psx|d2",
                "Saga (USA) (Disc 2)",
                vec![("/roms/Saga (USA) (Disc 2).chd", true, 0)],
            ),
            game(
                "psx",
                "psx|solo",
                "Solo (USA) (Disc 1)",
                vec![("/roms/solo.chd", true, 0)],
            ),
        ];
        let m3us = desired_m3us(&games);
        assert_eq!(m3us.len(), 1); // the lone "Solo" disc gets none
        let m = &m3us[0];
        assert_eq!(m.rel, "psx/Saga (USA).m3u");
        assert_eq!(m.title, "Saga (USA)");
        // Disc 1 is a SET: its line points INTO the folder at the
        // launch file's real name. Disc 2 is a single link name.
        assert_eq!(
            m.lines,
            vec![
                "Saga (USA) (Disc 1).cue/Saga (USA) (Disc 1).cue".to_string(),
                "Saga (USA) (Disc 2).chd".to_string(),
            ]
        );
        assert_eq!(m.identities, vec!["psx|d1".to_string(), "psx|d2".to_string()]);

        // Delta: fresh write, steady state, and retire.
        let actual_none: Vec<(String, Vec<String>)> = Vec::new();
        let ops = m3u_delta(&m3us, &actual_none);
        assert_eq!(
            ops,
            vec![M3uOp::Write {
                rel: m.rel.clone(),
                lines: m.lines.clone()
            }]
        );
        let actual_same = vec![(m.rel.clone(), m.lines.clone())];
        assert!(m3u_delta(&m3us, &actual_same).is_empty());
        assert_eq!(
            m3u_delta(&[], &actual_same),
            vec![M3uOp::Remove { rel: m.rel.clone() }]
        );
    }

    #[test]
    fn set_retires_cleanly_when_its_drive_unplugs() {
        let g = set_game(
            "psx",
            "psx|set1",
            "Bin Cue Game (USA)",
            "/roms/Bin Cue Game (USA).cue",
            &["/roms/Bin Cue Game (USA).cue", "/roms/Bin Cue Game (USA).bin"],
        );
        let d1 = desired_links(std::slice::from_ref(&g));
        let mut gone = g.clone();
        gone.copies[0].present = false;
        let d2 = desired_links(&[gone]);
        assert!(d2.is_empty());
        let actual: Vec<(String, String)> = d1
            .iter()
            .map(|l| (l.rel.clone(), l.target.clone()))
            .collect();
        let ops = farm_delta(&d2, &actual);
        assert_eq!(ops.len(), 2);
        assert!(ops.iter().all(|o| matches!(o, FarmOp::Remove { .. })));
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
