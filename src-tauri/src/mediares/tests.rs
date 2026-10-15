//! mediares tests: TMDB JSON mapping against REAL captured response
//! shapes (research/tmdb-*.json, fetched live 2026-10-15; values
//! below are the real fields, trimmed to one result), and the
//! scan → confirm → organize-plan flow over an in-memory catalog and
//! a temp folder. No network, no keyring.

use super::*;
use ufm_core::{ItemId, LocationId};
use ufm_vfs::ItemKind;

// ---------------------------------------------------------------------
// TMDB JSON mapping (real response shapes)
// ---------------------------------------------------------------------

#[test]
fn tmdb_json_mapping_real_shapes() {
    let movie: serde_json::Value = serde_json::from_str(
        r#"{"page":1,"results":[{"adult":false,"genre_ids":[878],"id":693134,
        "original_language":"en","original_title":"Dune: Part Two",
        "popularity":83.2501,"release_date":"2024-02-27",
        "title":"Dune: Part Two","video":false,"vote_average":8.1,
        "vote_count":8735}],"total_pages":1,"total_results":1}"#,
    )
    .unwrap();
    let cands = candidates_from_search(&movie, "movie");
    assert_eq!(cands.len(), 1);
    assert_eq!(cands[0].id, 693134);
    assert_eq!(cands[0].title, "Dune: Part Two");
    assert_eq!(cands[0].year, Some(2024));

    let tv: serde_json::Value = serde_json::from_str(
        r#"{"page":1,"results":[{"adult":false,"first_air_date":"2022-02-17",
        "genre_ids":[10765],"id":95396,"name":"Severance",
        "origin_country":["US"],"original_language":"en",
        "original_name":"Severance","popularity":79.5009,
        "vote_average":8.4,"vote_count":2000}],"total_pages":1,
        "total_results":1}"#,
    )
    .unwrap();
    let cands = candidates_from_search(&tv, "tv");
    assert_eq!(cands.len(), 1);
    assert_eq!(cands[0].id, 95396);
    assert_eq!(cands[0].title, "Severance");
    assert_eq!(cands[0].year, Some(2022));

    let season: serde_json::Value = serde_json::from_str(
        r#"{"episodes":[
          {"air_date":"2022-02-17","episode_number":1,"id":1,
           "name":"Good News About Hell","season_number":1},
          {"air_date":"2022-02-17","episode_number":2,"id":2,
           "name":"Half Loop","season_number":1},
          {"air_date":"2022-02-24","episode_number":3,"id":3,
           "name":"In Perpetuity","season_number":1}],
          "season_number":1}"#,
    )
    .unwrap();
    let eps = episodes_from_season(&season);
    assert_eq!(eps.len(), 3);
    assert_eq!(eps[0].name, "Good News About Hell");
    assert_eq!(eps[2].episode_number, 3);

    // Garbage in → empty out, never a panic.
    assert!(candidates_from_search(&serde_json::json!({}), "movie").is_empty());
    assert!(episodes_from_season(&serde_json::json!([])).is_empty());
}

// ---------------------------------------------------------------------
// Scan / confirm / plan over fixtures
// ---------------------------------------------------------------------

fn fixture() -> (Catalog, PathBuf, LocationId) {
    let dir = std::env::temp_dir().join(format!(
        "fable-mediares-test-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cat = Catalog::open_in_memory().unwrap();
    let loc = LocationId::new();
    cat.insert_location(&ufm_vfs::Location {
        id: loc,
        provider: "local".into(),
        display_name: "test".into(),
        capabilities: ufm_vfs::Capabilities {
            read: true,
            ..Default::default()
        },
        protected: ufm_vfs::ProtectionTier::None,
    })
    .unwrap();
    (cat, dir, loc)
}

fn add_file(cat: &Catalog, loc: LocationId, dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, b"x").unwrap();
    cat.insert_item(&ufm_vfs::Item {
        id: ItemId::new(),
        location: loc,
        name: name.into(),
        kind: ItemKind::File,
        size_bytes: Some(1),
        modified: None,
        provider_key: Some(path.to_string_lossy().into_owned()),
        labels: Vec::new(),
        starred: false,
    })
    .unwrap();
    path
}

#[test]
fn scan_keyless_judges_and_confirm_unlocks_organize() {
    let (cat, dir, loc) = fixture();
    let ep = add_file(&cat, loc, &dir, "Severance.S01E01.1080p.WEB.mkv");
    let mv = add_file(&cat, loc, &dir, "Dune.Part.Two.2024.1080p.mkv");
    let unk = add_file(&cat, loc, &dir, "Random.Thing.mkv");
    let paths: Vec<String> = [&ep, &mv, &unk]
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();

    // Keyless scan: complete parses get name-derived identities and
    // canonical targets; the no-year file needs attention.
    let res = scan(&cat, &dir, &paths, None).unwrap();
    assert!(!res.has_key);
    let row = |p: &PathBuf| {
        res.rows
            .iter()
            .find(|r| r.path == p.to_string_lossy())
            .unwrap()
    };
    let ep_row = row(&ep);
    assert_eq!(ep_row.state, "fixable");
    assert_eq!(ep_row.identity.as_ref().unwrap().source, "name");
    assert_eq!(
        ep_row.target_rel.as_deref(),
        Some("Severance/Season 01/Severance - S01E01.mkv")
    );
    assert_eq!(row(&mv).state, "fixable");
    assert_eq!(
        row(&mv).target_rel.as_deref(),
        Some("Dune Part Two (2024)/Dune Part Two (2024).mkv")
    );
    assert_eq!(row(&unk).state, "attention");

    // Name-derived identities are NOT organize-eligible: the plan
    // excludes everything until a human confirms.
    let plan = plan_organize(&cat, &dir, &paths).unwrap();
    assert!(plan.actions.is_empty());
    assert_eq!(plan.excluded.len(), 3);

    // Confirm the episode by hand (the Correct path) → eligible.
    let idn = confirm(
        &cat,
        &ep,
        &ConfirmChoice {
            media_type: "episode".into(),
            tmdb_id: Some(95396),
            title: "Severance".into(),
            year: Some(2022),
            season: Some(1),
            episode: Some(1),
            episode_end: Some(1),
            episode_titles: vec!["Good News About Hell".into()],
        },
    )
    .unwrap();
    assert!(idn.confirmed);
    let plan = plan_organize(&cat, &dir, &paths).unwrap();
    assert_eq!(plan.actions.len(), 1);
    assert!(plan.actions[0]
        .dst
        .ends_with("Severance (2022)/Season 01/Severance (2022) - S01E01 - Good News About Hell.mkv"));

    // Collision law: an existing destination excludes, never overwrites.
    let dst = PathBuf::from(&plan.actions[0].dst);
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::write(&dst, b"existing").unwrap();
    let plan = plan_organize(&cat, &dir, &paths).unwrap();
    assert!(plan.actions.is_empty());
    assert!(plan
        .excluded
        .iter()
        .any(|e| e.reason.contains("never overwrites")));

    // Confirm for a file outside the catalog is a plain error.
    let ghost = dir.join("Ghost.2024.mkv");
    std::fs::write(&ghost, b"x").unwrap();
    assert!(confirm(
        &cat,
        &ghost,
        &ConfirmChoice {
            media_type: "movie".into(),
            tmdb_id: None,
            title: "Ghost".into(),
            year: Some(2024),
            season: None,
            episode: None,
            episode_end: None,
            episode_titles: vec![],
        },
    )
    .is_err());
}

#[test]
fn tmdb_art_mapping_real_captured_values() {
    // poster_path / backdrop_path values are the REAL ones from the
    // live captures in research/ (Dune: Part Two movie search,
    // Severance TV search + season) — the same fields the details
    // responses carry.
    let movie_details: serde_json::Value = serde_json::from_str(
        r#"{"id":693134,"title":"Dune: Part Two",
        "poster_path":"/6izwz7rsy95ARzTR3poZ8H6c5pp.jpg",
        "backdrop_path":"/eZ239CUp1d6OryZEBPnO2n87gMG.jpg"}"#,
    )
    .unwrap();
    let (poster, backdrop) = art_from_details(&movie_details);
    assert_eq!(poster.as_deref(), Some("/6izwz7rsy95ARzTR3poZ8H6c5pp.jpg"));
    assert_eq!(backdrop.as_deref(), Some("/eZ239CUp1d6OryZEBPnO2n87gMG.jpg"));

    let season: serde_json::Value = serde_json::from_str(
        r#"{"season_number":1,"poster_path":"/lFf6LLrQjYldcZItzOkGmMMigP7.jpg","episodes":[]}"#,
    )
    .unwrap();
    assert_eq!(
        poster_from_season(&season).as_deref(),
        Some("/lFf6LLrQjYldcZItzOkGmMMigP7.jpg")
    );

    // Missing/null art stays None — never an invented path.
    let bare: serde_json::Value = serde_json::from_str(r#"{"id":1,"poster_path":null}"#).unwrap();
    assert_eq!(art_from_details(&bare), (None, None));
    assert_eq!(poster_from_season(&bare), None);

    assert_eq!(
        tmdb_image_url("w500", "/6izwz7rsy95ARzTR3poZ8H6c5pp.jpg"),
        "https://image.tmdb.org/t/p/w500/6izwz7rsy95ARzTR3poZ8H6c5pp.jpg"
    );
}

#[test]
fn expand_inputs_walks_directories() {
    let (_cat, dir, _loc) = fixture();
    let sub = dir.join("Season 01");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("Show.S01E01.mkv"), b"x").unwrap();
    std::fs::write(dir.join("notes.txt"), b"x").unwrap();
    let (files, truncated) = expand_inputs(&[dir.to_string_lossy().into_owned()]);
    assert!(!truncated);
    assert_eq!(files.len(), 1);
    assert!(files[0].ends_with("Show.S01E01.mkv"));
}
