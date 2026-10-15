//! Catalog-hygiene PROOF harness (Indexer v2) — NOT a gate test.
//!
//! Runs the terminal-job slimming + VACUUM against a COPY of a real
//! catalog and proves the operation is lossless for everything live:
//! item counts identical, unfinished jobs byte-identical, terminal
//! jobs' summaries/audits unchanged (only step payloads dropped).
//!
//! Skips unless FABLE_TEST_CATALOG_COPY points at a catalog.db copy:
//!   FABLE_TEST_CATALOG_COPY=/tmp/hygiene-proof/catalog.db \
//!     cargo test -p ufm-catalog --test hygiene_proof -- --nocapture

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use ufm_catalog::Catalog;

fn hash_str(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

struct JobSnap {
    state: String,
    blob_hash: u64,
    blob_len: usize,
    title: String,
    created: String,
    outcomes: usize,
    steps: usize,
    bytes_to_move: u64,
}

fn snapshot(conn: &rusqlite::Connection) -> (i64, i64, Vec<(String, JobSnap)>) {
    let items: i64 = conn
        .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
        .unwrap();
    let fts: i64 = conn
        .query_row("SELECT COUNT(*) FROM items_fts", [], |r| r.get(0))
        .unwrap();
    let mut stmt = conn
        .prepare("SELECT job_id, state, job_json FROM jobs ORDER BY job_id")
        .unwrap();
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .unwrap();
    let mut jobs = Vec::new();
    for row in rows {
        let (id, state, json) = row.unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        jobs.push((
            id,
            JobSnap {
                state,
                blob_hash: hash_str(&json),
                blob_len: json.len(),
                title: v["title"].as_str().unwrap_or("").to_string(),
                created: v["created"].as_str().unwrap_or("").to_string(),
                outcomes: v["outcomes"].as_array().map(|a| a.len()).unwrap_or(0),
                steps: v["steps"].as_object().map(|o| o.len()).unwrap_or(0),
                bytes_to_move: v["preview"]["bytes_to_move"].as_u64().unwrap_or(0),
            },
        ));
    }
    (items, fts, jobs)
}

#[test]
fn hygiene_proof_on_real_catalog_copy() {
    let Ok(path) = std::env::var("FABLE_TEST_CATALOG_COPY") else {
        eprintln!("hygiene_proof: FABLE_TEST_CATALOG_COPY unset — skipping");
        return;
    };
    let db_file = std::path::PathBuf::from(&path);
    let file_len = |p: &std::path::Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    let wal_path = db_file.with_extension("db-wal");
    let before_files = file_len(&db_file) + file_len(&wal_path);

    let catalog = Catalog::open(&db_file).unwrap();
    assert_eq!(catalog.integrity_check().unwrap(), "ok");
    let raw = rusqlite::Connection::open(&db_file).unwrap();
    let (items_before, fts_before, jobs_before) = snapshot(&raw);
    let size_before = catalog.database_size_bytes().unwrap();
    let unfinished_before = catalog.unfinished_jobs().unwrap().len();
    eprintln!(
        "PROOF BEFORE: files={} db_pages_bytes={} items={} fts={} jobs={} unfinished={}",
        before_files,
        size_before,
        items_before,
        fts_before,
        jobs_before.len(),
        unfinished_before
    );
    for (id, j) in &jobs_before {
        eprintln!(
            "  job {} state={} blob={} steps={} outcomes={}",
            &id[..8],
            j.state,
            j.blob_len,
            j.steps,
            j.outcomes
        );
    }

    let t0 = std::time::Instant::now();
    let report = catalog.slim_terminal_job_blobs().unwrap();
    catalog.vacuum().unwrap();
    let elapsed = t0.elapsed();

    let (items_after, fts_after, jobs_after) = snapshot(&raw);
    let size_after = catalog.database_size_bytes().unwrap();
    let after_files = file_len(&db_file) + file_len(&wal_path);
    let unfinished_after = catalog.unfinished_jobs().unwrap().len();
    eprintln!(
        "PROOF AFTER: files={} db_pages_bytes={} items={} fts={} jobs={} unfinished={} slimmed={} freed={} in {:?}",
        after_files,
        size_after,
        items_after,
        fts_after,
        jobs_after.len(),
        unfinished_after,
        report.jobs_slimmed,
        report.bytes_freed,
        elapsed
    );
    assert_eq!(catalog.integrity_check().unwrap(), "ok");

    // Lossless for live data:
    assert_eq!(items_before, items_after, "item rows changed");
    assert_eq!(fts_before, fts_after, "FTS rows changed");
    assert_eq!(jobs_before.len(), jobs_after.len(), "job rows changed");
    assert_eq!(unfinished_before, unfinished_after);
    let after_map: std::collections::HashMap<&str, &JobSnap> =
        jobs_after.iter().map(|(id, j)| (id.as_str(), j)).collect();
    for (id, before) in &jobs_before {
        let after = after_map[id.as_str()];
        assert_eq!(before.state, after.state, "job {id} state changed");
        assert_eq!(before.title, after.title, "job {id} title changed");
        assert_eq!(before.created, after.created, "job {id} created changed");
        assert_eq!(before.outcomes, after.outcomes, "job {id} audit changed");
        assert_eq!(
            before.bytes_to_move, after.bytes_to_move,
            "job {id} summary changed"
        );
        if before.state == "completed"
            || before.state == "cancelled"
            || before.state == "failed"
        {
            // Terminal: blob slimmed (or already minimal), step
            // payload gone, tallies recorded in its place.
            assert!(
                after.blob_len <= before.blob_len,
                "job {id} terminal blob GREW"
            );
        } else {
            // Unfinished: byte-for-byte untouchable.
            assert_eq!(
                before.blob_hash, after.blob_hash,
                "UNFINISHED job {id} blob changed — slimming is NOT lossless"
            );
            assert_eq!(before.blob_len, after.blob_len);
        }
    }
    eprintln!("PROOF VERDICT: lossless — live rows identical, terminal blobs slimmed");
}
