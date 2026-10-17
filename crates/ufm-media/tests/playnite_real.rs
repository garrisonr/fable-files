//! Real-library reconciliation (PC Games P1): the production
//! LiteDB reader + Playnite model, run against the actual Hyperbase
//! Playnite library copies in ~/workspace/fable-pc-games/probe/, with
//! every game's field VALUES diffed against the design phase's
//! forensic extractor oracle (games-extracted.json) — not row
//! counts: titles, sources, install dirs, exe paths, covers, scores,
//! sizes, link sets, per-collection counts, and the aggregate field
//! census from DESIGN.md §1.
//!
//! Ignored by default (needs the probe fixtures); run with:
//!   FABLE_PLAYNITE_PROBE=~/workspace/fable-pc-games/probe \
//!     cargo test -p ufm-media --test playnite_real -- --ignored

use std::collections::BTreeMap;
use std::path::PathBuf;

use ufm_media::playnite::load_library;

fn probe_dir() -> PathBuf {
    PathBuf::from(
        std::env::var("FABLE_PLAYNITE_PROBE")
            .expect("FABLE_PLAYNITE_PROBE must name the probe fixtures dir"),
    )
}

fn jstr(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(|s| s.to_string())
}

fn jarr(v: &serde_json::Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[test]
#[ignore]
fn real_library_reconciles_with_the_design_oracle() {
    let dir = probe_dir();
    let oracle: Vec<serde_json::Value> = serde_json::from_slice(
        &std::fs::read(dir.join("games-extracted.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(oracle.len(), 296, "oracle game count");

    let lib = load_library(&dir).expect("load the real library");
    assert_eq!(lib.non_game_docs, 0, "games.db holds only game docs");
    // EVIDENCE CORRECTION (2026-10-17, Playboy): the library holds
    // 392 game records, not the design phase's 296. The forensic
    // oracle anchored on BSON starts inside data pages and was
    // blind to the 96 records whose blocks live entirely on extend
    // pages (zero-length first segment) — Biomutant, Blair Witch,
    // GTA V, Hollow Knight among them. The block-table walk reads
    // them all; the media census corroborates (394 media folders =
    // 392 games + exactly 2 orphans, covers 392/392 in the listing).
    // The 296 oracle records remain a valid SUBSET oracle: every
    // value below is still diffed for them.
    assert_eq!(lib.games.len(), 392, "reader game count");

    // Per-collection document counts (DESIGN.md §1 census).
    let counts: BTreeMap<String, usize> =
        lib.collection_counts.iter().cloned().collect();
    for (name, want) in [
        ("games", 392),
        ("genres", 21),
        ("companies", 520),
        ("platforms", 66),
        ("features", 7),
        ("series", 154),
        ("ageratings", 35),
        ("completionstatuses", 8),
        ("regions", 4),
        ("sources", 0),
        ("tags", 0),
        ("categories", 0),
    ] {
        assert_eq!(
            counts.get(name).copied(),
            Some(want),
            "collection {name} count"
        );
    }

    let mine: BTreeMap<String, &ufm_media::playnite::PlayniteGame> =
        lib.games.iter().map(|g| (g.id.clone(), g)).collect();
    assert_eq!(mine.len(), 392, "game ids are unique");

    // The 96 records the oracle could not see get structural proof
    // instead of a value diff: every one is an installed game with
    // an install directory, a play action with a path, and a cover
    // that exists in the drive's own files listing.
    let listing: std::collections::HashSet<String> =
        std::fs::read_to_string(dir.join("files-listing.txt"))
            .unwrap()
            .lines()
            .map(|l| l.trim().to_string())
            .collect();
    let oracle_ids: std::collections::HashSet<String> = oracle
        .iter()
        .filter_map(|og| jstr(og, "id"))
        .collect();
    let mut unseen = 0usize;
    for g in &lib.games {
        if oracle_ids.contains(&g.id) {
            continue;
        }
        unseen += 1;
        assert!(g.is_installed, "{} installed", g.name);
        assert!(g.install_directory.is_some(), "{} has a dir", g.name);
        assert!(
            g.play_actions.iter().any(|a| a.path.is_some()),
            "{} has a play action path",
            g.name
        );
        let cover = g.cover_image.as_ref().expect("cover present");
        let want = format!("library/files/{}", cover.replace('\\', "/"));
        assert!(listing.contains(&want), "{} cover in listing", g.name);
    }
    assert_eq!(unseen, 96, "records beyond the oracle's sight");

    // The one shared folder: The Evil Within 2 is recorded twice
    // for a single install (two exe names) — 392 records stand
    // behind 391 distinct install directories.
    let dirs: std::collections::HashSet<&str> = lib
        .games
        .iter()
        .filter_map(|g| g.install_directory.as_deref())
        .collect();
    assert_eq!(dirs.len(), 391, "distinct install directories");
    let tew2: Vec<_> = lib
        .games
        .iter()
        .filter(|g| g.name == "The Evil Within 2")
        .collect();
    assert_eq!(tew2.len(), 2, "The Evil Within 2 recorded twice");

    let mut sigma_size: i64 = 0;
    let (mut n_cover, mut n_bg, mut n_desc, mut n_genre, mut n_dev, mut n_series, mut n_plat) =
        (0usize, 0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    for og in &oracle {
        let id = jstr(og, "id").expect("oracle id");
        let g = mine
            .get(&id)
            .unwrap_or_else(|| panic!("reader missed game {id}"));
        assert_eq!(g.name, jstr(og, "name").unwrap(), "{id} name");
        assert_eq!(g.game_id, jstr(og, "game_id"), "{id} game_id");
        assert_eq!(g.plugin_id, jstr(og, "plugin_id"), "{id} plugin_id");
        assert_eq!(g.source_name, jstr(og, "source"), "{id} source");
        assert_eq!(
            g.install_directory,
            jstr(og, "install_dir"),
            "{id} install_dir"
        );
        assert_eq!(
            g.is_installed,
            og.get("is_installed").and_then(|v| v.as_bool()).unwrap(),
            "{id} is_installed"
        );
        assert_eq!(g.cover_image, jstr(og, "cover"), "{id} cover");
        assert_eq!(g.background_image, jstr(og, "background"), "{id} background");
        assert_eq!(g.icon, jstr(og, "icon"), "{id} icon");
        assert_eq!(sorted(g.genres.clone()), sorted(jarr(og, "genres")), "{id} genres");
        assert_eq!(
            sorted(g.developers.clone()),
            sorted(jarr(og, "developers")),
            "{id} developers"
        );
        assert_eq!(
            sorted(g.publishers.clone()),
            sorted(jarr(og, "publishers")),
            "{id} publishers"
        );
        assert_eq!(
            sorted(g.platforms.clone()),
            sorted(jarr(og, "platforms")),
            "{id} platforms"
        );
        assert_eq!(
            sorted(g.features.clone()),
            sorted(jarr(og, "features")),
            "{id} features"
        );
        assert_eq!(sorted(g.series.clone()), sorted(jarr(og, "series")), "{id} series");
        assert_eq!(
            sorted(g.age_ratings.clone()),
            sorted(jarr(og, "age_ratings")),
            "{id} age_ratings"
        );
        assert_eq!(g.release_raw, jstr(og, "release"), "{id} release");
        assert_eq!(
            g.critic_score,
            og.get("critic").and_then(|v| v.as_i64()),
            "{id} critic"
        );
        assert_eq!(
            g.community_score,
            og.get("community").and_then(|v| v.as_i64()),
            "{id} community"
        );
        assert_eq!(
            g.user_score,
            og.get("user_score").and_then(|v| v.as_i64()),
            "{id} user_score"
        );
        assert_eq!(
            g.install_size,
            og.get("install_size").and_then(|v| v.as_i64()),
            "{id} install_size"
        );
        assert_eq!(
            g.playtime_secs,
            og.get("playtime").and_then(|v| v.as_i64()).unwrap_or(0),
            "{id} playtime"
        );
        assert_eq!(
            g.play_count,
            og.get("play_count").and_then(|v| v.as_i64()).unwrap_or(0),
            "{id} play_count"
        );
        assert_eq!(
            g.hidden,
            og.get("hidden").and_then(|v| v.as_bool()).unwrap_or(false),
            "{id} hidden"
        );
        assert_eq!(
            g.favorite,
            og.get("favorite").and_then(|v| v.as_bool()).unwrap_or(false),
            "{id} favorite"
        );
        assert_eq!(g.version, jstr(og, "version"), "{id} version");
        // Description: oracle kept only its length (characters).
        let want_len = og
            .get("description_len")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let got_len = g
            .description
            .as_deref()
            .map(|d| d.chars().count())
            .unwrap_or(0);
        assert_eq!(got_len, want_len, "{id} description length");
        // Play actions: oracle kept them whole; compare the play
        // action's path (the field the importer stands on).
        let oracle_actions = og.get("actions").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        assert_eq!(
            g.play_actions.len(),
            oracle_actions.len(),
            "{id} action count"
        );
        for (mine_a, ora_a) in g.play_actions.iter().zip(oracle_actions.iter()) {
            assert_eq!(
                mine_a.path,
                ora_a.get("Path").and_then(|v| v.as_str()).map(|s| s.to_string()),
                "{id} action path"
            );
            assert_eq!(
                mine_a.is_play_action,
                ora_a
                    .get("IsPlayAction")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                "{id} action play flag"
            );
        }
        // Links as (name, url) sets.
        let mut my_links: Vec<(String, String)> = g.links.clone();
        my_links.sort();
        let mut ora_links: Vec<(String, String)> = og
            .get("links")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|l| {
                        Some((
                            l.get("Name")?.as_str()?.to_string(),
                            l.get("Url")?.as_str()?.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        ora_links.sort();
        assert_eq!(my_links, ora_links, "{id} links");

        sigma_size += g.install_size.unwrap_or(0);
        if g.cover_image.is_some() {
            n_cover += 1;
        }
        if g.background_image.is_some() {
            n_bg += 1;
        }
        if g.description.is_some() {
            n_desc += 1;
        }
        if !g.genres.is_empty() {
            n_genre += 1;
        }
        if !g.developers.is_empty() {
            n_dev += 1;
        }
        if !g.series.is_empty() {
            n_series += 1;
        }
        if !g.platforms.is_empty() {
            n_plat += 1;
        }
    }

    // The oracle-subset census still matches the design's reading
    // of those 296 records (DESIGN.md §1).
    assert_eq!(n_cover, 296, "oracle subset with cover");
    assert_eq!(n_bg, 294, "oracle subset with background");
    assert_eq!(n_desc, 271, "oracle subset with description");
    assert_eq!(n_genre, 271, "oracle subset with genres");
    assert_eq!(n_dev, 261, "oracle subset with developers");
    assert_eq!(n_series, 140, "oracle subset with series");
    assert_eq!(n_plat, 110, "oracle subset with platforms");
    let _ = sigma_size;

    // The FULL library census, recomputed over all 392 records
    // (block-walk ground truth, cross-checked in the checkpoint).
    let full = &lib.games;
    assert_eq!(full.iter().filter(|g| g.cover_image.is_some()).count(), 392);
    assert_eq!(full.iter().filter(|g| g.background_image.is_some()).count(), 390);
    assert_eq!(full.iter().filter(|g| g.icon.is_some()).count(), 392);
    assert_eq!(full.iter().filter(|g| g.description.is_some()).count(), 365);
    assert_eq!(full.iter().filter(|g| !g.genres.is_empty()).count(), 365);
    assert_eq!(full.iter().filter(|g| !g.developers.is_empty()).count(), 346);
    assert_eq!(full.iter().filter(|g| !g.publishers.is_empty()).count(), 340);
    assert_eq!(full.iter().filter(|g| !g.series.is_empty()).count(), 188);
    assert_eq!(full.iter().filter(|g| !g.platforms.is_empty()).count(), 160);
    // Σ InstallSize over all 392 records ≈ 8.676 TB (the design's
    // 6.40 TB was the 296-record subset's sum).
    let sigma_full: i64 = full.iter().filter_map(|g| g.install_size).sum();
    let tb = sigma_full as f64 / 1e12;
    assert!((8.67..8.68).contains(&tb), "Σ install size {tb} TB");
}
