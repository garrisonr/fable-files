//! musicid tests — the MusicBrainz fixtures are built to the real
//! response shapes (recording search + recording lookup with
//! releases/media), captured in structure from musicbrainz.org's
//! ws/2 API documentation and live-response anatomy.

use super::*;

/// A recording-search response for Radiohead's "Creep", with two
/// releases on the recording (a later compilation + the original
/// album) so release picking has something to decide.
fn search_fixture() -> serde_json::Value {
    serde_json::json!({
        "created": "2026-10-15T00:00:00.000Z",
        "count": 1,
        "offset": 0,
        "recordings": [{
            "id": "a0e3240f-4b2f-4a4e-9a3f-example00001",
            "score": 100,
            "title": "Creep",
            "length": 238000,
            "artist-credit": [{
                "name": "Radiohead",
                "artist": {"id": "a74b1b7f-71a5-4011-9441-d0b5e4122711", "name": "Radiohead"},
                "joinphrase": ""
            }],
            "releases": [
                {
                    "id": "rel-comp-0001",
                    "title": "Now That's What I Call Music! (compilation)",
                    "date": "2005-11-08",
                    "status": "Official",
                    "release-group": {"id": "rg-comp", "title": "Now", "primary-type": "Album", "secondary-types": ["Compilation"]}
                },
                {
                    "id": "rel-pablo-0002",
                    "title": "Pablo Honey",
                    "date": "1993-02-22",
                    "status": "Official",
                    "release-group": {"id": "rg-pablo", "title": "Pablo Honey", "primary-type": "Album"}
                }
            ],
            "tags": [
                {"count": 5, "name": "alternative rock"},
                {"count": 2, "name": "rock"}
            ]
        }]
    })
}

/// The recording lookup (`inc=releases+genres`) for the same
/// recording: Pablo Honey's media listing puts Creep at track 2.
fn lookup_fixture() -> serde_json::Value {
    serde_json::json!({
        "id": "a0e3240f-4b2f-4a4e-9a3f-example00001",
        "title": "Creep",
        "length": 238000,
        "genres": [
            {"id": "g1", "name": "alternative rock", "count": 4},
            {"id": "g2", "name": "rock", "count": 1}
        ],
        "releases": [{
            "id": "rel-pablo-0002",
            "title": "Pablo Honey",
            "date": "1993-02-22",
            "status": "Official",
            "release-group": {"id": "rg-pablo", "title": "Pablo Honey", "primary-type": "Album"},
            "media": [{
                "format": "CD",
                "track-count": 12,
                "discs": [],
                "tracks": [
                    {"id": "t1", "number": "1", "position": 1, "title": "You", "length": 209000,
                     "recording": {"id": "rec-you-00000001", "title": "You"}},
                    {"id": "t2", "number": "2", "position": 2, "title": "Creep", "length": 238000,
                     "recording": {"id": "a0e3240f-4b2f-4a4e-9a3f-example00001", "title": "Creep"}}
                ]
            }]
        }]
    })
}

#[test]
fn maps_search_response_and_picks_the_album() {
    let cands = map_search_response(&search_fixture());
    assert_eq!(cands.len(), 1);
    let c = &cands[0];
    assert_eq!(c.title, "Creep");
    assert_eq!(c.artist, "Radiohead");
    assert_eq!(c.mb_score, 100);
    assert_eq!(c.duration_ms, Some(238_000));
    // Both releases are Official Albums; the earlier date wins, so
    // the completion describes Pablo Honey, not the compilation.
    assert_eq!(c.release_id.as_deref(), Some("rel-pablo-0002"));
    assert_eq!(c.album.as_deref(), Some("Pablo Honey"));
    assert_eq!(c.year, Some(1993));
    // Genre from search tags, title-cased.
    assert_eq!(c.genre.as_deref(), Some("Alternative Rock"));
}

#[test]
fn lookup_merge_fills_track_and_genre() {
    let mut c = map_search_response(&search_fixture()).remove(0);
    merge_lookup(&mut c, &lookup_fixture());
    assert_eq!(c.track, Some(2));
    assert_eq!(c.track_total, Some(12));
    assert_eq!(c.disc, Some(1));
    assert_eq!(c.disc_total, Some(1));
    assert_eq!(c.genre.as_deref(), Some("Alternative Rock"));
}

#[test]
fn parses_the_library_filename_conventions() {
    let p = parse_music_filename("02 - Creep");
    assert_eq!(p.track, Some(2));
    assert_eq!(p.title.as_deref(), Some("Creep"));

    let p = parse_music_filename("Radiohead - Creep");
    assert_eq!(p.artist.as_deref(), Some("Radiohead"));
    assert_eq!(p.title.as_deref(), Some("Creep"));
    assert_eq!(p.track, None);

    let p = parse_music_filename("Radiohead - Pablo Honey - 02 - Creep");
    assert_eq!(p.artist.as_deref(), Some("Radiohead"));
    assert_eq!(p.album.as_deref(), Some("Pablo Honey"));
    assert_eq!(p.track, Some(2));
    assert_eq!(p.title.as_deref(), Some("Creep"));

    let p = parse_music_filename("02. Radiohead - Creep (1993)");
    assert_eq!(p.track, Some(2));
    assert_eq!(p.artist.as_deref(), Some("Radiohead"));
    assert_eq!(p.title.as_deref(), Some("Creep"));
    assert_eq!(p.year, Some(1993));

    // A bare year is a title, not a track number.
    let p = parse_music_filename("1979");
    assert_eq!(p.track, None);
    assert_eq!(p.title.as_deref(), Some("1979"));
}

#[test]
fn similarity_rewards_the_right_things() {
    assert_eq!(similarity("Creep", "Creep"), 1.0);
    assert_eq!(similarity("Creep", "creep"), 1.0);
    assert!(similarity("Radiohead", "Radiohead feat. Someone") > 0.95);
    assert!(similarity("Creep", "Creep (Remastered)") >= 0.9);
    assert!(similarity("Creep", "Karma Police") < 0.4);
    assert!(similarity("The Beatles", "Beatles") > 0.6);
}

#[test]
fn confident_match_scores_confident() {
    let cand = map_search_response(&search_fixture()).remove(0);
    let local = LocalInfo {
        title: Some("Creep".into()),
        artist: Some("Radiohead".into()),
        album: Some("Pablo Honey".into()),
        duration_secs: Some(239.0),
    };
    let (score, ev) = score_candidate(&local, &cand);
    assert!(score >= CONFIDENT_MIN, "score {score}: {ev:?}");
    assert_eq!(tier_for(score), Tier::Confident);
    assert!(ev.iter().any(|e| e.contains("title matches exactly")));
}

#[test]
fn wrong_artist_kills_confidence() {
    let cand = map_search_response(&search_fixture()).remove(0);
    let local = LocalInfo {
        title: Some("Creep".into()),
        artist: Some("TLC".into()),
        album: None,
        duration_secs: Some(262.0),
    };
    let (score, _ev) = score_candidate(&local, &cand);
    assert!(score < CONFIDENT_MIN, "score {score}");
}

#[test]
fn plan_fills_only_empty_and_flags_overwrites() {
    let mut cand = map_search_response(&search_fixture()).remove(0);
    merge_lookup(&mut cand, &lookup_fixture());
    let current = AudioTags {
        title: Some("Creep".into()),       // agrees → no change
        artist: Some("Radiohed".into()),   // typo'd → overwrite proposal
        album: None,                       // empty → fill
        year: Some("1993".into()),        // agrees numerically
        ..Default::default()
    };
    let plan = plan_changes(&current, &cand);
    let by_field: std::collections::HashMap<&str, &TagChange> =
        plan.iter().map(|c| (c.field.as_str(), c)).collect();
    assert!(!by_field.contains_key("title"), "agreeing title must not be a change");
    assert!(!by_field.contains_key("year"));
    let artist = by_field["artist"];
    assert!(artist.overwrite);
    assert_eq!(artist.current.as_deref(), Some("Radiohed"));
    assert_eq!(artist.proposed, "Radiohead");
    let album = by_field["album"];
    assert!(!album.overwrite);
    assert_eq!(album.proposed, "Pablo Honey");
    // Filled from the lookup merge:
    assert_eq!(by_field["track"].proposed, "2");
    assert_eq!(by_field["trackTotal"].proposed, "12");
    assert_eq!(by_field["genre"].proposed, "Alternative Rock");
}

#[test]
fn numeric_agreement_ignores_leading_zeros() {
    let cand = map_search_response(&search_fixture()).remove(0);
    let current = AudioTags {
        title: Some("Creep".into()),
        artist: Some("Radiohead".into()),
        year: Some("1993".into()),
        ..Default::default()
    };
    // Candidate year matches; only genuinely missing fields fill.
    let plan = plan_changes(&current, &cand);
    assert!(plan.iter().all(|c| c.field != "year"));
}
