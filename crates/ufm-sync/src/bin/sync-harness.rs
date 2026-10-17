//! sync-harness: drives the ufm-sync engine against real folders,
//! for scripted proofs and recovery drills.
//!
//!   sync-harness plan <state_dir> <root_a> <root_b>
//!   sync-harness run  <state_dir> <root_a> <root_b> [--die-after N]
//!
//! state_dir holds manifest.json (the pair base) and report.json
//! (the last run's report). --die-after N aborts the process after N
//! committed actions — a power-cut simulator. Re-running `run` after
//! a crash replans from the partially committed manifest and disk
//! truth, and must converge with no duplicates and no loss.

use std::path::{Path, PathBuf};
use ufm_sync::{exec, plan, Manifest};

fn stamp_now() -> String {
    ufm_core::now_utc().format("%Y-%m-%d-%H%M%S").to_string()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!("usage: sync-harness <plan|run> <state_dir> <root_a> <root_b> [--die-after N]");
        std::process::exit(2);
    }
    let mode = args[1].as_str();
    let state = PathBuf::from(&args[2]);
    let root_a = PathBuf::from(&args[3]);
    let root_b = PathBuf::from(&args[4]);
    let die_after: Option<usize> = args
        .windows(2)
        .find(|w| w[0] == "--die-after")
        .map(|w| w[1].parse().expect("--die-after takes a number"));

    let manifest_path = state.join("manifest.json");
    let manifest = Manifest::load(&manifest_path).expect("load manifest");
    let p = plan(&root_a, &root_b, &manifest, stamp_now()).expect("plan");
    match mode {
        "plan" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&p).expect("serialize plan")
            );
        }
        "run" => {
            let started = ufm_core::now_utc().to_rfc3339();
            let mut report = exec::run_plan(
                &root_a,
                &root_b,
                &manifest_path,
                "harness-pair",
                "Harness Pair",
                &p,
                started,
                die_after,
            )
            .expect("run plan");
            report.finished = Some(ufm_core::now_utc().to_rfc3339());
            ufm_sync::save_json_atomic(&state.join("report.json"), &report)
                .expect("save report");
            // Summary on stdout; the full JSON lives in report.json
            // (printing it here panics on EPIPE when piped to grep).
            println!("SUMMARY: {}", report.summary());
            if args.iter().any(|a| a == "--json") {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).expect("serialize report")
                );
            }
        }
        other => {
            eprintln!("unknown mode: {other}");
            std::process::exit(2);
        }
    }
    let _ = Path::new(""); // keep Path import honest across feature sets
}
