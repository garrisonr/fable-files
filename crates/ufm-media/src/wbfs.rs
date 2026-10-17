//! WBFS container header + embedded disc-header identity.
//!
//! Original implementation of the WBFS file container's header
//! layout, written from the format's structural documentation and
//! verified byte-for-byte against real files. A .wbfs file opens
//! with a one-sector header:
//!
//!   "WBFS" magic, u32 BE count of hd sectors, hd_sec_sz_s (log2 of
//!   the hd sector size) at 8, wbfs_sec_sz_s at 9, version at 10,
//!   then the disc table (one byte per slot)
//!
//! The disc-info area begins at offset 1 << hd_sec_sz_s and opens
//! with a verbatim copy of the disc's own header: game ID at 0,
//! the Wii magic 0x5D1C9EA3 at 0x18 (big-endian) or the GameCube
//! magic 0xC2339F3D at 0x1C, and the title at 0x20.
//!
//! HOW FAR THIS IDENTITY GOES — stated plainly: the disc ID and
//! title above are real (they are the disc's own header bytes), but
//! a WBFS payload is a scrubbed, still-encrypted partition image;
//! it cannot be expanded into the full decrypted ISO the DAT rows
//! hash. This module therefore never feeds romdat's DAT lookup —
//! it exists so the container is recognized, validated, and its
//! header identity is available to surfaces that label it as
//! header-derived. A DAT identification from WBFS bytes alone
//! would be a fabrication, and this code will not produce one.

use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::Path;

pub const WBFS_MAGIC: &[u8; 4] = b"WBFS";
pub const WII_MAGIC: u32 = 0x5D1C_9EA3; // at disc offset 0x18, big-endian
pub const GC_MAGIC: u32 = 0xC233_9F3D; // at disc offset 0x1C, big-endian

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum WbfsError {
    Io(std::io::Error),
    BadMagic,
    Truncated(&'static str),
    Corrupt(String),
}

impl fmt::Display for WbfsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WbfsError::Io(e) => write!(f, "wbfs: io error: {e}"),
            WbfsError::BadMagic => write!(f, "wbfs: bad magic (not a WBFS file)"),
            WbfsError::Truncated(what) => write!(f, "wbfs: truncated {what}"),
            WbfsError::Corrupt(msg) => write!(f, "wbfs: corrupt: {msg}"),
        }
    }
}

impl std::error::Error for WbfsError {}

impl From<std::io::Error> for WbfsError {
    fn from(e: std::io::Error) -> Self {
        WbfsError::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, WbfsError>;

// ---------------------------------------------------------------------------
// Disc header identity (from the container's embedded copy)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WbfsDiscHeader {
    /// Six-byte disc ID, e.g. "SF8E01".
    pub game_id: String,
    /// Title at disc offset 0x20, up to the first NUL.
    pub title: String,
    /// "wii" or "gamecube", from the copy's magic word.
    pub platform: &'static str,
    /// log2 of the WBFS sector size (container geometry).
    pub wbfs_sec_sz_s: u8,
}

/// Parse the WBFS header and the embedded disc-header copy.
/// Ok(None) = a structurally valid WBFS whose disc copy carries
/// neither platform magic (not a Wii/GC disc image we recognize).
pub fn disc_header(path: &Path) -> Result<Option<WbfsDiscHeader>> {
    let mut file = File::open(path)?;
    let mut head = [0u8; 12];
    let n = read_up_to(&mut file, &mut head)?;
    if n < 12 {
        return Err(WbfsError::Truncated("header"));
    }
    if &head[0..4] != WBFS_MAGIC {
        return Err(WbfsError::BadMagic);
    }
    let hd_sec_sz_s = head[8];
    let wbfs_sec_sz_s = head[9];
    if !(9..=15).contains(&hd_sec_sz_s) {
        return Err(WbfsError::Corrupt(format!(
            "implausible hd sector shift {hd_sec_sz_s}"
        )));
    }
    let copy_off = 1usize << hd_sec_sz_s;
    let want = copy_off + 0x80;
    let mut buf = vec![0u8; want];
    buf[..12].copy_from_slice(&head);
    let got = 12 + read_up_to(&mut file, &mut buf[12..])?;
    if got < want {
        return Err(WbfsError::Truncated("disc-info area"));
    }
    let copy = &buf[copy_off..copy_off + 0x80];
    let wii = u32::from_be_bytes(copy[0x18..0x1C].try_into().unwrap());
    let gc = u32::from_be_bytes(copy[0x1C..0x20].try_into().unwrap());
    let platform = if wii == WII_MAGIC {
        "wii"
    } else if gc == GC_MAGIC {
        "gamecube"
    } else {
        return Ok(None);
    };
    let game_id = String::from_utf8_lossy(&copy[0..6]).into_owned();
    let raw_title = &copy[0x20..0x80];
    let end = raw_title.iter().position(|&b| b == 0).unwrap_or(raw_title.len());
    let title = String::from_utf8_lossy(&raw_title[..end])
        .trim()
        .to_string();
    Ok(Some(WbfsDiscHeader {
        game_id,
        title,
        platform,
        wbfs_sec_sz_s,
    }))
}

fn read_up_to(r: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..])? {
            0 => break,
            m => n += m,
        }
    }
    Ok(n)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal WBFS: 512-byte header sector (slot 0 occupied), the
    /// disc-header copy at 512, then filler.
    fn build_wbfs(game_id: &[u8; 6], title: &str, wii: bool) -> Vec<u8> {
        let mut out = vec![0u8; 4096];
        out[0..4].copy_from_slice(WBFS_MAGIC);
        out[4..8].copy_from_slice(&8000u32.to_be_bytes());
        out[8] = 9; // hd sector = 512
        out[9] = 21; // wbfs sector = 2 MiB
        out[10] = 0; // version
        out[12] = 1; // disc table: slot 0 present
        let copy = 512;
        out[copy..copy + 6].copy_from_slice(game_id);
        if wii {
            out[copy + 0x18..copy + 0x1C].copy_from_slice(&WII_MAGIC.to_be_bytes());
        } else {
            out[copy + 0x1C..copy + 0x20].copy_from_slice(&GC_MAGIC.to_be_bytes());
        }
        out[copy + 0x20..copy + 0x20 + title.len()].copy_from_slice(title.as_bytes());
        out
    }

    fn write_tmp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn wii_disc_header_parses() {
        let bytes = build_wbfs(b"SF8E01", "Donkey Kong Country Returns", true);
        let p = write_tmp("fable-wbfs-wii.wbfs", &bytes);
        let h = disc_header(&p).expect("parses").expect("recognized");
        assert_eq!(h.game_id, "SF8E01");
        assert_eq!(h.title, "Donkey Kong Country Returns");
        assert_eq!(h.platform, "wii");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn gc_disc_header_parses() {
        let bytes = build_wbfs(b"GS8P01", "Some Cube Game", false);
        let p = write_tmp("fable-wbfs-gc.wbfs", &bytes);
        let h = disc_header(&p).expect("parses").expect("recognized");
        assert_eq!(h.platform, "gamecube");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn bad_magic_and_truncation_are_errors() {
        let p = write_tmp("fable-wbfs-bad.wbfs", b"NOPE-not-wbfs");
        assert!(matches!(disc_header(&p), Err(WbfsError::BadMagic)));
        std::fs::remove_file(&p).ok();
        let mut bytes = build_wbfs(b"SF8E01", "X", true);
        bytes.truncate(300); // header sector + partial copy
        let p = write_tmp("fable-wbfs-trunc.wbfs", &bytes);
        assert!(matches!(disc_header(&p), Err(WbfsError::Truncated(_))));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn valid_container_unknown_disc_is_none() {
        let mut bytes = build_wbfs(b"ZZZZ99", "Mystery", true);
        // Erase both magics: structurally valid, unrecognized disc.
        bytes[512 + 0x18..512 + 0x1C].copy_from_slice(&[0, 0, 0, 0]);
        let p = write_tmp("fable-wbfs-none.wbfs", &bytes);
        assert!(disc_header(&p).expect("parses").is_none());
        std::fs::remove_file(&p).ok();
    }
}
