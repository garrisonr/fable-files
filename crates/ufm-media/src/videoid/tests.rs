//! Corpus + scoring tests for videoid. The corpus is the contract:
//! each entry is a real-world naming pattern with the parse Fable
//! promises. Accuracy is reported per class in the build report —
//! these tests asserting green IS the measurement.

use super::*;

fn p(name: &str) -> ParsedVideo {
    parse_video_filename(name)
}

// ---------------------------------------------------------------------
// Movies: scene-style, paren years, bracket years, sequels, parts
// ---------------------------------------------------------------------

#[test]
fn corpus_movies() {
    let cases: &[(&str, &str, Option<u32>)] = &[
        ("Dune.Part.Two.2024.1080p.BluRay.x264-EVO.mkv", "Dune Part Two", Some(2024)),
        ("The.Greasy.Strangler.2016.1080p.BluRay.x264.mp4", "The Greasy Strangler", Some(2016)),
        ("Maid.Robot.2026.1080p.WEBRip.x264-[YTS].mp4", "Maid Robot", Some(2026)),
        ("Avatar (2009).mkv", "Avatar", Some(2009)),
        ("Film.Title.[2018].1080p.WEB-DL.mkv", "Film Title", Some(2018)),
        ("Se7en.1995.1080p.BluRay.mkv", "Se7en", Some(1995)),
        ("2012.2009.1080p.BluRay.x264.mkv", "2012", Some(2009)),
        ("The Matrix Reloaded (2003) 2160p UHD BluRay REMUX.mkv", "The Matrix Reloaded", Some(2003)),
        ("Some.Movie.2019.WEBRip.mkv", "Some Movie", Some(2019)),
        ("A.Few.Words.1080p.mkv", "A Few Words", None),
        ("S.W.A.T. (2017).mkv", "S W A T", Some(2017)),
    ];
    for (name, title, year) in cases {
        let v = p(name);
        assert_eq!(v.title, *title, "title for {name}");
        assert_eq!(v.year, *year, "year for {name}");
        assert_eq!(v.kind, ParsedKind::Movie, "kind for {name}");
    }
}

#[test]
fn movie_explicit_ids_and_editions() {
    let v = p("The Dark Knight (2008) {tmdb-155}.mkv");
    assert_eq!(v.explicit_tmdb_id, Some(155));
    assert_eq!(v.title, "The Dark Knight");
    assert_eq!(v.year, Some(2008));

    let v = p("Batman Begins (2005) {imdb-tt0372784}.mp4");
    assert_eq!(v.explicit_imdb_id.as_deref(), Some("tt0372784"));

    let v = p("Movie (2020) [tmdbid=999].mkv");
    assert_eq!(v.explicit_tmdb_id, Some(999));

    let v = p("Blade.Runner.1982.FINAL.CUT.2160p.BluRay.mkv");
    assert_eq!(v.title, "Blade Runner");
    assert_eq!(v.edition.as_deref(), Some("Final Cut"));

    let v = p("Alien.1979.Directors.Cut.1080p.BluRay.mkv");
    assert_eq!(v.title, "Alien");
    assert_eq!(v.edition.as_deref(), Some("Director's Cut"));

    let v = p("Alien (1979) Directors Cut BluRay.mkv");
    assert_eq!(v.title, "Alien");
    assert_eq!(v.edition.as_deref(), Some("Director's Cut"));

    // A film actually CALLED "The Final Cut" keeps its title.
    let v = p("The Final Cut (2004).mkv");
    assert_eq!(v.title, "The Final Cut");
    assert_eq!(v.edition, None);

    let v = p("The.Lord.of.the.Rings.The.Fellowship.of.the.Ring.2001.EXTENDED.BluRay.mkv");
    assert!(v.title.starts_with("The Lord of the Rings"));
    assert_eq!(v.edition.as_deref(), Some("Extended"));

    let v = p("Blade Runner (1982) {edition-Final Cut}.mkv");
    assert_eq!(v.edition.as_deref(), Some("Final Cut"));
}

#[test]
fn movie_split_parts() {
    let v = p("Some Movie (1999) CD1.avi");
    assert_eq!(v.title, "Some Movie");
    assert_eq!(v.part.as_deref(), Some("cd1"));

    let v = p("The Dark Knight (2008) - pt2.mp4");
    assert_eq!(v.title, "The Dark Knight");
    assert_eq!(v.part.as_deref(), Some("pt2"));

    let v = p("Old.Film.1975.Disc.1.DVDrip.avi");
    assert_eq!(v.title, "Old Film");
    assert_eq!(v.part.as_deref(), Some("disc1"));

    // Sequel spelled with a digit BEFORE the year: title, not a part.
    let v = p("Dune Part 2 (2024).mkv");
    assert_eq!(v.title, "Dune Part 2");
    assert_eq!(v.part, None);

    // "part N" AFTER a bare year: a split part.
    let v = p("Long.Movie.2020.Part.1.1080p.mkv");
    assert_eq!(v.title, "Long Movie");
    assert_eq!(v.part.as_deref(), Some("part1"));
}

// ---------------------------------------------------------------------
// Episodes: every marker family
// ---------------------------------------------------------------------

#[test]
fn corpus_episodes() {
    // (name, show, season, first, last)
    let cases: &[(&str, &str, u32, u32, u32)] = &[
        ("Show.Name.S01E02.720p.WEB-DL.mkv", "Show Name", 1, 2, 2),
        ("Show Name S01E02.mkv", "Show Name", 1, 2, 2),
        ("show.name.s01e02.hdtv.x264.mkv", "show name", 1, 2, 2),
        ("Breaking.Bad.1x03.HDTV.x264-ASAP.avi", "Breaking Bad", 1, 3, 3),
        ("Show.12x104.mkv", "Show", 12, 104, 104),
        ("The.Office.US.S03E05-E06.720p.mkv", "The Office US", 3, 5, 6),
        ("Show Name S01E01E02.mkv", "Show Name", 1, 1, 2),
        ("Show Name S01E01-E02-E03.mkv", "Show Name", 1, 1, 3),
        ("Show Name S01E01-02.mkv", "Show Name", 1, 1, 2),
        ("Breaking Bad Season 2 Episode 5.mkv", "Breaking Bad", 2, 5, 5),
        ("Show.Name.S00E03.Special.720p.mkv", "Show Name", 0, 3, 3),
        ("Severance.S02E01.2160p.WEB.H.265.DV.HDR.mkv", "Severance", 2, 1, 1),
        ("Doctor.Who.1963.S01E01.An.Unearthly.Child.mkv", "Doctor Who", 1, 1, 1),
        ("Band of Brothers (2001) - S01E01 - Currahee.mkv", "Band of Brothers", 1, 1, 1),
        ("S.W.A.T.2017.S01E01.HDTV.mkv", "S W A T", 1, 1, 1),
    ];
    for (name, show, s, e1, e2) in cases {
        let v = p(name);
        assert_eq!(v.kind, ParsedKind::Episode, "kind for {name}");
        assert_eq!(v.title, *show, "show for {name}");
        assert_eq!(v.season, Some(*s), "season for {name}");
        assert_eq!(v.episode_first, Some(*e1), "ep first for {name}");
        assert_eq!(v.episode_last, Some(*e2), "ep last for {name}");
    }
}

#[test]
fn episode_year_and_hints() {
    let v = p("Doctor.Who.1963.S01E01.An.Unearthly.Child.mkv");
    assert_eq!(v.year, Some(1963));
    assert_eq!(v.episode_title_hint.as_deref(), Some("An Unearthly Child"));

    let v = p("Band of Brothers (2001) - S01E01 - Currahee.mkv");
    assert_eq!(v.year, Some(2001));
    assert_eq!(v.episode_title_hint.as_deref(), Some("Currahee"));

    // Pure-junk tail = no hint, not a garbage hint.
    let v = p("Show.Name.S01E02.720p.WEB-DL.DD5.1.H.264-EVO.mkv");
    assert_eq!(v.title, "Show Name");
    assert!(v.episode_title_hint.is_none(), "hint was {:?}", v.episode_title_hint);
}

#[test]
fn flagged_not_guessed() {
    // Date-based: flagged, dated, NOT converted to S/E.
    let v = p("The.Colbert.Report.2011.11.15.HDTV.x264.mkv");
    assert_eq!(v.kind, ParsedKind::Episode);
    assert_eq!(v.air_date.as_deref(), Some("2011-11-15"));
    assert_eq!(v.season, None);
    assert_eq!(v.episode_first, None);
    assert!(v.flags.contains(&"date_based".to_string()));
    assert_eq!(v.title, "The Colbert Report");

    // Anime absolute: flagged, numbered, NOT split into S/E.
    let v = p("Naruto - 042 [1080p].mkv");
    assert_eq!(v.kind, ParsedKind::Episode);
    assert_eq!(v.absolute_episode, Some(42));
    assert_eq!(v.season, None);
    assert!(v.flags.contains(&"absolute_numbering".to_string()));
    assert_eq!(v.title, "Naruto");

    let v = p("One Piece EP1100.mkv");
    assert_eq!(v.absolute_episode, Some(1100));
    assert_eq!(v.title, "One Piece");

    // A bare resolution after a dash is not an episode number.
    let v = p("Some Show - 1080.mkv");
    assert_eq!(v.absolute_episode, None);
}

#[test]
fn folder_context_fills_season_and_year() {
    let v = p("Show Name - E05.mkv");
    assert_eq!(v.absolute_episode, Some(5)); // bare E05 reads absolute…

    let mut v2 = p("Show.Name.E05.720p.mkv");
    apply_folder_context(&mut v2, "Season 02");
    // …but inside a Season folder, E05 is episode 5 of season 2.
    assert_eq!(v2.season, Some(2));
    assert_eq!(v2.episode_first, Some(5));
    assert_eq!(v2.absolute_episode, None);

    let mut v3 = p("Show Name S01E01.mkv");
    apply_folder_context(&mut v3, "Specials");
    assert_eq!(v3.season, Some(1)); // an explicit S01 beats the folder

    // No season folder → the absolute number stays absolute + flagged.
    let mut v4 = p("Show.Name.E07.mkv");
    apply_folder_context(&mut v4, "Random Folder");
    assert_eq!(v4.season, None);
    assert_eq!(v4.absolute_episode, Some(7));

    // A show folder supplies the year the file name lacks.
    let mut v5 = p("Show Name S01E01.mkv");
    apply_folder_context(&mut v5, "Show Name (2019)");
    assert_eq!(v5.year, Some(2019));
}

// ---------------------------------------------------------------------
// Canonical naming
// ---------------------------------------------------------------------

#[test]
fn canonical_names() {
    assert_eq!(
        movie_target_rel("Dune: Part Two", Some(2024), None, None, "mkv"),
        PathBuf::from("Dune Part Two (2024)").join("Dune Part Two (2024).mkv")
    );
    assert_eq!(
        movie_file_stem("The Dark Knight", Some(2008), None, Some("pt1")),
        "The Dark Knight (2008) - pt1"
    );
    assert_eq!(
        movie_file_stem("Blade Runner", Some(1982), Some("Final Cut"), None),
        "Blade Runner (1982) {edition-Final Cut}"
    );
    assert_eq!(
        episode_target_rel("Severance", Some(2022), 1, &[1], &["Good News About Hell".to_string()], "mkv"),
        PathBuf::from("Severance (2022)")
            .join("Season 01")
            .join("Severance (2022) - S01E01 - Good News About Hell.mkv")
    );
    assert_eq!(
        episode_file_stem("Show", Some(2020), 1, &[1, 2], &["Pilot".into(), "Second".into()]),
        "Show (2020) - S01E01-E02 - Pilot - Second"
    );
    assert_eq!(season_dir_name(0), "Season 00");
    assert_eq!(sanitize_component("A/B: C?"), "A B C");
}

// ---------------------------------------------------------------------
// Scoring
// ---------------------------------------------------------------------

fn cand(id: i64, media_type: &str, title: &str, year: u32, pop: f64) -> TmdbCandidate {
    TmdbCandidate {
        id,
        media_type: media_type.into(),
        title: title.into(),
        original_title: title.into(),
        year: Some(year),
        popularity: pop,
    }
}

#[test]
fn scoring_tiers() {
    // Exact title + year → Confident.
    let v = p("Dune.Part.Two.2024.1080p.mkv");
    let s = score_candidate(&v, &cand(693134, "movie", "Dune: Part Two", 2024, 500.0));
    assert_eq!(s.tier, Tier::Confident);
    assert!(s.score >= 0.99, "score {}", s.score);

    // Same words, wrong year → Unknown (containment cannot rescue it).
    let v = p("Dune.2021.1080p.mkv");
    let s = score_candidate(&v, &cand(693134, "movie", "Dune: Part Two", 2024, 500.0));
    assert_eq!(s.tier, Tier::Unknown, "score {}", s.score);

    // Year is the Office disambiguator.
    let v = p("The.Office.2005.S01E01.mkv");
    let us = score_candidate(&v, &cand(2316, "tv", "The Office", 2005, 300.0));
    let uk = score_candidate(&v, &cand(2996, "tv", "The Office", 2001, 200.0));
    assert_eq!(us.tier, Tier::Confident);
    assert!(us.score > uk.score);
    // The Office's parsed title is "The Office" — similarity runs
    // through normalize_title which drops articles on both sides.

    // Word-number parity: "Part 2" parses to the TMDB "Part Two".
    let v = p("Dune Part 2 (2024).mkv");
    let s = score_candidate(&v, &cand(693134, "movie", "Dune: Part Two", 2024, 500.0));
    assert_eq!(s.tier, Tier::Confident);

    // Explicit id beats everything.
    let v = p("Anything (2008) {tmdb-155}.mkv");
    let s = score_candidate(&v, &cand(155, "movie", "Totally Different", 1999, 1.0));
    assert_eq!(s.score, 1.0);
    assert_eq!(s.tier, Tier::Confident);

    // Ranking is best-first.
    let v = p("Severance.2022.S01E01.mkv");
    let ranked = rank_candidates(
        &v,
        &[
            cand(1, "tv", "Severance Pay", 2020, 5.0),
            cand(95396, "tv", "Severance", 2022, 250.0),
        ],
    );
    assert_eq!(ranked[0].candidate.id, 95396);
}

#[test]
fn episode_verification() {
    let eps = vec![
        TmdbEpisode { episode_number: 1, name: "Good News About Hell".into(), air_date: Some("2022-02-17".into()) },
        TmdbEpisode { episode_number: 2, name: "Half Loop".into(), air_date: None },
    ];
    let v = p("Severance.S01E01.Good.News.About.Hell.mkv");
    let (bonus, note) = verify_episode(&v, &eps);
    assert!(bonus >= 0.09, "bonus {bonus}");
    assert!(note.unwrap().contains("matches TMDB"));

    let v = p("Severance.S01E11.mkv");
    let (adj, note) = verify_episode(&v, &eps);
    assert!(adj < 0.0);
    assert!(note.unwrap().contains("claims E11"));
}

#[test]
fn title_similarity_basics() {
    assert_eq!(title_similarity("Dune Part Two", "Dune: Part Two"), 1.0);
    assert_eq!(title_similarity("The Office", "Office"), 1.0);
    assert!(title_similarity("Star Wars", "Star Trek") < 0.6);
    assert_eq!(title_similarity("", "Anything"), 0.0);
}
