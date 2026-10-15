//! mediacomplete tests: the scheduler state machine (due / missed /
//! catch-up / weekday masks), artwork placement, subtitle naming,
//! and the settings round trip. No network, no keyring, no systemd.

use super::*;

fn sched(time: &str, last_started: Option<&str>) -> Schedule {
    Schedule {
        id: "s1".into(),
        name: "Overnight films".into(),
        target: "/media/films".into(),
        steps: ScheduleSteps::default(),
        time: time.into(),
        weekdays: vec![],
        idle_minutes: 0,
        require_on_ac: false,
        enabled: true,
        last_started: last_started.map(|s| s.to_string()),
        last_status: None,
    }
}

fn at(s: &str) -> chrono::NaiveDateTime {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").unwrap()
}

#[test]
fn due_exactly_at_and_after_the_slot() {
    // Yesterday's run already happened, so the ONLY slot that can
    // come due today is today's — this isolates the boundary. (A
    // never-run schedule would owe yesterday's slot instead: that
    // is catch-up, covered by its own test.)
    let s = sched("02:30", Some("2026-10-14T02:30:00"));
    assert_eq!(evaluate(&s, at("2026-10-15T02:29:00")), Decision::NotDue);
    assert_eq!(
        evaluate(&s, at("2026-10-15T02:30:00")),
        Decision::Due { late: false }
    );
    // Ten minutes of grace, then the run admits it is late.
    assert_eq!(
        evaluate(&s, at("2026-10-15T02:40:00")),
        Decision::Due { late: false }
    );
    assert_eq!(
        evaluate(&s, at("2026-10-15T02:41:00")),
        Decision::Due { late: true }
    );
}

#[test]
fn a_started_slot_is_not_owed_again() {
    let s = sched("02:30", Some("2026-10-15T02:30:05"));
    assert_eq!(evaluate(&s, at("2026-10-15T03:00:00")), Decision::NotDue);
    // Tomorrow's slot is in the future: still not due today.
    assert_eq!(evaluate(&s, at("2026-10-15T23:00:00")), Decision::NotDue);
}

#[test]
fn catch_up_after_downtime_is_one_late_run() {
    // Machine was off for two nights: exactly the newest missed
    // slot is owed, flagged late — not a run per missed night.
    let s = sched("02:30", Some("2026-10-12T02:30:00"));
    let slot = owed_slot(&s, at("2026-10-15T09:00:00")).unwrap();
    assert_eq!(slot, at("2026-10-15T02:30:00"));
    assert_eq!(
        evaluate(&s, at("2026-10-15T09:00:00")),
        Decision::Due { late: true }
    );
    // And before today's slot on a normal morning, yesterday's
    // already-run slot leaves nothing owed.
    let s2 = sched("02:30", Some("2026-10-14T02:31:00"));
    assert_eq!(evaluate(&s2, at("2026-10-15T01:00:00")), Decision::NotDue);
}

#[test]
fn weekday_masks_and_disabled() {
    // 2026-10-15 is a Thursday. Mask: Mondays only (index 0).
    let mut s = sched("02:30", None);
    s.weekdays = vec![true, false, false, false, false, false, false];
    // Non-Monday days generate no slot of their own; the most
    // recent Monday slot (2026-10-12) is the owed one — catch-up
    // reaches it on Thursday, flagged late.
    assert_eq!(
        owed_slot(&s, at("2026-10-15T09:00:00")),
        Some(at("2026-10-12T02:30:00"))
    );
    assert_eq!(
        evaluate(&s, at("2026-10-15T09:00:00")),
        Decision::Due { late: true }
    );
    // Once Monday's run has started, nothing is owed until the
    // next Monday — whose slot next_slot reports.
    s.last_started = Some("2026-10-12T02:30:00".into());
    assert_eq!(owed_slot(&s, at("2026-10-15T09:00:00")), None);
    assert_eq!(
        next_slot(&s, at("2026-10-15T09:00:00")),
        Some(at("2026-10-19T02:30:00"))
    );
    s.enabled = false;
    assert_eq!(owed_slot(&s, at("2026-10-15T09:00:00")), None);
    assert_eq!(next_slot(&s, at("2026-10-15T09:00:00")), None);
}

#[test]
fn next_slot_points_forward() {
    let s = sched("02:30", None);
    assert_eq!(
        next_slot(&s, at("2026-10-15T01:00:00")),
        Some(at("2026-10-15T02:30:00"))
    );
    assert_eq!(
        next_slot(&s, at("2026-10-15T03:00:00")),
        Some(at("2026-10-16T02:30:00"))
    );
}

#[test]
fn artwork_placement_follows_the_server_layout() {
    let movie = Path::new("/lib/Dune (2021)/Dune (2021).mkv");
    let t = artwork_targets(movie, "movie", true);
    assert_eq!(
        t.iter().map(|x| x.path.to_string_lossy().into_owned()).collect::<Vec<_>>(),
        vec!["/lib/Dune (2021)/poster.jpg", "/lib/Dune (2021)/fanart.jpg"]
    );
    let ep = Path::new("/lib/Severance (2022)/Season 01/Severance (2022) - S01E01.mkv");
    let t = artwork_targets(ep, "episode", true);
    assert_eq!(
        t.iter().map(|x| (x.path.to_string_lossy().into_owned(), x.kind)).collect::<Vec<_>>(),
        vec![
            ("/lib/Severance (2022)/poster.jpg".into(), "poster"),
            ("/lib/Severance (2022)/fanart.jpg".into(), "fanart"),
            ("/lib/Severance (2022)/Season 01/poster.jpg".into(), "season"),
        ]
    );
    // Flat episode (no Season folder): just a poster beside it.
    let flat = Path::new("/lib/loose/episode.mkv");
    let t = artwork_targets(flat, "episode", false);
    assert_eq!(t.len(), 1);
    assert_eq!(t[0].path.to_string_lossy(), "/lib/loose/poster.jpg");
}

#[test]
fn subtitle_names_are_server_sidecars() {
    let v = Path::new("/lib/Dune (2021)/Dune (2021).mkv");
    assert_eq!(
        subtitle_target(v, "en", false, false).to_string_lossy(),
        "/lib/Dune (2021)/Dune (2021).en.srt"
    );
    assert_eq!(
        subtitle_target(v, "en", true, false).to_string_lossy(),
        "/lib/Dune (2021)/Dune (2021).en.sdh.srt"
    );
    assert_eq!(
        subtitle_target(v, "en", false, true).to_string_lossy(),
        "/lib/Dune (2021)/Dune (2021).en.forced.srt"
    );
}

#[test]
fn existing_subtitles_are_detected_and_respected() {
    let dir = std::env::temp_dir().join(format!("fable-mc-subs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let v = dir.join("Film (2020).mkv");
    std::fs::write(&v, b"x").unwrap();
    assert!(!has_subtitle(&v, "en"));
    std::fs::write(dir.join("Film (2020).en.srt"), b"1\n").unwrap();
    assert!(has_subtitle(&v, "en"));
    assert!(!has_subtitle(&v, "es"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn settings_round_trip_with_schedules() {
    let mut s = McSettings::default();
    s.schedules.push(sched("03:00", None));
    let text = serde_json::to_string(&s).unwrap();
    let back: McSettings = serde_json::from_str(&text).unwrap();
    assert_eq!(back.schedules.len(), 1);
    assert_eq!(back.schedules[0].time, "03:00");
    assert!(back.artwork_on_organize);
    assert!(!back.subtitles_on_organize);
    assert_eq!(back.subtitle_language, "en");
}
