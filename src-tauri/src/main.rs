//! Binary entry point — delegates to the library so mobile targets work.

#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

/// The AppImage launcher REPLACES GST_PLUGIN_SYSTEM_PATH_1_0 with the
/// bundle's own minimal gstreamer directory — no appsink, no demuxers,
/// no decoders — which makes WebKit's media pipeline fail hard enough
/// to blank the whole window on video previews. Heal the path before
/// anything starts: the host's plugin directories are ADDED back
/// (never removed), so the full system set is searchable again.
fn heal_gst_plugin_path() {
    const SYS_DIRS: [&str; 3] = [
        "/usr/lib/gstreamer-1.0",
        "/usr/lib/x86_64-linux-gnu/gstreamer-1.0",
        "/usr/lib64/gstreamer-1.0",
    ];
    for var in ["GST_PLUGIN_SYSTEM_PATH_1_0", "GST_PLUGIN_SYSTEM_PATH"] {
        let cur = std::env::var(var).unwrap_or_default();
        let mut parts: Vec<String> = cur
            .split(':')
            .filter(|x| !x.is_empty())
            .map(|x| x.to_string())
            .collect();
        for dir in SYS_DIRS {
            if std::path::Path::new(dir).is_dir() && !parts.iter().any(|x| x == dir) {
                parts.push(dir.to_string());
            }
        }
        if !parts.is_empty() {
            std::env::set_var(var, parts.join(":"));
        }
    }
    // Same story for the plugin scanner helper: libgstreamer derives
    // its location from the loaded library's prefix (the AppImage
    // mount), which doesn't ship the helper — point it at the host's.
    if std::env::var_os("GST_PLUGIN_SCANNER").is_none() {
        for cand in [
            "/usr/lib/gstreamer-1.0/gst-plugin-scanner",
            "/usr/lib/x86_64-linux-gnu/gstreamer-1.0/gst-plugin-scanner",
            "/usr/lib64/gstreamer-1.0/gst-plugin-scanner",
        ] {
            if std::path::Path::new(cand).is_file() {
                std::env::set_var("GST_PLUGIN_SCANNER", cand);
                break;
            }
        }
    }
}

fn main() {
    heal_gst_plugin_path();
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
