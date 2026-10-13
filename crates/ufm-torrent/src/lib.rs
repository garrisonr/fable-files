//! ufm-torrent — .torrent creation with the Fable Files Washing
//! Machine built in.
//!
//! Design rules (Dickles' spec, 2026-10-13):
//! - Works for any single file or a whole folder tree.
//! - When washing is requested the torrent is built from a STAGED
//!   copy — originals are never mutated by torrent creation.
//! - Washing mirrors the app's wash_metadata recipe exactly:
//!   exiftool `-all=` then Orientation + ICC_Profile restored from
//!   the original file.
//! - "Clean images" goes further: staged images are decoded and
//!   re-encoded from pixels, so no metadata can survive at all.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct TorrentOptions {
    pub wash: bool,
    pub clean_images: bool,
    pub trackers: Vec<String>,
    pub private_flag: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TorrentOutcome {
    pub torrent_path: String,
    pub payload_path: String,
    pub washed_files: u32,
    pub cleaned_images: u32,
}

/// Is mktorrent on this machine?
pub fn mktorrent_available() -> bool {
    Path::new("/usr/bin/mktorrent").exists()
}

fn exiftool_bin() -> String {
    if Path::new("/usr/bin/vendor_perl/exiftool").exists() {
        "/usr/bin/vendor_perl/exiftool".to_string()
    } else {
        "exiftool".to_string()
    }
}

/// The Washing Machine recipe on one staged file: strip all
/// metadata, restore Orientation + ICC_Profile from the ORIGINAL.
fn wash_one_file(original: &Path, target: &Path) -> Result<(), String> {
    let out = Command::new(exiftool_bin())
        .arg("-all=")
        .arg("-tagsFromFile")
        .arg(original)
        .arg("-Orientation")
        .arg("-ICC_Profile")
        .arg("-overwrite_original")
        .arg(target)
        .output()
        .map_err(|e| format!("exiftool: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr)
            .lines()
            .last()
            .unwrap_or("exiftool failed")
            .trim()
            .to_string())
    }
}

/// Pixel re-encode (mirrors the app's clean_photo_copy): decode,
/// save from scratch — nothing attached survives.
fn clean_reencode(src: &Path, dest: &Path) -> Result<(), String> {
    let img = image::open(src).map_err(|e| format!("image open: {e}"))?;
    let ext = dest
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png")
        .to_lowercase();
    let fmt = image::ImageFormat::from_extension(&ext).unwrap_or(image::ImageFormat::Png);
    if fmt == image::ImageFormat::Jpeg {
        img.to_rgb8()
            .save_with_format(dest, fmt)
            .map_err(|e| format!("save: {e}"))?;
    } else {
        img.save_with_format(dest, fmt)
            .map_err(|e| format!("save: {e}"))?;
    }
    Ok(())
}

/// Formats the bundled image build can actually re-encode.
/// (Washing works on more formats than re-encoding does.)
pub fn is_cleanable_image(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase()
            .as_str(),
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "ico"
    )
}

pub fn is_image_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase()
            .as_str(),
        "jpg" | "jpeg" | "png" | "webp" | "tif" | "tiff" | "gif" | "bmp" | "avif" | "heic"
    )
}

/// Trim, drop empties, dedupe (order-preserving).
pub fn clean_trackers(trackers: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in trackers {
        let t = t.trim().to_string();
        if !t.is_empty() && !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// First free "<stem> (<tag>)<ext>" / "<stem> (<tag>) <n><ext>"
/// sibling path — the app's washed/clean naming convention.
fn staged_sibling(src: &Path, tag: &str) -> PathBuf {
    let parent = src.parent().unwrap_or(Path::new("."));
    let stem = src
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    let mut dest = parent.join(format!("{stem} ({tag}){ext}"));
    let mut n = 2;
    while dest.exists() {
        dest = parent.join(format!("{stem} ({tag}) {n}{ext}"));
        n += 1;
    }
    dest
}

/// Create a .torrent for `src` (file or folder).
///
/// No wash + no clean → the torrent points at the original.
/// Otherwise a staged copy is made first (originals untouched)
/// and the torrent points at it. The .torrent lands beside the
/// payload it describes.
pub fn create_torrent(src: &Path, opts: &TorrentOptions) -> Result<TorrentOutcome, String> {
    if !mktorrent_available() {
        return Err("mktorrent is not installed".to_string());
    }
    let meta = std::fs::metadata(src).map_err(|e| e.to_string())?;
    let parent = src.parent().ok_or("no parent dir")?.to_path_buf();
    let mut washed_files: u32 = 0;
    let mut cleaned_images: u32 = 0;

    let payload: PathBuf = if !opts.wash && !opts.clean_images {
        src.to_path_buf()
    } else if !meta.is_dir() {
        // Single file → staged sibling copy.
        let want_clean = opts.clean_images && is_cleanable_image(src);
        if !want_clean && !opts.wash {
            // Clean was requested but this isn't an image and no
            // wash was asked for: nothing to stage.
            return create_torrent(
                src,
                &TorrentOptions {
                    wash: false,
                    clean_images: false,
                    trackers: opts.trackers.clone(),
                    private_flag: opts.private_flag,
                },
            );
        }
        let dest = staged_sibling(src, if want_clean { "clean" } else { "washed" });
        if want_clean {
            match clean_reencode(src, &dest) {
                Ok(()) => {
                    cleaned_images += 1;
                    return finish_torrent(&dest, &parent, opts, washed_files, cleaned_images);
                }
                Err(_) => {
                    // Re-encode failed: fall back to copy + wash.
                    std::fs::copy(src, &dest).map_err(|e| format!("copy: {e}"))?;
                    if opts.wash && wash_one_file(src, &dest).is_ok() {
                        washed_files += 1;
                    }
                }
            }
        } else {
            std::fs::copy(src, &dest).map_err(|e| format!("copy: {e}"))?;
            if opts.wash && wash_one_file(src, &dest).is_ok() {
                washed_files += 1;
            }
        }
        dest
    } else {
        // Folder → staged tree copy "<name> (shared)".
        let folder_name = src
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("payload")
            .to_string();
        let mut stage = parent.join(format!("{folder_name} (shared)"));
        let mut n = 2;
        while stage.exists() {
            stage = parent.join(format!("{folder_name} (shared {n})"));
            n += 1;
        }
        std::fs::create_dir_all(&stage).map_err(|e| e.to_string())?;
        let mut stack = vec![(src.to_path_buf(), stage.clone())];
        while let Some((from, to)) = stack.pop() {
            for entry in std::fs::read_dir(&from).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                let ep = entry.path();
                let dest = to.join(entry.file_name());
                let ft = entry.file_type().map_err(|e| e.to_string())?;
                if ft.is_dir() {
                    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
                    stack.push((ep, dest));
                } else if ft.is_file() {
                    if opts.clean_images && is_cleanable_image(&ep) {
                        match clean_reencode(&ep, &dest) {
                            Ok(()) => {
                                cleaned_images += 1;
                                continue;
                            }
                            Err(_) => {
                                // fall through to copy (+wash) below
                                std::fs::copy(&ep, &dest).map_err(|e| e.to_string())?;
                            }
                        }
                    } else {
                        std::fs::copy(&ep, &dest).map_err(|e| e.to_string())?;
                    }
                    if opts.wash && wash_one_file(&ep, &dest).is_ok() {
                        washed_files += 1;
                    }
                }
            }
        }
        stage
    };

    finish_torrent(&payload, &parent, opts, washed_files, cleaned_images)
}

/// Run mktorrent for an already-chosen payload and build the
/// outcome. The .torrent lands beside the payload.
fn finish_torrent(
    payload: &Path,
    parent: &Path,
    opts: &TorrentOptions,
    washed_files: u32,
    cleaned_images: u32,
) -> Result<TorrentOutcome, String> {
    let payload_name = payload
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("payload")
        .to_string();
    let torrent_path = parent.join(format!("{payload_name}.torrent"));
    let trackers = clean_trackers(&opts.trackers);
    let mut cmd = Command::new("mktorrent");
    cmd.arg("-o").arg(&torrent_path);
    cmd.arg("-c").arg("Created with Fable Files");
    if opts.private_flag {
        cmd.arg("-p");
    }
    if !trackers.is_empty() {
        cmd.arg("-a").arg(trackers.join(","));
    }
    cmd.arg(&payload);
    let out = cmd.output().map_err(|e| format!("mktorrent: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "mktorrent failed: {}",
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .last()
                .unwrap_or("")
                .trim()
        ));
    }
    Ok(TorrentOutcome {
        torrent_path: torrent_path.to_string_lossy().to_string(),
        payload_path: payload.to_string_lossy().to_string(),
        washed_files,
        cleaned_images,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trackers_are_trimmed_and_deduped() {
        let input = vec![
            "  udp://a.example:6969/announce ".to_string(),
            "".to_string(),
            "udp://a.example:6969/announce".to_string(),
            "udp://b.example:1337/announce".to_string(),
        ];
        assert_eq!(
            clean_trackers(&input),
            vec![
                "udp://a.example:6969/announce".to_string(),
                "udp://b.example:1337/announce".to_string()
            ]
        );
    }

    #[test]
    fn image_detection_is_by_extension() {
        assert!(is_image_file(Path::new("/x/photo.JPG")));
        assert!(is_image_file(Path::new("/x/scan.tiff")));
        assert!(!is_image_file(Path::new("/x/movie.mkv")));
        assert!(!is_image_file(Path::new("/x/noext")));
    }
}
