//! Fixture-tree tests for the File Auditor: plant typos, pattern
//! breaks, gibberish, junk, dupes, and corruption — assert flags
//! AND non-flags (the spec's named proofs: gibberish left alone,
//! a consistently ALL-CAPS folder left alone).

use super::*;
use std::collections::HashMap as Map;

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "fable-auditor-test-{}-{}",
        tag,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn put(root: &Path, rel: &str, bytes: &[u8]) -> PathBuf {
    let p = root.join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&p, bytes).unwrap();
    p
}

fn put_sized(root: &Path, rel: &str, size: u64) -> PathBuf {
    let p = root.join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let f = std::fs::File::create(&p).unwrap();
    f.set_len(size).unwrap();
    p
}

fn age_file(path: &Path, when: &str) {
    let ok = std::process::Command::new("touch")
        .args(["-d", when, &path.to_string_lossy()])
        .status()
        .unwrap()
        .success();
    assert!(ok, "touch -d failed for {}", path.display());
}

fn scan(root: &Path) -> (ScanTree, HashMap<PathBuf, Vec<usize>>) {
    let tree = walk(root);
    let by_parent = children_by_parent(&tree);
    (tree, by_parent)
}

fn fams<'a>(cands: &'a [Candidate], family: &str) -> Vec<&'a Candidate> {
    cands.iter().filter(|c| c.family == family).collect()
}

struct StubProbe {
    avail: bool,
    map: Map<String, ProbeResult>,
}
impl MediaProbe for StubProbe {
    fn available(&self) -> bool {
        self.avail
    }
    fn probe(&self, path: &Path) -> Option<ProbeResult> {
        self.map
            .get(path.file_name()?.to_str()?)
            .cloned()
    }
}
fn no_probe() -> StubProbe {
    StubProbe { avail: false, map: Map::new() }
}

fn test_words() -> HashSet<String> {
    [
        "the", "richard", "garrison", "story", "movie", "video", "great", "american",
        "holiday", "photos", "family", "dinner", "party",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

// ------------------------------------------------------------------

#[test]
fn bundled_dictionary_loads() {
    let words = bundled_words();
    assert!(words.len() > 50_000, "bundled list should be full-size");
    assert!(words.contains("the"));
    assert!(words.contains("movie"));
    // His name is a proper noun — NOT in a general dictionary. It
    // arrives via user words, which is the whole design.
    assert!(!words.contains("richrd"));
    assert_eq!(edit_distance("richrd", "richard"), 1);
    assert_eq!(edit_distance("teh", "the"), 1, "transposition counts once");
    assert_eq!(edit_distance("garrison", "garrison"), 0);
}

#[test]
fn typo_flags_near_miss_and_ignores_gibberish() {
    let root = fixture("typo");
    put(&root, "The Richrd Garrison Story.txt", b"x");
    put(&root, "asdfgh qwer zxcv.txt", b"x");
    put(&root, "xK9f zQw7 plM2x.bin", b"x");
    put(&root, "The Richrd Garrissn Story.txt", b"x"); // two misses — not the typo shape
    put(&root, "The Richard Garrison Story.txt", b"x"); // clean
    let (tree, _) = scan(&root);
    let cands = detect_typos(&tree, &test_words());
    assert_eq!(cands.len(), 1, "exactly the one near-miss flags: {cands:?}");
    let c = &cands[0];
    assert!(c.path.ends_with("The Richrd Garrison Story.txt"));
    assert_eq!(c.lane, "question");
    assert_eq!(
        c.proposal["to"],
        serde_json::json!("The Richard Garrison Story.txt")
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn pattern_gaps_and_outliers() {
    let root = fixture("pattern");
    for n in ["XY1.txt", "XY2.txt", "XY3.txt", "XY5.txt", "XY-4.txt", "XYY6.txt"] {
        put(&root, n, b"x");
    }
    let (tree, by_parent) = scan(&root);
    let cands = detect_patterns(&tree, &by_parent);
    let gaps: Vec<_> = cands
        .iter()
        .filter(|c| c.class_key.starts_with("pattern:gap"))
        .collect();
    // Numbers present: 1,2,3,5 plus XY-4's 4 → no interior gap.
    assert!(gaps.is_empty(), "run is dense once XY-4 counts: {cands:?}");
    let outliers: Vec<_> = cands
        .iter()
        .filter(|c| c.class_key.starts_with("pattern:outlier"))
        .collect();
    assert_eq!(outliers.len(), 2, "XY-4 (separator) + XYY6 (letters): {cands:?}");
    let proposals: Vec<String> = outliers
        .iter()
        .map(|c| c.proposal["to"].as_str().unwrap().to_string())
        .collect();
    assert!(proposals.contains(&"XY4.txt".to_string()), "{proposals:?}");
    assert!(proposals.contains(&"XY6.txt".to_string()), "{proposals:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn pattern_gap_alone() {
    let root = fixture("gap");
    for n in ["AB10.log", "AB11.log", "AB13.log", "AB14.log"] {
        put(&root, n, b"x");
    }
    let (tree, by_parent) = scan(&root);
    let cands = detect_patterns(&tree, &by_parent);
    assert_eq!(cands.len(), 1);
    assert_eq!(cands[0].lane, "certain");
    assert!(cands[0].title.contains("AB12"), "{}", cands[0].title);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn corrupt_layers() {
    let root = fixture("corrupt");
    put(&root, "empty.txt", b"");
    put(&root, "fake.png", b"this is not a png at all");
    put(&root, "real.png", b"\x89PNG\r\n\x1a\nrest");
    put(&root, "notes.txt", b"plain text, no magic expected");
    let bad_wav = put(&root, "bad.wav", b"RIFF\x04\x00\x00\x00JUNKmore");
    let _ = bad_wav;
    put(&root, "good.wav", b"RIFF\x04\x00\x00\x00WAVEfmt ");
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        symlink("nowhere-at-all", root.join("dangling-link")).unwrap();
    }
    let (tree, _) = scan(&root);
    let (cands, _notes) = detect_corrupt(&tree, &no_probe());
    let by_title: Vec<(&str, &str)> = cands
        .iter()
        .map(|c| {
            (
                c.path.rsplit('/').next().unwrap(),
                c.class_key.as_str(),
            )
        })
        .collect();
    assert!(by_title.contains(&("empty.txt", "corrupt:empty")), "{by_title:?}");
    assert!(
        by_title.contains(&("fake.png", "corrupt:magic-mismatch")),
        "{by_title:?}"
    );
    assert!(
        by_title.contains(&("bad.wav", "corrupt:magic-mismatch")),
        "{by_title:?}"
    );
    #[cfg(unix)]
    assert!(
        by_title.contains(&("dangling-link", "corrupt:broken-link")),
        "{by_title:?}"
    );
    assert!(
        !by_title.iter().any(|(n, _)| *n == "real.png" || *n == "good.wav" || *n == "notes.txt"),
        "clean files stay clean: {by_title:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn compat_names_and_collisions() {
    let root = fixture("compat");
    put(&root, "CON.txt", b"x");
    put(&root, "bad:name.txt", b"x");
    put(&root, "trailing.", b"x");
    put(&root, "Readme.txt", b"x");
    put(&root, "README.txt", b"x");
    put(&root, "fine.txt", b"x");
    let (tree, by_parent) = scan(&root);
    let cands = detect_compat(&tree, &by_parent);
    let get = |needle: &str| {
        cands
            .iter()
            .find(|c| c.path.ends_with(needle))
            .unwrap_or_else(|| panic!("no flag for {needle}: {cands:?}"))
    };
    assert_eq!(get("CON.txt").proposal["to"], serde_json::json!("CON_.txt"));
    assert_eq!(
        get("bad:name.txt").proposal["to"],
        serde_json::json!("bad-name.txt")
    );
    assert_eq!(
        get("trailing.").proposal["to"],
        serde_json::json!("trailing")
    );
    // Collision: exactly one of the pair is asked to move.
    let collisions: Vec<_> = cands
        .iter()
        .filter(|c| c.class_key == "compat:case-collision")
        .collect();
    assert_eq!(collisions.len(), 1);
    assert!(!cands.iter().any(|c| c.path.ends_with("fine.txt")));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn space_duplicates_verified_and_conversions_labeled() {
    let root = fixture("space");
    let cat = Catalog::open_in_memory().unwrap();
    let blob = vec![b'x'; 4096];
    put(&root, "a/movie.mkv", &blob);
    put(&root, "b/movie.mkv", &blob);
    put(&root, "c/other.mkv", &vec![b'y'; 4096]); // same size, different content
    put_sized(&root, "song.wav", 2 << 20);
    put_sized(&root, "big.mkv", 60 << 20);
    let mut map = Map::new();
    map.insert(
        "song.wav".to_string(),
        ProbeResult {
            readable: true,
            video_codec: None,
            audio_codec: Some("pcm_s16le".into()),
        },
    );
    map.insert(
        "big.mkv".to_string(),
        ProbeResult {
            readable: true,
            video_codec: Some("h264".into()),
            audio_codec: Some("aac".into()),
        },
    );
    let probe = StubProbe { avail: true, map };
    let (tree, _) = scan(&root);
    let cands = detect_space(&tree, &cat, &probe, &[]);
    let dups = fams(&cands, "space");
    let dup_flags: Vec<_> = dups
        .iter()
        .filter(|c| c.class_key == "space:duplicate")
        .collect();
    assert_eq!(dup_flags.len(), 1, "one verified dupe: {cands:?}");
    assert!(
        dup_flags[0].path.ends_with("b/movie.mkv"),
        "the settled copy (a/) is kept: {}",
        dup_flags[0].path
    );
    assert!(dup_flags[0].detail.contains("SHA-256"));
    let flac = dups
        .iter()
        .find(|c| c.class_key == "space:convert-flac")
        .expect("WAV→FLAC proposed");
    assert_eq!(flac.proposal["convertKind"], serde_json::json!("flac"));
    assert!(flac.detail.contains("lossless"), "{}", flac.detail);
    let hevc = dups
        .iter()
        .find(|c| c.class_key == "space:convert-hevc")
        .expect("H.264→HEVC proposed");
    assert_eq!(hevc.lane, "question", "a re-encode is ASKED, never asserted");
    assert!(hevc.detail.contains("RE-ENCODE"), "{}", hevc.detail);
    assert!(hevc.detail.contains("Estimated"), "{}", hevc.detail);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn junk_signatures_and_context() {
    let root = fixture("junk");
    put(&root, "Movie (2020)/Movie (2020).mkv", b"v");
    // Enough videos that the folder is genuinely video-majority.
    for extra in ["Trailer.mkv", "Sample.mkv", "Featurette.mkv", "Deleted Scenes.mkv", "Interview.mkv"] {
        put(&root, &format!("Movie (2020)/{extra}"), b"v");
    }
    put(&root, "Movie (2020)/YTS.LT.jpg", b"j");
    put(&root, "Movie (2020)/DoNotMirror.exe", b"e");
    put(&root, "Movie (2020)/setup.exe", b"e");
    put(&root, "Movie (2020)/site.url", b"u");
    put(&root, "Movie (2020)/readme.txt", b"Downloaded from www.example-torrents.com - enjoy");
    put(&root, "Docs/manual.url", b"u");
    put(&root, "Docs/handbook.pdf", b"%PDF-1.4");
    let (tree, by_parent) = scan(&root);
    let cands = detect_junk(&tree, &by_parent);
    let classes: Vec<&str> = cands.iter().map(|c| c.class_key.as_str()).collect();
    assert!(classes.contains(&"junk:yts-image"), "{classes:?}");
    assert!(classes.contains(&"junk:donotmirror"), "{classes:?}");
    assert!(classes.contains(&"junk:downloaded-from"), "{classes:?}");
    assert!(classes.contains(&"junk:url-media"), "{classes:?}");
    let exe = cands
        .iter()
        .find(|c| c.class_key == "junk:exe-in-video-folder")
        .expect("exe among videos asked about");
    assert_eq!(exe.lane, "question");
    // A .url outside a media folder is a question, not a verdict.
    let doc_url = cands
        .iter()
        .find(|c| c.path.ends_with("Docs/manual.url"))
        .expect("docs .url present");
    assert_eq!(doc_url.lane, "question");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn clutter_families() {
    let root = fixture("clutter");
    put(&root, ".DS_Store", b"x");
    put(&root, "Films/Movie.mkv", b"v");
    put(&root, "Films/Movie.en.srt", b"s");
    put(&root, "Films/Gone Movie.srt", b"s");
    let old_part = put(&root, "Downloads/film.mkv.part", b"p");
    age_file(&old_part, "30 days ago");
    put(&root, "Downloads/fresh.mkv.part", b"p");
    put(&root, "Pics/pics.zip", b"z");
    put(&root, "Pics/pics/inner.txt", b"i");
    put(&root, "Films/Movie (1).mkv", b"v");
    std::fs::create_dir_all(root.join("EmptyNest/inner")).unwrap();
    let (tree, by_parent) = scan(&root);
    let settings = AuditorSettings::default();
    let cands = detect_clutter(&tree, &by_parent, &settings, &root);
    let classes: Vec<&str> = cands.iter().map(|c| c.class_key.as_str()).collect();
    assert!(classes.contains(&"clutter:os-junk"), "{classes:?}");
    assert!(classes.contains(&"clutter:orphan-sidecar"), "{classes:?}");
    assert!(classes.contains(&"clutter:stale-partial"), "{classes:?}");
    assert!(classes.contains(&"clutter:archive-pair"), "{classes:?}");
    assert!(classes.contains(&"clutter:copy-chain"), "{classes:?}");
    assert!(classes.contains(&"clutter:empty-folder"), "{classes:?}");
    // Only the TOPMOST empty folder flags.
    assert_eq!(
        cands
            .iter()
            .filter(|c| c.class_key == "clutter:empty-folder")
            .count(),
        1
    );
    // Fresh partial and the live movie's subtitle stay unflagged.
    assert!(!cands.iter().any(|c| c.path.ends_with("fresh.mkv.part")));
    assert!(!cands.iter().any(|c| c.path.ends_with("Movie.en.srt")));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn style_folder_relative() {
    let root = fixture("style");
    // Consistently ALL-CAPS: NO flags — the spec's motto case.
    for n in ["ALPHA ONE.txt", "BRAVO TWO.txt", "CHARLIE THREE.txt", "DELTA FOUR.txt", "ECHO FIVE.txt"] {
        put(&root, &format!("Caps/{n}"), b"x");
    }
    // Mostly lowercase with one screamer.
    for n in ["alpha.txt", "beta.txt", "gamma.txt", "delta.txt", "OMEGA.txt"] {
        put(&root, &format!("Mixed/{n}"), b"x");
    }
    // Underscore house style with one dash rebel.
    for n in ["one_two.txt", "three_four.txt", "five_six.txt", "seven_eight.txt", "eleven_twelve.txt", "nine-ten.txt"] {
        put(&root, &format!("Seps/{n}"), b"x");
    }
    let (tree, by_parent) = scan(&root);
    let cands = detect_style(&tree, &by_parent);
    assert!(
        !cands.iter().any(|c| c.path.contains("Caps")),
        "consistent caps folder gets NO flags: {cands:?}"
    );
    let case = cands
        .iter()
        .find(|c| c.class_key == "style:case")
        .expect("OMEGA flags");
    assert!(case.path.ends_with("OMEGA.txt"));
    assert_eq!(case.proposal["to"], serde_json::json!("omega.txt"));
    let sep = cands
        .iter()
        .find(|c| c.class_key == "style:separator")
        .expect("nine-ten flags");
    assert_eq!(sep.proposal["to"], serde_json::json!("nine_ten.txt"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn editions_group_same_title_year() {
    let root = fixture("editions");
    put(&root, "Mean Girls (2004).mkv", b"a");
    put(&root, "Mean Girls (2004) 2160p.mkv", b"b");
    put(&root, "Mean Girls (2024).mkv", b"c");
    let (tree, _) = scan(&root);
    let cands = detect_editions(&tree);
    assert_eq!(cands.len(), 1, "only the 2004 pair groups: {cands:?}");
    assert!(cands[0].title.contains("appears 2 times"), "{}", cands[0].title);
    assert_eq!(cands[0].proposal["kind"], serde_json::json!("none"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cold_flags_only_old_and_big_first() {
    let root = fixture("cold");
    let old = put(&root, "ancient.bin", b"o");
    age_file(&old, "5 years ago");
    put(&root, "recent.bin", b"r");
    let (tree, _) = scan(&root);
    let settings = AuditorSettings::default();
    let cands = detect_cold(&tree, &settings);
    assert_eq!(cands.len(), 1);
    assert!(cands[0].path.ends_with("ancient.bin"));
    let _ = std::fs::remove_dir_all(&root);
}

// ------------------------------------------------------------------
// The memory: ask lane, dismissal persistence, standing rules.
// ------------------------------------------------------------------

#[test]
fn run_audit_memory_supersede_and_rules() {
    let root = fixture("memory");
    put(&root, "The Richrd Garrison Story.txt", b"x");
    let cat = Catalog::open_in_memory().unwrap();
    let settings = AuditorSettings::default();
    let req = RunRequest {
        target: root.to_string_lossy().into_owned(),
        schedule_id: None,
        schedule_name: "Manual audit".into(),
        late: false,
    };
    // Run 1: the typo is flagged.
    let run = run_audit(&cat, &req, &settings, &no_probe()).unwrap();
    assert_eq!(run.status, "completed");
    let open = cat.auditor_flags(Some("open"), 10).unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].family, "typo");
    // "This one is OK" — dismissal lands, flag closes.
    let flag = open[0].clone();
    cat.add_auditor_dismissal(&flag.path, "typo", "2026-10-15T00:00:00")
        .unwrap();
    cat.set_auditor_flag_state(&flag.id, "dismissed", "2026-10-15T00:00:00")
        .unwrap();
    // Run 2: never reflagged.
    run_audit(&cat, &req, &settings, &no_probe()).unwrap();
    assert_eq!(cat.auditor_open_flag_count().unwrap(), 0);
    // Standing rule path: fresh fixture, suppress the whole class.
    let root2 = fixture("memory2");
    put(&root2, "The Richrd Garrison Story.txt", b"x");
    cat.set_auditor_rule("typo:richrd", "suppress", "2026-10-15T00:00:00")
        .unwrap();
    let req2 = RunRequest {
        target: root2.to_string_lossy().into_owned(),
        ..RunRequest {
            target: String::new(),
            schedule_id: None,
            schedule_name: "Manual audit".into(),
            late: false,
        }
    };
    run_audit(&cat, &req2, &settings, &no_probe()).unwrap();
    assert_eq!(cat.auditor_open_flag_count().unwrap(), 0);
    // User-word path: teaching the dictionary the token ends it too.
    cat.delete_auditor_rule("typo:richrd").unwrap();
    cat.add_auditor_user_word("richrd", "2026-10-15T00:00:00").unwrap();
    run_audit(&cat, &req2, &settings, &no_probe()).unwrap();
    assert_eq!(cat.auditor_open_flag_count().unwrap(), 0);
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&root2);
}

#[test]
fn skip_once_resurfaces_next_run_skip_forever_never_does() {
    // The answer grammar (Dickles, 2026-10-15): "Skip This One
    // Once" dismisses for THIS run only — the flag returns on the
    // next audit. "Skip This One Forever" is the dismissal memory
    // and never returns.
    let root = fixture("skipgrammar");
    put(&root, "The Richrd Garrison Story.txt", b"x");
    let cat = Catalog::open_in_memory().unwrap();
    let settings = AuditorSettings::default();
    let req = RunRequest {
        target: root.to_string_lossy().into_owned(),
        schedule_id: None,
        schedule_name: "Manual audit".into(),
        late: false,
    };
    run_audit(&cat, &req, &settings, &no_probe()).unwrap();
    let open = cat.auditor_flags(Some("open"), 10).unwrap();
    assert_eq!(open.len(), 1);
    // SKIP ONCE — exactly what auditor_resolve does: state change
    // only, no dismissal row, no rule.
    cat.set_auditor_flag_state(&open[0].id, "skipped", "2026-10-15T00:00:00")
        .unwrap();
    assert_eq!(cat.auditor_open_flag_count().unwrap(), 0);
    // Next run: it resurfaces as a fresh open flag.
    run_audit(&cat, &req, &settings, &no_probe()).unwrap();
    let open = cat.auditor_flags(Some("open"), 10).unwrap();
    assert_eq!(open.len(), 1, "skip-once must resurface on the next run");
    assert_eq!(open[0].family, "typo");
    // SKIP FOREVER — dismissal memory lands.
    cat.add_auditor_dismissal(&open[0].path, "typo", "2026-10-15T00:00:00")
        .unwrap();
    cat.set_auditor_flag_state(&open[0].id, "dismissed", "2026-10-15T00:00:00")
        .unwrap();
    run_audit(&cat, &req, &settings, &no_probe()).unwrap();
    assert_eq!(
        cat.auditor_open_flag_count().unwrap(),
        0,
        "skip-forever must never resurface"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn fix_rule_promotes_future_matches_to_facts() {
    // "Fix All Similar (Irreversible)" records a "fix" standing
    // rule: future matches of the class surface as FACTS with the
    // fix proposed — never questions again, and never silently
    // fixed (the file on disk is untouched by the rule itself).
    let root = fixture("fixrule");
    put(&root, "The Richrd Garrison Story.txt", b"x");
    let cat = Catalog::open_in_memory().unwrap();
    cat.set_auditor_rule("typo:richrd", "fix", "2026-10-15T00:00:00")
        .unwrap();
    let settings = AuditorSettings::default();
    let req = RunRequest {
        target: root.to_string_lossy().into_owned(),
        schedule_id: None,
        schedule_name: "Manual audit".into(),
        late: false,
    };
    run_audit(&cat, &req, &settings, &no_probe()).unwrap();
    let open = cat.auditor_flags(Some("open"), 10).unwrap();
    assert_eq!(open.len(), 1, "a fix-ruled match still surfaces");
    assert_eq!(open[0].lane, "certain", "as a FACT, not a question");
    assert!(
        open[0].proposal_json.as_deref().unwrap_or("").contains("rename"),
        "with the fix proposed: {:?}",
        open[0].proposal_json
    );
    assert!(
        root.join("The Richrd Garrison Story.txt").exists(),
        "the rule never silently renames"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn schedule_conversion_uses_the_tested_machine() {
    let sched = AuditSchedule {
        id: "s1".into(),
        name: "Nightly".into(),
        target: "/data".into(),
        time: "03:00".into(),
        weekdays: vec![],
        idle_minutes: 0,
        require_on_ac: false,
        enabled: true,
        last_started: None,
        last_status: None,
    };
    let mc = sched.as_mc();
    let slot =
        chrono::NaiveDateTime::parse_from_str("2026-10-15T03:00:00", "%Y-%m-%dT%H:%M:%S")
            .unwrap();
    // Due exactly AT the slot (the semantics the pulse pinned).
    assert!(matches!(
        crate::mediacomplete::evaluate(&mc, slot),
        crate::mediacomplete::Decision::Due { late: false }
    ));
    // Before today's slot with yesterday's already started: not due.
    // (Without last_started, yesterday's missed slot is owed as a
    // catch-up — the machine's documented behavior.)
    let mut done_sched = sched.clone();
    done_sched.last_started = Some("2026-10-14T03:00:00".into());
    let mc2 = done_sched.as_mc();
    let before =
        chrono::NaiveDateTime::parse_from_str("2026-10-15T02:00:00", "%Y-%m-%dT%H:%M:%S")
            .unwrap();
    assert!(matches!(
        crate::mediacomplete::evaluate(&mc2, before),
        crate::mediacomplete::Decision::NotDue
    ));
    // And the catch-up case itself: yesterday's slot owed, late.
    assert!(matches!(
        crate::mediacomplete::evaluate(&mc, before),
        crate::mediacomplete::Decision::Due { late: true }
    ));
}

#[test]
fn typo_suggestion_carries_the_tokens_case() {
    // Dickles, 2026-10-15: the question headline cited the
    // dictionary's lowercase form ("breckenridge") while the
    // detail and the rename proposal in the SAME card correctly
    // showed "Breckenridge". The suggestion must carry the case
    // pattern of the token it replaces — in the headline, the
    // detail, AND the proposal; they render from one string.
    let mut words = test_words();
    words.insert("breckenridge".to_string());
    let root = fixture("typocase");
    put(&root, "The Breckenrdige Story.txt", b"x");
    put(&root, "THE BRECKENRDIGE STORY.txt", b"x");
    let (tree, _) = scan(&root);
    let cands = detect_typos(&tree, &words);
    assert_eq!(cands.len(), 2, "both case shapes flag: {cands:?}");
    let cap = cands
        .iter()
        .find(|c| c.path.ends_with("The Breckenrdige Story.txt"))
        .expect("capitalized fixture flags");
    assert!(cap.title.contains("“Breckenridge”"), "headline: {}", cap.title);
    assert!(cap.detail.contains("“Breckenridge”"), "detail: {}", cap.detail);
    assert_eq!(
        cap.proposal["to"],
        serde_json::json!("The Breckenridge Story.txt"),
        "proposal"
    );
    let upper = cands
        .iter()
        .find(|c| c.path.ends_with("THE BRECKENRDIGE STORY.txt"))
        .expect("all-caps fixture flags");
    assert!(upper.title.contains("“BRECKENRIDGE”"), "headline: {}", upper.title);
    assert!(upper.detail.contains("“BRECKENRIDGE”"), "detail: {}", upper.detail);
    assert_eq!(
        upper.proposal["to"],
        serde_json::json!("THE BRECKENRIDGE STORY.txt"),
        "proposal"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn typo_ignores_release_vocabulary() {
    // Dickles, 2026-10-15: the engine asked "Did you mean 'yes'?"
    // about the [YTS.MX] tag — and proposed renaming it to [YES] —
    // on every YTS file in the library. Release vocabulary is
    // excluded by default (NEUTRAL_TOKENS). "yes" is planted in
    // the dictionary so the pre-fix engine WOULD have asked; the
    // fixed engine must not, while a real typo still flags.
    let mut words = test_words();
    words.insert("belinda".to_string());
    words.insert("yes".to_string());
    let root = fixture("typorelease");
    put(&root, "Oh Belinda (1986) [1080p] [WEBRip] [YTS.MX].mkv", b"v");
    for n in ["The Great Movie", "The American Story", "Family Holiday Photos"] {
        put(&root, &format!("{n} (2020) [1080p] [WEBRip] [YTS.MX].mkv"), b"v");
    }
    put(&root, "The Richrd Story [YTS.MX].mkv", b"v");
    let (tree, _) = scan(&root);
    let cands = detect_typos(&tree, &words);
    assert_eq!(cands.len(), 1, "only the real typo flags: {cands:?}");
    assert!(cands[0].path.ends_with("The Richrd Story [YTS.MX].mkv"));
    let _ = std::fs::remove_dir_all(&root);
}

// ------------------------------------------------------------------
// Red Pen (2026-10-18): an orphaned "running" row is closed out as
// "interrupted"; a genuinely in-flight run is untouchable.
// ------------------------------------------------------------------

#[test]
fn stale_running_runs_are_interrupted_unless_in_flight() {
    let cat = Catalog::open_in_memory().unwrap();
    let mk = |id: &str, status: &str| AuditorRunRow {
        id: id.into(),
        schedule_id: None,
        schedule_name: "Manual audit".into(),
        target: "/nowhere".into(),
        started: "2026-10-18T03:00:00".into(),
        finished: None,
        was_late: false,
        status: status.into(),
        summary_json: "{}".into(),
    };
    cat.upsert_auditor_run(&mk("zombie", "running")).unwrap();
    cat.upsert_auditor_run(&mk("live", "running")).unwrap();
    cat.upsert_auditor_run(&mk("done", "completed")).unwrap();

    let flipped = cat
        .mark_stale_auditor_runs_interrupted(&["live".to_string()])
        .unwrap();
    assert_eq!(flipped, 1);
    let runs = cat.recent_auditor_runs(10).unwrap();
    let status_of = |id: &str| {
        runs.iter()
            .find(|r| r.id == id)
            .unwrap()
            .status
            .clone()
    };
    assert_eq!(status_of("zombie"), "interrupted");
    assert_eq!(status_of("live"), "running");
    assert_eq!(status_of("done"), "completed");

    // With nothing in flight, the remaining runner closes too.
    let flipped = cat.mark_stale_auditor_runs_interrupted(&[]).unwrap();
    assert_eq!(flipped, 1);
    let runs = cat.recent_auditor_runs(10).unwrap();
    assert_eq!(
        runs.iter().find(|r| r.id == "live").unwrap().status,
        "interrupted"
    );
}
