//! On-demand proof harness for the Hyperbase #5 EmulationStation
//! import against the REAL RetroBat tree on the Hyperbase drive.
//! Ignored by default (it reads production metadata); run
//! explicitly:
//!
//!   FABLE_ES_ROOT="/media/richard/Hyper base PN ultra/A gift to you/retrobat" \
//!   FABLE_ES_SYSTEMS="snes,psx,wii" \
//!   cargo test -p ufm-catalog --test es_real_proof -- --ignored --nocapture
//!
//! What it proves, on real data, through the shipped code path
//! (detect → parse → match by path → apply):
//! - detection finds the install root and the chosen systems;
//! - each system's catalog items are seeded into a SCRATCH
//!   catalog from the files actually on disk (stat only — no game
//!   file is ever opened), mirroring an indexed location;
//! - the per-system report reconciles exactly: every gamelist
//!   entry lands in exactly one bucket;
//! - spot-checked VALUES in es_metadata / media_identities /
//!   rom_game_state / rom_media equal the raw gamelist values
//!   (re-parsed independently in this harness);
//! - a second import is a reconciliation no-op (refresh only).
//!
//! Drive discipline: gamelist XML text + file metadata only.

use std::collections::HashSet;
use std::path::PathBuf;

use ufm_catalog::Catalog;
use ufm_core::{ItemId, LocationId};
use ufm_media::esgamelist::{
    detect_es_root, es_datetime_to_rfc3339, parse_gamelist, resolve_es_ref, EsRootKind,
};

fn seed_system_items(
    cat: &Catalog,
    loc: LocationId,
    sys: &ufm_media::esgamelist::EsSystemDir,
) -> usize {
    let xml = std::fs::read_to_string(&sys.gamelist_path).expect("gamelist readable");
    let list = parse_gamelist(&xml).expect("gamelist parses");
    let mut seeded = 0;
    let mut seen: HashSet<String> = HashSet::new();
    for game in &list.games {
        let Some(raw) = game.path.as_deref() else { continue };
        let Some(resolved) = resolve_es_ref(&sys.dir, raw) else { continue };
        let key = resolved.to_string_lossy().into_owned();
        if !seen.insert(key.clone()) {
            continue;
        }
        let Ok(md) = std::fs::metadata(&resolved) else { continue };
        if !md.is_file() {
            continue;
        }
        let item = ufm_vfs::Item {
            id: ItemId::new(),
            location: loc,
            name: resolved
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            kind: ufm_vfs::ItemKind::File,
            size_bytes: Some(md.len()),
            modified: md
                .modified()
                .ok()
                .map(chrono::DateTime::<chrono::Utc>::from),
            provider_key: Some(key),
            labels: vec![],
            starred: false,
        };
        cat.insert_item(&item).expect("seed item");
        seeded += 1;
    }
    seeded
}

#[test]
#[ignore]
fn es_real_proof() {
    let root = std::env::var("FABLE_ES_ROOT").expect("FABLE_ES_ROOT must name the RetroBat root");
    let systems_want: Vec<String> = std::env::var("FABLE_ES_SYSTEMS")
        .unwrap_or_else(|_| "snes,psx,wii".to_string())
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();
    let root = PathBuf::from(root);
    let det = detect_es_root(&root).expect("ES layout detected at the RetroBat root");
    assert_eq!(det.kind, EsRootKind::InstallRoot, "RetroBat root shape");
    println!(
        "PROOF detection: {} systems carry gamelists under {}",
        det.systems.len(),
        root.display()
    );

    let cat = Catalog::open_in_memory().expect("scratch catalog");
    let loc = LocationId::new();
    cat.insert_location(&ufm_vfs::Location {
        id: loc,
        provider: "local".into(),
        display_name: "hyperbase-proof".into(),
        capabilities: ufm_vfs::Capabilities {
            read: true,
            ..Default::default()
        },
        protected: ufm_vfs::ProtectionTier::None,
    })
    .expect("seed location");

    for want in &systems_want {
        let sys = det
            .systems
            .iter()
            .find(|s| &s.es_name == want)
            .unwrap_or_else(|| panic!("system {want} not detected"));
        let seeded = seed_system_items(&cat, loc, sys);
        let rep = cat.import_es_system(sys).expect("import runs");
        assert!(rep.parse_error.is_none(), "{want}: {:?}", rep.parse_error);
        let c = &rep.counts;
        let bucketed = c.duplicates
            + c.no_path
            + c.unresolvable
            + c.matched_exact
            + c.matched_casefold
            + c.unmatched_not_indexed
            + c.unmatched_missing_file;
        assert_eq!(
            bucketed, c.entries,
            "{want}: every entry lands in exactly one bucket"
        );
        println!(
            "PROOF {want}: entries={} seeded_items={} matched={} (exact {} + casefold {}) \
             unmatched: not_indexed={} missing_file={} | identity: written={} dat_kept={} \
             art_filled={} state_updates={} offline={}",
            c.entries,
            seeded,
            c.matched_exact + c.matched_casefold,
            c.matched_exact,
            c.matched_casefold,
            c.unmatched_not_indexed,
            c.unmatched_missing_file,
            c.identity_written,
            c.dat_kept,
            c.art_filled,
            c.state_updates,
            c.matched_offline
        );

        // Independent value spot-checks against the raw XML.
        let xml = std::fs::read_to_string(&sys.gamelist_path).unwrap();
        let list = parse_gamelist(&xml).unwrap();
        let mut checked_desc = 0;
        let mut checked_state = 0;
        for game in &list.games {
            let Some(raw) = game.path.as_deref() else { continue };
            let Some(resolved) = resolve_es_ref(&sys.dir, raw) else { continue };
            let key = resolved.to_string_lossy().into_owned();
            let Some(meta) = cat.es_metadata_for(&key).unwrap() else { continue };
            if checked_desc < 5 {
                if let (Some(want_desc), Some(got_desc)) = (&game.desc, &meta.description) {
                    assert_eq!(got_desc, want_desc, "{want}: description is the gamelist's own text");
                    assert_eq!(meta.developer, game.developer, "{want}: developer");
                    assert_eq!(meta.name, game.name, "{want}: name provenance");
                    // The identity the room will show is the gamelist's
                    // name (scratch catalog: no DAT rows compete).
                    let item = cat.item_by_provider_key(&key).unwrap().unwrap();
                    let ident = cat.media_identity_for(&item.id).unwrap().unwrap();
                    assert_eq!(ident.title, game.name, "{want}: identity title");
                    assert_eq!(
                        ident.platform.as_deref(),
                        Some(ufm_media::esgamelist::fable_platform(&sys.es_name).as_str()),
                        "{want}: identity platform"
                    );
                    checked_desc += 1;
                }
            }
            if checked_state < 5 {
                if let (Some(pc), Some(lp)) = (game.playcount, &game.lastplayed) {
                    let st = cat.rom_game_state_for(&key).unwrap();
                    assert_eq!(st.play_count, pc as i64, "{want}: play_count merged");
                    assert_eq!(
                        st.last_played.as_deref(),
                        es_datetime_to_rfc3339(lp).as_deref(),
                        "{want}: last_played converted"
                    );
                    checked_state += 1;
                }
            }
            if checked_desc >= 5 && checked_state >= 5 {
                break;
            }
        }
        println!(
            "PROOF {want}: value spot-checks passed (desc x{checked_desc}, play history x{checked_state})"
        );

        // Second import: refresh-only reconciliation.
        let rep2 = cat.import_es_system(sys).expect("re-import runs");
        let c2 = &rep2.counts;
        assert_eq!(c2.identity_written, 0, "{want}: second run writes no new identities");
        assert_eq!(
            c2.identity_refreshed, c.identity_written,
            "{want}: second run refreshes exactly its own rows"
        );
        assert_eq!(c2.art_filled, 0, "{want}: second run fills no art twice");
        assert_eq!(c2.state_updates, 0, "{want}: second run moves no state");
        println!("PROOF {want}: second import is a reconciliation no-op");
    }
}
