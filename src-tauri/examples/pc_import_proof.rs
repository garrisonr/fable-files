//! PC Games P1 proof harness (not shipped): drives the import /
//! folder-scan engines from src-tauri/src/pcgames.rs — the exact
//! code the app's commands run — against a chosen catalog, media
//! root and library root, printing the report as JSON.
//!
//! usage:
//!   pc_import_proof import <catalog.db> <media_root> <root>
//!   pc_import_proof scan   <catalog.db> <media_root> <folder>
//!   pc_import_proof counts <catalog.db>

use std::path::PathBuf;
use std::sync::Mutex;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: pc_import_proof <import|scan|counts> <catalog.db> [media_root] [root]");
        std::process::exit(2);
    }
    let mode = args[1].as_str();
    let catalog_path = PathBuf::from(&args[2]);
    let catalog = Mutex::new(
        ufm_catalog::Catalog::open(&catalog_path).expect("open catalog"),
    );
    let emit = |p: fable_files_lib::pcgames::PcProgress| {
        eprintln!("[{} {}/{}] {}", p.phase, p.done, p.total, p.title);
    };
    match mode {
        "import" => {
            let media_root = PathBuf::from(&args[3]);
            let root = PathBuf::from(&args[4]);
            let report = fable_files_lib::pcgames::execute_pc_import(
                &catalog,
                vec![root],
                &media_root,
                &emit,
            )
            .expect("import");
            println!(
                "{}",
                serde_json::to_string_pretty(&report).expect("json")
            );
        }
        "scan" => {
            let media_root = PathBuf::from(&args[3]);
            let root = args[4].clone();
            let report = fable_files_lib::pcgames::execute_pc_scan(
                &catalog,
                root,
                &media_root,
                &emit,
            );
            println!(
                "{}",
                serde_json::to_string_pretty(&report).expect("json")
            );
        }
        "counts" => {
            let cat = catalog.lock().unwrap();
            let counts = cat.rom_home_counts(&[]).expect("counts");
            println!(
                "{}",
                serde_json::to_string_pretty(&counts).expect("json")
            );
            let facets = cat.rom_home_genre_facets().expect("facets");
            eprintln!("genre facets: {} (top: {:?})", facets.len(), facets.first());
            let mut spec = ufm_catalog::RomHomeShelfSpec {
                kind: "system".to_string(),
                platform: Some("pc".to_string()),
                query: None,
                search_platforms: vec![],
                exclude_platforms: vec![],
                member_keys: None,
                pc_source: None,
                filters: ufm_catalog::RomHomeShelfFilters::default(),
                sort: "title".to_string(),
                cursor: None,
                limit: 500,
            };
            let page = cat.rom_home_shelf_window(&spec).expect("window");
            eprintln!("pc window: total={} games={}", page.total, page.games.len());
            for g in &page.games {
                if g.title.contains("Wolcen") || g.group_key.contains("wolcen") {
                    eprintln!(
                        "WOLCEN row: title={:?} group_key={:?} platform={:?} pc={:?}",
                        g.title,
                        g.group_key,
                        g.platform,
                        g.pc.as_ref().map(|p| (p.library_name.clone(), p.install_state.clone()))
                    );
                }
            }
            spec.pc_source = Some("Playnite".to_string());
            let page = cat.rom_home_shelf_window(&spec).expect("window2");
            eprintln!("pc window (Playnite): total={}", page.total);
        }
        other => {
            eprintln!("unknown mode {other}");
            std::process::exit(2);
        }
    }
}
