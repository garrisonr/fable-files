//! Binary entry point — delegates to the library so mobile targets work.

#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

fn main() {
    // Headless scheduled sync: `fable-files --run-sync <id>` runs one
    // saved sync profile and exits — no window, no Tauri runtime.
    let args: Vec<String> = std::env::args().collect();
    if let Some(pos) = args.iter().position(|a| a == "--run-sync") {
        if let Some(id) = args.get(pos + 1) {
            std::process::exit(fable_files_lib::commands::run_sync_headless(id));
        }
    }
    fable_files_lib::run()
}
