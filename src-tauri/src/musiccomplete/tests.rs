//! musiccomplete tests — fixture proof against synthetic audio
//! files with known tags. Fixtures are generated with ffmpeg
//! (present on the dev box; tests self-skip when it is absent so
//! the suite stays honest everywhere). Live-endpoint tests are
//! #[ignore]d and run deliberately: they prove the MusicBrainz /
//! LRCLIB / Cover Art Archive shapes against the real services.

use super::*;
use std::collections::HashMap;
use std::path::PathBuf;

fn ffmpeg() -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|d| d.join("ffmpeg"))
        .find(|p| p.is_file())
}

fn fixture_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "fable-mu-test-{}-{}",
        std::process::id(),
        tag
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A 1-second silent audio file in the requested format, tagged
/// with the given fields via ffmpeg itself (ground truth written
/// by a tool that isn't ours).
fn make_audio(dir: &Path, name: &str, tags: &[(&str, &str)]) -> Option<PathBuf> {
    let ff = ffmpeg()?;
    let out = dir.join(name);
    let mut cmd = std::process::Command::new(&ff);
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        "anullsrc=r=44100:cl=mono",
        "-t",
        "1",
    ]);
    for (k, v) in tags {
        cmd.args(["-metadata", &format!("{k}={v}")]);
    }
    // Keep ID3v2.4 off for mp3 (ffmpeg default writes v2.3+v1 fine).
    cmd.arg(&out);
    let status = cmd.status().ok()?;
    if status.success() && out.exists() {
        Some(out)
    } else {
        None
    }
}

/// A small real JPEG, made by ffmpeg (a hand-hexed "tiny JPEG"
/// fixture proved to be a transcription-error magnet).
fn tiny_jpeg(dir: &Path) -> Option<Vec<u8>> {
    let ff = ffmpeg()?;
    let out = dir.join("cover.jpg");
    let status = std::process::Command::new(&ff)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=white:s=16x16",
            "-frames:v",
            "1",
        ])
        .arg(&out)
        .status()
        .ok()?;
    if status.success() {
        std::fs::read(&out).ok().filter(|b| b.len() > 100)
    } else {
        None
    }
}

#[test]
fn tag_roundtrip_and_priors_flac_and_mp3() {
    let dir = fixture_dir("roundtrip");
    let mut proven: Vec<&str> = Vec::new();
    for name in ["a.flac", "b.mp3", "c.ogg", "d.m4a"] {
        let Some(path) = make_audio(&dir, name, &[("title", "Creep"), ("artist", "Radiohed")])
        else {
            eprintln!("ffmpeg could not make {name} — format skipped");
            continue;
        };
        let view = read_file_tags(&path).unwrap();
        assert_eq!(view.tags.get("title"), Some("Creep"), "{name}");
        assert_eq!(view.tags.get("artist"), Some("Radiohed"), "{name}");
        assert!(view.duration_secs.is_some(), "{name} duration");

        // Write fills + the correction, capture priors.
        let priors = write_tag_fields(
            &path,
            &[
                ("album".into(), Some("Pablo Honey".into())),
                ("artist".into(), Some("Radiohead".into())),
                ("track".into(), Some("2".into())),
            ],
        )
        .unwrap();
        let prior_map: HashMap<&str, &Option<String>> =
            priors.iter().map(|(k, v)| (k.as_str(), v)).collect();
        assert_eq!(prior_map["album"], &None, "album was empty");
        assert_eq!(
            prior_map["artist"].as_deref(),
            Some("Radiohed"),
            "the typo is the journal's undo payload"
        );
        let view = read_file_tags(&path).unwrap();
        assert_eq!(view.tags.get("album"), Some("Pablo Honey"));
        assert_eq!(view.tags.get("artist"), Some("Radiohead"));
        assert_eq!(view.tags.get("track"), Some("2"));

        // Clearing works and reports the prior.
        let priors = write_tag_fields(&path, &[("album".into(), None)]).unwrap();
        assert_eq!(priors[0].1.as_deref(), Some("Pablo Honey"));
        assert_eq!(read_file_tags(&path).unwrap().tags.get("album"), None);
        proven.push(name);
    }
    // The promised minimum: FLAC and MP3 must be fixture-proven
    // wherever ffmpeg exists at all.
    assert!(
        proven.contains(&"a.flac") && proven.contains(&"b.mp3"),
        "only proven: {proven:?}"
    );
    eprintln!("tag roundtrip proven for: {proven:?}");
}

#[test]
fn picture_content_standard() {
    // Empty and garbage blocks are not cover art, at any size.
    assert!(!picture_has_content(&[]));
    assert!(!picture_has_content(&[7u8; 900]));
    // A 1x1 placeholder GIF (43 bytes, valid magic) is not art.
    let mut gif = b"GIF89a".to_vec();
    gif.resize(43, 0);
    assert!(!picture_has_content(&gif));
    // Real magic at a sane size counts.
    let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0];
    jpeg.resize(600, 1);
    assert!(picture_has_content(&jpeg));
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    png.resize(600, 1);
    assert!(picture_has_content(&png));
}

/// Regression (Dickles, 2026-10-15): empty embedded fields used
/// to count as content — an empty picture block read as "cover
/// art already in the file" and empty/header-only lyrics as
/// "lyrics already saved". Presence now requires content.
#[test]
fn empty_fields_do_not_count_as_content() {
    use lofty::prelude::*;
    let dir = fixture_dir("emptyfields");
    let Some(path) = make_audio(&dir, "song.flac", &[("title", "Creep")]) else {
        eprintln!("ffmpeg unavailable — skipped");
        return;
    };
    let push_picture = |path: &Path, data: Vec<u8>| {
        let mut file = lofty::read_from_path(path).unwrap();
        let tag = file.primary_tag_mut().unwrap();
        tag.push_picture(lofty::picture::Picture::new_unchecked(
            lofty::picture::PictureType::CoverFront,
            Some(lofty::picture::MimeType::Jpeg),
            None,
            data,
        ));
        file.save_to_path(path, lofty::config::WriteOptions::default())
            .unwrap();
    };
    let set_lyrics = |path: &Path, text: &str| {
        let mut file = lofty::read_from_path(path).unwrap();
        let tag = file.primary_tag_mut().unwrap();
        tag.insert_text(lofty::tag::ItemKey::Lyrics, text.to_string());
        file.save_to_path(path, lofty::config::WriteOptions::default())
            .unwrap();
    };

    // An empty picture block: not art. Neither is a big garbage one.
    push_picture(&path, Vec::new());
    assert!(!read_file_tags(&path).unwrap().has_art, "empty picture block counted as art");
    push_picture(&path, vec![7u8; 900]);
    assert!(!read_file_tags(&path).unwrap().has_art, "garbage picture counted as art");

    // Empty / whitespace / header-only / stamps-only lyrics: none
    // of them are "lyrics already saved".
    for shell in [
        "",
        "   \n  ",
        "[ti:Creep]\n[ar:Radiohead]\n[by:Ripper]\n",
        "[00:12.00]\n[00:17.50] \n",
    ] {
        set_lyrics(&path, shell);
        let view = read_file_tags(&path).unwrap();
        assert!(
            view.lyrics_plain.is_none() && view.lyrics_synced.is_none(),
            "shell lyrics counted as content: {shell:?}"
        );
    }
    // Header + real plain lines (the SpotiFLAC shape) DO count.
    set_lyrics(&path, "[ti:Creep]\n[ar:Radiohead]\nWhen you were here before\n");
    let view = read_file_tags(&path).unwrap();
    assert!(view.lyrics_plain.is_some(), "real plain lyrics not seen");
    // Real synced lines count as synced.
    set_lyrics(&path, "[00:12.00]When you were here before\n");
    let view = read_file_tags(&path).unwrap();
    assert!(view.lyrics_synced.is_some(), "real synced lyrics not seen");

    // embed_art repairs a garbage-only file instead of deferring
    // to the placeholder, and then the real art wins as usual.
    let Some(jpeg) = tiny_jpeg(&dir) else {
        eprintln!("ffmpeg could not make a JPEG — skipped");
        return;
    };
    assert!(embed_art(&path, &jpeg).unwrap(), "embed deferred to garbage picture");
    assert!(read_file_tags(&path).unwrap().has_art);
    assert!(!embed_art(&path, &jpeg).unwrap(), "real art must win");
}

#[test]
fn sidecar_shells_do_not_count_as_lyrics() {
    let dir = fixture_dir("sidecarshell");
    let audio = dir.join("track.flac");
    std::fs::write(&audio, b"not audio - sidecar_text never reads it").unwrap();
    let sc = sidecar_path(&audio);
    std::fs::write(&sc, "[ti:Only A Header]\n[ar:Someone]\n").unwrap();
    assert!(sidecar_text(&audio).is_none(), "header-only sidecar counted");
    std::fs::write(&sc, "  \n").unwrap();
    assert!(sidecar_text(&audio).is_none(), "blank sidecar counted");
    std::fs::write(&sc, "real words someone saved\n").unwrap();
    assert!(sidecar_text(&audio).is_some(), "real sidecar not seen");
}

#[test]
fn artwork_embeds_only_when_absent_and_undoes() {
    let dir = fixture_dir("art");
    let Some(path) = make_audio(&dir, "song.flac", &[("title", "Creep")]) else {
        eprintln!("ffmpeg unavailable — skipped");
        return;
    };
    let Some(jpeg) = tiny_jpeg(&dir) else {
        eprintln!("ffmpeg could not make a JPEG — skipped");
        return;
    };
    assert!(!read_file_tags(&path).unwrap().has_art);
    assert!(embed_art(&path, &jpeg).unwrap());
    assert!(read_file_tags(&path).unwrap().has_art);
    // Existing art wins: a second embed is a no-op.
    assert!(!embed_art(&path, &jpeg).unwrap());
    remove_embedded_art(&path).unwrap();
    assert!(!read_file_tags(&path).unwrap().has_art);
}

#[test]
fn apply_journals_priors_and_undo_restores_them() {
    let dir = fixture_dir("undo");
    let Some(path) = make_audio(
        &dir,
        "track.flac",
        &[("title", "Creep"), ("artist", "Radiohed")],
    ) else {
        eprintln!("ffmpeg unavailable — skipped");
        return;
    };
    let catalog = Catalog::open_in_memory().unwrap();
    let settings = MuSettings::default(); // lyrics target "both"
    let lrc_text = "[00:01.00]When you were here before\n";
    let item = ApplyItem {
        path: path.to_string_lossy().into_owned(),
        fields: vec![
            ApplyField {
                field: "album".into(),
                value: Some("Pablo Honey".into()),
            },
            ApplyField {
                field: "artist".into(),
                value: Some("Radiohead".into()),
            },
        ],
        art_release_id: None,
        art_release_group_id: None,
        lyrics_plain: None,
        lyrics_synced: Some(lrc_text.into()),
    };
    let row = apply_core(
        &catalog,
        "Manual completion",
        None,
        &dir.to_string_lossy(),
        &[item],
        Vec::new(),
        &settings,
        false,
    )
    .unwrap();
    assert_eq!(row.status, "completed");
    // Effects landed: tags written, lyrics embedded + sidecar.
    let view = read_file_tags(&path).unwrap();
    assert_eq!(view.tags.get("artist"), Some("Radiohead"));
    assert_eq!(view.tags.get("album"), Some("Pablo Honey"));
    assert!(view.lyrics_synced.is_some());
    let sc = sidecar_path(&path);
    assert!(sc.exists());
    // The journal holds the priors.
    let actions: Vec<serde_json::Value> =
        serde_json::from_str(&row.actions_json).unwrap();
    let tags_action = actions
        .iter()
        .find(|a| a["kind"] == "tags")
        .expect("tags action journaled");
    assert_eq!(tags_action["prior"]["artist"], "Radiohed");
    assert!(tags_action["prior"]["album"].is_null());

    // Undo restores the typo, clears the fill, drops the lyrics
    // and the sidecar.
    let msg = undo_run_with(&catalog, &row.id).unwrap();
    assert!(msg.contains("reversed"), "{msg}");
    let view = read_file_tags(&path).unwrap();
    assert_eq!(view.tags.get("artist"), Some("Radiohed"));
    assert_eq!(view.tags.get("album"), None);
    assert!(view.lyrics_synced.is_none() && view.lyrics_plain.is_none());
    assert!(!sc.exists());
    // And a second undo is refused honestly.
    assert!(undo_run_with(&catalog, &row.id).is_err());
}

#[test]
fn existing_sidecar_is_never_overwritten() {
    let dir = fixture_dir("sidecar");
    let Some(path) = make_audio(&dir, "track.flac", &[("title", "Creep")]) else {
        eprintln!("ffmpeg unavailable — skipped");
        return;
    };
    let sc = sidecar_path(&path);
    std::fs::write(&sc, "MY OWN LYRICS").unwrap();
    let catalog = Catalog::open_in_memory().unwrap();
    let mut settings = MuSettings::default();
    settings.lyrics_target = "sidecar".into();
    let item = ApplyItem {
        path: path.to_string_lossy().into_owned(),
        fields: Vec::new(),
        art_release_id: None,
        art_release_group_id: None,
        lyrics_plain: Some("fetched words".into()),
        lyrics_synced: None,
    };
    let row = apply_core(
        &catalog,
        "Manual completion",
        None,
        &dir.to_string_lossy(),
        &[item],
        Vec::new(),
        &settings,
        false,
    )
    .unwrap();
    assert_eq!(std::fs::read_to_string(&sc).unwrap(), "MY OWN LYRICS");
    let actions: Vec<serde_json::Value> =
        serde_json::from_str(&row.actions_json).unwrap();
    assert!(
        actions.iter().all(|a| a["kind"] != "lyrics-sidecar"),
        "no sidecar action may be journaled for a file we didn't write"
    );
}

#[test]
fn lyrics_save_is_journaled() {
    let dir = fixture_dir("lyrsave");
    let Some(path) = make_audio(&dir, "track.mp3", &[("title", "Creep")]) else {
        eprintln!("ffmpeg unavailable — skipped");
        return;
    };
    let catalog = Catalog::open_in_memory().unwrap();
    let mut settings = MuSettings::default();
    settings.lyrics_target = "embedded".into();
    let msg = lyrics_save_with(
        &catalog,
        &path,
        Some("hand typed words"),
        &settings,
    )
    .unwrap();
    assert!(msg.contains("saved"), "{msg}");
    let view = lyrics_view(&path, false).unwrap();
    assert_eq!(view.embedded_plain.as_deref(), Some("hand typed words"));
    // Clearing is journaled too — and the run can undo the clear.
    lyrics_save_with(&catalog, &path, None, &settings).unwrap();
    assert!(lyrics_view(&path, false).unwrap().embedded_plain.is_none());
    let runs = catalog.recent_musiccomplete_runs(5).unwrap();
    let clear_run = runs
        .iter()
        .find(|r| r.actions_json.contains("hand typed words"))
        .expect("the clear run journaled the prior text");
    undo_run_with(&catalog, &clear_run.id).unwrap();
    assert_eq!(
        lyrics_view(&path, false).unwrap().embedded_plain.as_deref(),
        Some("hand typed words")
    );
}

#[test]
fn schedule_adapter_reuses_the_tested_state_machine() {
    let sched = MuSchedule {
        id: "s1".into(),
        name: "Nightly music".into(),
        target: "/music".into(),
        steps: MuSteps::default(),
        time: "02:30".into(),
        weekdays: Vec::new(),
        idle_minutes: 0,
        require_on_ac: false,
        enabled: true,
        last_started: None,
        last_status: None,
    };
    let mc = sched.as_mc();
    // 03:00 on an arbitrary day: the 02:30 slot is owed.
    let now = chrono::NaiveDateTime::parse_from_str("2026-10-15T03:00:00", "%Y-%m-%dT%H:%M:%S")
        .unwrap();
    assert!(matches!(
        crate::mediacomplete::evaluate(&mc, now),
        crate::mediacomplete::Decision::Due { .. }
    ));
}

// ---------------------------------------------------------------------------
// Live endpoint proofs (run deliberately with --ignored on a
// networked box). These assert the real services' shapes.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "live MusicBrainz — run on the box"]
fn live_musicbrainz_search_and_lookup() {
    let client = MbClient::new();
    let local = LocalInfo {
        title: Some("Creep".into()),
        artist: Some("Radiohead".into()),
        album: None,
        duration_secs: Some(239.0),
    };
    let mut cands = client.search(&local).expect("MusicBrainz search");
    assert!(!cands.is_empty(), "no candidates for Creep");
    let mut best = cands.remove(0);
    let (score, _ev) = musicid::score_candidate(&local, &best);
    assert!(score >= 0.8, "top candidate scored {score}");
    client.lookup(&mut best).expect("MusicBrainz lookup");
    assert_eq!(best.title, "Creep");
    assert!(best.release_id.is_some());
}

#[test]
#[ignore = "live LRCLIB — run on the box"]
fn live_lrclib_get() {
    let client = LrclibClient::new();
    let hit = client
        .fetch("Radiohead", "Creep", Some("Pablo Honey"), Some(239.0))
        .expect("LRCLIB fetch")
        .expect("LRCLIB has Creep");
    let plain = hit.plain.expect("plain lyrics present");
    assert!(plain.to_lowercase().contains("creep"));
}

#[test]
#[ignore = "live Cover Art Archive (chains off MusicBrainz) — run on the box"]
fn live_cover_art_chain() {
    let client = MbClient::new();
    let local = LocalInfo {
        title: Some("Creep".into()),
        artist: Some("Radiohead".into()),
        album: Some("Pablo Honey".into()),
        duration_secs: Some(239.0),
    };
    // MusicBrainz answers 503 ("busy") under load bursts; the
    // chain retries politely rather than calling that a failure
    // of the archive.
    let mut cands = Vec::new();
    for attempt in 0..4 {
        match client.search(&local) {
            Ok(c) if !c.is_empty() => {
                cands = c;
                break;
            }
            _ if attempt < 3 => {
                std::thread::sleep(std::time::Duration::from_secs(15));
            }
            _ => {}
        }
    }
    let cand = cands.into_iter().next().expect("a candidate");
    let bytes = fetch_cover_art(
        cand.release_id.as_deref(),
        cand.release_group_id.as_deref(),
    )
    .expect("cover art bytes");
    assert!(bytes.len() > 1024);
    assert!(
        bytes.starts_with(&[0xFF, 0xD8]) || bytes.starts_with(&[0x89, b'P', b'N', b'G']),
        "not an image"
    );
}
