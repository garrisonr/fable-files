//! ROM identification against the bundled DAT dataset (rom-dats.bin,
//! format FABLEDAT1, compiled by tools/datc from No-Intro / Redump /
//! MAME DATs as redistributed by libretro-database — see ROM-DATS.md).
//!
//! A file is a member of a built-in ROM collection ONLY when its bytes
//! hash-match a DAT row (or, for MAME-style zip sets, when the zip's
//! filename is a known set whose contained ROM CRCs are all present).
//! Extensions merely nominate candidates; the DAT decides.
//!
//! Containers and disc sets (2026-10-16, ROM Identification Phase 2):
//! 7z/rar archives get the zip treatment — members are enumerated with
//! their stored CRCs (7z's technical listing; bsdtar as fallback) and
//! matched without decompression — plus one layer zip never needed:
//! a cue/ccd set inside the archive (or loose on disk, anchored by its
//! .cue/.ccd/.gdi) identifies by hashing the cue's data-track SPAN of
//! the disc image, because Redump DATs carry per-track rows, so a
//! whole-image hash can never match. One archive can yield several
//! identifications (TruRip packs regional variants side by side);
//! `identify_file_all` returns them all, best variant first.
//!
//! Identification is cheap by construction:
//!   1. extension → plausible systems (table below),
//!   2. magic gates reject wrong formats before any hashing
//!      (a C64 cartridge image must start "C64 CARTRIDGE"; a Genesis
//!      dump must carry "SEGA" at 0x100; an .nes must start "NES\x1A"),
//!   3. size gate: if no DAT row in ANY plausible system (under that
//!      system's header transform) has the file's size, the file is
//!      never opened,
//!   4. one streaming pass computes CRC32 + MD5 + SHA1 together;
//!      header transforms (iNES skip, copier-header skip, SMD
//!      deinterleave, N64 byte-order) are applied in the stream.
//!      MD5/SHA1 only arbitrate the rare (crc,size) collisions —
//!      the dataset keeps side hashes for exactly those rows.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const HEADER_LEN: usize = 44; // magic 8 + ver 2 + sys_count 2 + 8 × u32
const ROW_LEN: usize = 28;
const SET_LEN: usize = 20;
const SETROM_LEN: usize = 12;
const NONE32: u32 = u32::MAX;

// ---------------------------------------------------------------------------
// Dataset
// ---------------------------------------------------------------------------

pub struct RomDataset {
    data: Vec<u8>,
    systems: Vec<(String, String)>, // (key, display) by index
    rows_off: usize,
    row_count: usize,
    sets_off: usize,
    set_count: usize,
    setroms_off: usize,
    names_off: usize,
    names_len: usize,
    shorts_off: usize,
    shorts_len: usize,
    md5_off: usize,
    sha1_off: usize,
    /// Per-system set of known content sizes (the cheap size gate).
    sizes: Vec<HashSet<u64>>,
    /// (sys, lowercase shortname) → set index.
    set_index: HashMap<(u8, String), usize>,
}

fn rd_u16(d: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([d[off], d[off + 1]])
}
fn rd_u32(d: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([d[off], d[off + 1], d[off + 2], d[off + 3]])
}
fn rd_u64(d: &[u8], off: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&d[off..off + 8]);
    u64::from_le_bytes(b)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowHit {
    pub sys: u8,
    pub name: String,
    pub crc: u32,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct SetView {
    pub sys: u8,
    pub short: String,
    pub display: String,
    pub parent: Option<String>,
    pub roms: Vec<(u32, u64)>,
}

impl RomDataset {
    pub fn load(data: Vec<u8>) -> Result<RomDataset, String> {
        if data.len() < HEADER_LEN || &data[0..8] != b"FABLEDAT" {
            return Err("rom dataset: bad magic".into());
        }
        if rd_u16(&data, 8) != 1 {
            return Err("rom dataset: unsupported version".into());
        }
        let sys_count = rd_u16(&data, 10) as usize;
        let row_count = rd_u32(&data, 12) as usize;
        let md5_count = rd_u32(&data, 16) as usize;
        let sha1_count = rd_u32(&data, 20) as usize;
        let names_len = rd_u32(&data, 24) as usize;
        let sys_len = rd_u32(&data, 28) as usize;
        let set_count = rd_u32(&data, 32) as usize;
        let setrom_count = rd_u32(&data, 36) as usize;
        let shorts_len = rd_u32(&data, 40) as usize;

        let sys_off = HEADER_LEN;
        let rows_off = sys_off + sys_len;
        let sets_off = rows_off + row_count * ROW_LEN;
        let setroms_off = sets_off + set_count * SET_LEN;
        let names_off = setroms_off + setrom_count * SETROM_LEN;
        let shorts_off = names_off + names_len;
        let md5_off = shorts_off + shorts_len;
        let sha1_off = md5_off + md5_count * 16;
        let total = sha1_off + sha1_count * 20;
        if total > data.len() {
            return Err("rom dataset: truncated".into());
        }

        // Systems blob: u8 klen, key, u8 dlen, display.
        let mut systems = Vec::with_capacity(sys_count);
        let mut p = sys_off;
        for _ in 0..sys_count {
            let kl = data[p] as usize;
            let key = String::from_utf8_lossy(&data[p + 1..p + 1 + kl]).into_owned();
            let dl = data[p + 1 + kl] as usize;
            let disp =
                String::from_utf8_lossy(&data[p + 2 + kl..p + 2 + kl + dl]).into_owned();
            p += 2 + kl + dl;
            systems.push((key, disp));
        }

        let mut ds = RomDataset {
            data,
            systems,
            rows_off,
            row_count,
            sets_off,
            set_count,
            setroms_off,
            names_off,
            names_len,
            shorts_off,
            shorts_len,
            md5_off,
            sha1_off,
            sizes: Vec::new(),
            set_index: HashMap::new(),
        };
        // Size gate index + set index.
        let mut sizes: Vec<HashSet<u64>> = (0..sys_count).map(|_| HashSet::new()).collect();
        for i in 0..row_count {
            let off = rows_off + i * ROW_LEN;
            let size = rd_u64(&ds.data, off + 4);
            let sys = ds.data[off + 24] as usize;
            if sys < sizes.len() {
                sizes[sys].insert(size);
            }
        }
        ds.sizes = sizes;
        for i in 0..set_count {
            let (sys, short) = {
                let v = ds.set_at(i);
                (v.sys, v.short.to_lowercase())
            };
            ds.set_index.insert((sys, short), i);
        }
        Ok(ds)
    }

    pub fn systems(&self) -> &[(String, String)] {
        &self.systems
    }

    pub fn sys_index(&self, key: &str) -> Option<u8> {
        self.systems.iter().position(|(k, _)| k == key).map(|i| i as u8)
    }

    pub fn sys_key(&self, idx: u8) -> &str {
        self.systems
            .get(idx as usize)
            .map(|(k, _)| k.as_str())
            .unwrap_or("")
    }

    pub fn row_count(&self) -> usize {
        self.row_count
    }

    pub fn set_count(&self) -> usize {
        self.set_count
    }

    fn blob_str(&self, base: usize, len: usize, off: u32) -> String {
        let start = base + off as usize;
        if start >= base + len {
            return String::new();
        }
        let slice = &self.data[start..base + len];
        let end = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());
        String::from_utf8_lossy(&slice[..end]).into_owned()
    }

    fn name_at(&self, off: u32) -> String {
        self.blob_str(self.names_off, self.names_len, off)
    }

    fn short_at(&self, off: u32) -> String {
        self.blob_str(self.shorts_off, self.shorts_len, off)
    }

    fn row_at(&self, i: usize) -> (u32, u64, u32, u32, u32, u8) {
        let off = self.rows_off + i * ROW_LEN;
        (
            rd_u32(&self.data, off),
            rd_u64(&self.data, off + 4),
            rd_u32(&self.data, off + 12), // name_off
            rd_u32(&self.data, off + 16), // md5_idx
            rd_u32(&self.data, off + 20), // sha1_idx
            self.data[off + 24],          // sys
        )
    }

    fn md5_at(&self, idx: u32) -> Option<[u8; 16]> {
        if idx == NONE32 {
            return None;
        }
        let off = self.md5_off + idx as usize * 16;
        let mut h = [0u8; 16];
        h.copy_from_slice(&self.data[off..off + 16]);
        Some(h)
    }

    fn sha1_at(&self, idx: u32) -> Option<[u8; 20]> {
        if idx == NONE32 {
            return None;
        }
        let off = self.sha1_off + idx as usize * 20;
        let mut h = [0u8; 20];
        h.copy_from_slice(&self.data[off..off + 20]);
        Some(h)
    }

    /// Does any row in `sys` have exactly this (content) size?
    pub fn size_known(&self, sys: u8, size: u64) -> bool {
        self.sizes
            .get(sys as usize)
            .map(|s| s.contains(&size))
            .unwrap_or(false)
    }

    /// All rows matching (crc, size) among the given systems. When more
    /// than one distinct row matches, rows carrying side hashes are
    /// confirmed/denied against the file's md5 then sha1; rows without
    /// side hashes survive as unconfirmed.
    pub fn lookup(
        &self,
        crc: u32,
        size: u64,
        md5: Option<&[u8; 16]>,
        sha1: Option<&[u8; 20]>,
        sys_filter: &[u8],
    ) -> Vec<RowHit> {
        // Rows are sorted by (crc, size, sys) — binary-search the crc run.
        let lo = self.partition(|i| self.row_at(i).0 < crc);
        let hi = self.partition(|i| self.row_at(i).0 <= crc);
        let mut hits: Vec<(RowHit, Option<[u8; 16]>, Option<[u8; 20]>)> = Vec::new();
        for i in lo..hi {
            let (rcrc, rsize, name_off, md5_idx, sha1_idx, sys) = self.row_at(i);
            if rsize != size || !sys_filter.contains(&sys) {
                continue;
            }
            hits.push((
                RowHit {
                    sys,
                    name: self.name_at(name_off),
                    crc: rcrc,
                    size: rsize,
                },
                self.md5_at(md5_idx),
                self.sha1_at(sha1_idx),
            ));
        }
        if hits.len() > 1 {
            // Arbitrate: a row whose stored md5/sha1 DISAGREES with the
            // file's is a different ROM that happens to share (crc,size)
            // — drop it. Rows that agree, or carry no side hash to check,
            // survive (resolve_hits applies system preference among them).
            hits.retain(|(_, side_md5, side_sha1)| {
                let md5_ok = match (md5, side_md5) {
                    (Some(f), Some(s)) => f == s,
                    _ => true,
                };
                let sha1_ok = match (sha1, side_sha1) {
                    (Some(f), Some(s)) => f == s,
                    _ => true,
                };
                md5_ok && sha1_ok
            });
        }
        hits.into_iter().map(|(h, _, _)| h).collect()
    }

    fn partition(&self, pred: impl Fn(usize) -> bool) -> usize {
        let mut lo = 0usize;
        let mut hi = self.row_count;
        while lo < hi {
            let mid = (lo + hi) / 2;
            if pred(mid) {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    fn set_at(&self, i: usize) -> SetView {
        let off = self.sets_off + i * SET_LEN;
        let sys = self.data[off];
        let count = rd_u16(&self.data, off + 2) as usize;
        let short_off = rd_u32(&self.data, off + 4);
        let disp_off = rd_u32(&self.data, off + 8);
        let parent_off = rd_u32(&self.data, off + 12);
        let start = rd_u32(&self.data, off + 16) as usize;
        let mut roms = Vec::with_capacity(count);
        for j in 0..count {
            let ro = self.setroms_off + (start + j) * SETROM_LEN;
            roms.push((rd_u32(&self.data, ro), rd_u64(&self.data, ro + 4)));
        }
        SetView {
            sys,
            short: self.short_at(short_off),
            display: self.name_at(disp_off),
            parent: if parent_off == NONE32 {
                None
            } else {
                Some(self.short_at(parent_off))
            },
            roms,
        }
    }

    pub fn find_set(&self, sys: u8, short: &str) -> Option<SetView> {
        self.set_index
            .get(&(sys, short.to_lowercase()))
            .map(|&i| self.set_at(i))
    }
}

// ---------------------------------------------------------------------------
// Candidate systems per extension (the Fable ROM system seeds, keyed to
// dataset system keys). Order = preference when bytes match several DATs.
// ---------------------------------------------------------------------------

pub fn systems_for_extension(ext: &str) -> &'static [&'static str] {
    match ext {
        "32x" => &["sega32x"],
        "a26" => &["atari2600"],
        "a52" => &["atari5200"],
        "a78" => &["atari7800"],
        "adf" | "dms" | "lha" => &["amiga"],
        "atr" | "atx" | "xex" | "xfd" => &["atari8bit"],
        "bin" => &[
            "genesis", "segacd", "saturn", "psx", "tg16", "dreamcast", "odyssey2", "channelf",
            "studio2", "arcadia2001", "scv", "pv1000", "superacan", "gamecom", "cdi", "pcfx",
            "neogeocd", "amigacd32",
        ],
        "ccd" => &[],
        "chd" => &[],
        "cia" | "3ds" => &["n3ds"],
        "ciso" => &["gamecube"],
        "col" | "cv" => &["colecovision"],
        "chf" => &["channelf"],
        "cpr" => &["gx4000"],
        "crt" => &["c64", "vic20"],
        "cso" => &["psp"],
        "cue" => &[],
        "d64" | "t64" => &["c64"],
        "prg" => &["c64", "vic20"],
        "dsk" => &["msx"],
        "fds" => &[],
        "fig" | "sfc" | "smc" | "swc" => &["snes"],
        "gb" | "sgb" => &["gb", "gbc"],
        "gba" => &["gba"],
        "gbc" => &["gbc"],
        "gc" | "gcm" => &["gamecube"],
        "gdi" | "cdi" => &["dreamcast"],
        "gen" | "md" => &["genesis"],
        "gg" => &["gamegear"],
        "img" => &["psx", "saturn", "segacd", "neogeocd"],
        "int" => &["intellivision"],
        "iso" => &[
            "psx", "ps2", "saturn", "segacd", "dreamcast", "psp", "gamecube", "wii", "xbox",
            "xbox360", "ps3", "cdi", "pcfx", "neogeocd", "amigacd32", "jaguar",
        ],
        "j64" | "jag" => &["jaguar"],
        "lnx" => &["lynx"],
        "mdf" => &["psx", "ps2", "saturn", "segacd"],
        "mx1" | "mx2" => &["msx"],
        "n64" | "v64" | "z64" => &["n64"],
        "nds" => &["nds"],
        "neo" => &["neogeo"],
        "nes" | "unh" => &["nes"],
        "ngc" | "ngp" => &["neogeopocket"],
        "pbp" => &["psp", "psx"],
        "pce" => &["tg16"],
        "pkg" => &["ps3", "psp"],
        "rom" => &["atari8bit", "msx", "intellivision"],
        "rpx" | "wua" | "wux" => &["wiiu"],
        "rvz" | "wbfs" | "wad" => &["wii"],
        "smd" => &["genesis"],
        "sg" => &["sg1000"],
        "sms" => &["mastersystem"],
        "sna" | "szx" | "tzx" | "z80" => &["zxspectrum"],
        "tap" => &["c64", "zxspectrum", "vic20"],
        "tgc" => &["gamecom"],
        "vb" | "vboy" => &["virtualboy"],
        "vpk" => &["psvita"],
        "vec" => &["vectrex"],
        "ws" | "wsc" => &["wonderswan"],
        "xbe" | "xiso" => &["xbox"],
        _ => &[],
    }
}

/// Every extension that can nominate a file for identification
/// (the union of `systems_for_extension` keys, plus the container and
/// disc-set anchors: "zip", "7z", "rar", "cue", "ccd", "chd", "gdi").
/// "md" is deliberately NOT here: it nominates Genesis in
/// `systems_for_extension`, but on a real disk nearly every .md is a
/// Markdown document — nomination wrote thousands of junk identities
/// (audit §5). A loose .md can still be identified when a caller asks
/// about that file directly; it is never scanned for.
pub fn candidate_extensions() -> Vec<&'static str> {
    vec![
        "32x", "3ds", "7z", "a26", "a52", "a78", "adf", "atr", "atx", "bin", "ccd", "cdi", "chd",
        "chf", "cia", "ciso", "col", "cpr", "crt", "cso", "cue", "cv", "d64", "dms", "dsk", "fig",
        "gb", "gba", "gbc", "gc", "gcm", "gdi", "gen", "gg", "img", "int", "iso", "j64", "jag",
        "lha", "lnx", "mdf", "mx1", "mx2", "n64", "nds", "neo", "nes", "ngc", "ngp", "pbp", "pce",
        "pkg", "prg", "rar", "rom", "rpx", "sfc", "sg", "sgb", "smc", "smd", "sms", "sna", "swc",
        "szx", "t64", "tap", "tgc", "tzx", "unh", "v64", "vb", "vboy", "vec", "vpk", "wad", "wbfs",
        "ws", "wsc", "wua", "wux", "xbe", "xex", "xfd", "xiso", "z64", "z80", "zip",
    ]
}

/// All dataset system keys that accept loose files (for zip containers:
/// a zip can hold any cartridge system's ROM).
fn cartridge_systems() -> &'static [&'static str] {
    &[
        "atari2600", "atari5200", "atari7800", "atari8bit", "lynx", "colecovision", "c64",
        "gb", "gba", "gbc", "gamegear", "genesis", "intellivision", "jaguar", "mastersystem",
        "msx", "neogeopocket", "nes", "n64", "nds", "n3ds", "sega32x", "snes", "tg16",
        "vectrex", "virtualboy", "wonderswan", "zxspectrum", "amiga", "odyssey2", "channelf",
        "studio2", "arcadia2001", "scv", "pv1000", "superacan", "sg1000", "vic20", "gamecom",
        "gx4000",
    ]
}

// ---------------------------------------------------------------------------
// Header transforms + gates
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transform {
    Raw,
    Skip(usize),
    /// Genesis .smd: skip the 512B copier header, then deinterleave.
    Smd,
    /// N64: normalize byte order to big-endian (z64 layout).
    N64Endian,
}

/// Header gate: false = this file cannot be this system's ROM, cheaply
/// proven (before any hashing). `probe` is the first 512 bytes (or less).
fn gate(sys: &str, ext: &str, probe: &[u8], size: u64) -> bool {
    match sys {
        "c64" if ext == "crt" => probe.starts_with(b"C64 CARTRIDGE"),
        "nes" if ext == "nes" => probe.starts_with(b"NES\x1A"),
        "genesis" if matches!(ext, "md" | "gen" | "bin") => {
            size >= 0x104 + 4 && probe.len() >= 0x104 + 4 && &probe[0x100..0x104] == b"SEGA"
        }
        _ => true,
    }
}

/// The primary + variant transforms to try for a system, in order.
/// (No-Intro hashes headerless data; dumps in the wild carry headers.)
fn transforms(sys: &str, ext: &str, probe: &[u8], size: u64) -> Vec<Transform> {
    match sys {
        "nes" if ext == "nes" => vec![Transform::Skip(16), Transform::Raw],
        "lynx" => {
            if probe.starts_with(b"LYNX") {
                vec![Transform::Skip(64), Transform::Raw]
            } else {
                vec![Transform::Raw]
            }
        }
        "snes" | "tg16" => {
            if size % 1024 == 512 {
                vec![Transform::Raw, Transform::Skip(512)]
            } else {
                vec![Transform::Raw]
            }
        }
        "atari7800" => {
            if probe.len() >= 109 && &probe[100..109] == b"ATARI7800" {
                vec![Transform::Raw, Transform::Skip(128)]
            } else {
                vec![Transform::Raw]
            }
        }
        "n64" => vec![Transform::N64Endian],
        "genesis" if ext == "smd" => vec![Transform::Smd, Transform::Raw],
        _ => vec![Transform::Raw],
    }
}

fn effective_size(t: Transform, size: u64) -> Option<u64> {
    match t {
        Transform::Skip(n) => size.checked_sub(n as u64),
        Transform::Smd => size.checked_sub(512),
        _ => Some(size),
    }
}

// ---------------------------------------------------------------------------
// Streaming hash with transforms
// ---------------------------------------------------------------------------

struct HashSink {
    crc: crc32fast::Hasher,
    md5: md5::Context,
    sha1: sha1::Sha1,
    len: u64,
}

impl HashSink {
    fn new() -> Self {
        use sha1::Digest as _;
        HashSink {
            crc: crc32fast::Hasher::new(),
            md5: md5::Context::new(),
            sha1: sha1::Sha1::new(),
            len: 0,
        }
    }
    fn push(&mut self, bytes: &[u8]) {
        use sha1::Digest as _;
        self.crc.update(bytes);
        self.md5.consume(bytes);
        self.sha1.update(bytes);
        self.len += bytes.len() as u64;
    }
    fn finish(self) -> (u32, [u8; 16], [u8; 20], u64) {
        use sha1::Digest as _;
        (
            self.crc.finalize(),
            self.md5.compute().0,
            self.sha1.finalize().into(),
            self.len,
        )
    }
}

/// Hash a file under a header transform. Returns (crc, md5, sha1, hashed_len).
fn hash_file(path: &Path, t: Transform) -> std::io::Result<(u32, [u8; 16], [u8; 20], u64)> {
    let mut f = std::fs::File::open(path)?;
    let mut sink = HashSink::new();
    let skip = match t {
        Transform::Skip(n) => n,
        Transform::Smd => 512,
        _ => 0,
    };
    // N64 byte order from the first 4 bytes.
    let n64_mode: u8 = if t == Transform::N64Endian {
        let mut magic = [0u8; 4];
        let n = read_up_to(&mut f, &mut magic)?;
        if n < 4 {
            return Ok((0, [0; 16], [0; 20], 0));
        }
        f.seek(SeekFrom::Start(0))?;
        match magic {
            [0x80, 0x37, 0x12, 0x40] => 0, // z64: already big-endian
            [0x37, 0x80, 0x40, 0x12] => 1, // v64: swap byte pairs
            [0x40, 0x12, 0x37, 0x80] => 2, // n64: reverse each 4
            _ => 0,
        }
    } else {
        0
    };
    let mut buf = vec![0u8; 64 * 1024];
    if t == Transform::Smd {
        // Deinterleave in 16KB blocks: second half = even bytes,
        // first half = odd bytes.
        let mut block = vec![0u8; 16 * 1024];
        let mut out = vec![0u8; 16 * 1024];
        // consume the 512B header first through the generic skip path
        let mut to_skip = skip;
        while to_skip > 0 {
            let want = to_skip.min(buf.len());
            let n = f.read(&mut buf[..want])?;
            if n == 0 {
                break;
            }
            to_skip -= n;
        }
        loop {
            let mut filled = 0usize;
            while filled < block.len() {
                let n = f.read(&mut block[filled..])?;
                if n == 0 {
                    break;
                }
                filled += n;
            }
            if filled == 0 {
                break;
            }
            let half = filled / 2;
            for i in 0..half {
                out[2 * i] = block[half + i];
                out[2 * i + 1] = block[i];
            }
            let produced = half * 2;
            if filled % 2 == 1 {
                // odd tail byte: passes through (shouldn't happen in SMD)
                sink.push(&out[..produced]);
                sink.push(&block[filled - 1..filled]);
            } else {
                sink.push(&out[..produced]);
            }
            if filled < block.len() {
                break;
            }
        }
        return Ok(sink.finish());
    }
    let _ = skip;
    let mut pending_skip = match t {
        Transform::Skip(n) => n,
        _ => 0,
    };
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let mut chunk = &buf[..n];
        if pending_skip > 0 {
            if chunk.len() <= pending_skip {
                pending_skip -= chunk.len();
                continue;
            }
            chunk = &chunk[pending_skip..];
            pending_skip = 0;
        }
        match (t, n64_mode) {
            (Transform::N64Endian, 1) => {
                let mut v = chunk.to_vec();
                for pair in v.chunks_exact_mut(2) {
                    pair.swap(0, 1);
                }
                sink.push(&v);
            }
            (Transform::N64Endian, 2) => {
                let mut v = chunk.to_vec();
                for quad in v.chunks_exact_mut(4) {
                    quad.reverse();
                }
                sink.push(&v);
            }
            _ => sink.push(chunk),
        }
    }
    Ok(sink.finish())
}

fn read_up_to(f: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        let r = f.read(&mut buf[n..])?;
        if r == 0 {
            break;
        }
        n += r;
    }
    Ok(n)
}

// ---------------------------------------------------------------------------
// ZIP central directory
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ZipEntry {
    pub name: String,
    pub crc: u32,
    pub size: u64,
}

/// Read a zip's central directory (names + stored CRC32s) without
/// decompressing anything. Zip64 archives return an error.
pub fn zip_entries(path: &Path) -> Result<Vec<ZipEntry>, String> {
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    let tail_len = len.min(65_557 + 22);
    f.seek(SeekFrom::End(-(tail_len as i64)))
        .map_err(|e| e.to_string())?;
    let mut tail = vec![0u8; tail_len as usize];
    f.read_exact(&mut tail).map_err(|e| e.to_string())?;
    // Find the End Of Central Directory record (scan from the end).
    let mut eocd = None;
    if tail.len() >= 22 {
        for i in (0..=tail.len() - 22).rev() {
            if &tail[i..i + 4] == b"PK\x05\x06" {
                eocd = Some(i);
                break;
            }
        }
    }
    let eocd = eocd.ok_or("not a zip (no end-of-central-directory)")?;
    let count = rd_u16(&tail, eocd + 10) as usize;
    let cd_size = rd_u32(&tail, eocd + 12) as u64;
    let cd_off = rd_u32(&tail, eocd + 16) as u64;
    if count == 0xFFFF || cd_off == u32::MAX as u64 {
        return Err("zip64 archives are not supported".into());
    }
    f.seek(SeekFrom::Start(cd_off)).map_err(|e| e.to_string())?;
    let mut cd = vec![0u8; cd_size as usize];
    f.read_exact(&mut cd).map_err(|e| e.to_string())?;
    let mut entries = Vec::with_capacity(count);
    let mut p = 0usize;
    while p + 46 <= cd.len() && entries.len() < count {
        if &cd[p..p + 4] != b"PK\x01\x02" {
            break;
        }
        let crc = rd_u32(&cd, p + 16);
        let size = rd_u32(&cd, p + 24) as u64;
        let nlen = rd_u16(&cd, p + 28) as usize;
        let xlen = rd_u16(&cd, p + 30) as usize;
        let clen = rd_u16(&cd, p + 32) as usize;
        let name = String::from_utf8_lossy(&cd[p + 46..p + 46 + nlen]).into_owned();
        entries.push(ZipEntry { name, crc, size });
        p += 46 + nlen + xlen + clen;
    }
    Ok(entries)
}

// ---------------------------------------------------------------------------
// Identification
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identification {
    /// Primary dataset system key (e.g. "nes").
    pub sys: String,
    /// Other systems the same bytes/set also belong to (MAME sets live
    /// in both Arcade and their home system, e.g. Neo Geo).
    pub also: Vec<String>,
    /// Display name from the DAT.
    pub dat_name: String,
    pub crc: u32,
    pub md5: [u8; 16],
    pub sha1: [u8; 20],
    /// "file" (the file itself matched), "zip" (a contained ROM
    /// matched), or "set" (a MAME-style set matched by name+contents).
    pub container: &'static str,
    /// The identified file's own size on disk.
    pub file_size: u64,
    /// For container/disc identifications: the member that IS the
    /// game — an archive member path ("Game (World)/Game.img") or,
    /// for a loose cue/ccd set, the disc image's path beside the
    /// anchor. None for plain files and set matches.
    pub member: Option<String>,
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Identification {
    pub fn md5_hex(&self) -> String {
        hex_of(&self.md5)
    }
    pub fn sha1_hex(&self) -> String {
        hex_of(&self.sha1)
    }
}

/// Try to identify a MAME-style zip set: the file's stem is a known
/// set shortname and every ROM the set requires is inside the zip
/// (by stored CRC + size). Parent (romof) ROMs live in the parent's
/// own zip and are NOT required here (split-set friendly).
fn identify_set(
    ds: &RomDataset,
    stem: &str,
    entries: &[ZipEntry],
    file_size: u64,
) -> Option<Identification> {
    let contained: HashSet<(u32, u64)> =
        entries.iter().map(|e| (e.crc, e.size)).collect();
    // Neo Geo first: a Neo Geo set belongs to both collections; the
    // more specific system is primary.
    for sys_key in ["neogeo", "arcade"] {
        let sys = ds.sys_index(sys_key)?;
        let set = match ds.find_set(sys, stem) {
            Some(s) => s,
            None => continue,
        };
        if set.roms.is_empty() {
            continue;
        }
        if set.roms.iter().all(|r| contained.contains(r)) {
            let in_neogeo = ds
                .sys_index("neogeo")
                .map(|ni| ds.find_set(ni, stem).is_some())
                .unwrap_or(false);
            let also = if sys_key == "neogeo" {
                vec!["arcade".to_string()]
            } else if in_neogeo {
                vec!["neogeo".to_string()]
            } else {
                vec![]
            };
            return Some(Identification {
                sys: sys_key.to_string(),
                also,
                dat_name: set.display.clone(),
                crc: 0,
                md5: [0; 16],
                sha1: [0; 20],
                container: "set",
                file_size,
                member: None,
            });
        }
    }
    None
}

/// Identify one file. Err = the file could not be read at all;
/// Ok(None) = read fine, no DAT match (not a known ROM).
pub fn identify_file(ds: &RomDataset, path: &Path) -> Result<Option<Identification>, String> {
    Ok(identify_file_all(ds, path)?.into_iter().next())
}

/// Identify one file, returning EVERY identification it yields.
/// Loose files and zips yield at most one (established behavior);
/// 7z/rar archives can yield several — one per disc-set variant
/// packed inside (TruRip's regional variants), best variant first.
pub fn identify_file_all(
    ds: &RomDataset,
    path: &Path,
) -> Result<Vec<Identification>, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Ok(Vec::new());
    }
    let file_size = meta.len();
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        "zip" => Ok(identify_zip(ds, path, file_size)?.into_iter().collect()),
        "7z" | "rar" => identify_archive(ds, path, file_size, &ext),
        "cue" => Ok(identify_cue_anchor(ds, path, file_size)?.into_iter().collect()),
        "ccd" => Ok(identify_ccd_anchor(ds, path, file_size)?.into_iter().collect()),
        "gdi" => Ok(identify_gdi_anchor(ds, path, file_size)?.into_iter().collect()),
        // CHDs identify natively: the reader in crate::chd slices
        // per-track spans out of the compressed hunks (see
        // identify_chd_anchor); DAT rows arbitrate as for cue sets.
        "chd" => Ok(identify_chd_anchor(ds, path, file_size)?.into_iter().collect()),
        _ => Ok(identify_loose(ds, path, file_size, &ext)?.into_iter().collect()),
    }
}

fn identify_loose(
    ds: &RomDataset,
    path: &Path,
    file_size: u64,
    ext: &str,
) -> Result<Option<Identification>, String> {
    let sys_keys = systems_for_extension(ext);
    if sys_keys.is_empty() {
        return Ok(None);
    }
    // Probe the header once for gates/transforms.
    let probe = {
        let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; 512];
        let n = read_up_to(&mut f, &mut buf).map_err(|e| e.to_string())?;
        buf.truncate(n);
        buf
    };
    // Group candidate systems by transform; primary transforms (the
    // canonical layout per system) are attempted before variants.
    let mut primary: Vec<(Transform, Vec<u8>)> = Vec::new();
    let mut variant: Vec<(Transform, Vec<u8>)> = Vec::new();
    for key in sys_keys {
        let sys = match ds.sys_index(key) {
            Some(s) => s,
            None => continue,
        };
        if !gate(key, &ext, &probe, file_size) {
            continue;
        }
        for (i, t) in transforms(key, &ext, &probe, file_size).into_iter().enumerate() {
            let eff = match effective_size(t, file_size) {
                Some(e) => e,
                None => continue,
            };
            if !ds.size_known(sys, eff) {
                continue;
            }
            let bucket = if i == 0 { &mut primary } else { &mut variant };
            match bucket.iter_mut().find(|(at, _)| *at == t) {
                Some((_, syses)) => {
                    if !syses.contains(&sys) {
                        syses.push(sys);
                    }
                }
                None => bucket.push((t, vec![sys])),
            }
        }
    }
    let attempts: Vec<(Transform, Vec<u8>)> =
        primary.into_iter().chain(variant).collect();
    for (t, syses) in attempts {
        let (crc, md5, sha1, _len) = hash_file(path, t).map_err(|e| e.to_string())?;
        let eff = effective_size(t, file_size).unwrap_or(file_size);
        let hits = ds.lookup(crc, eff, Some(&md5), Some(&sha1), &syses);
        if !hits.is_empty() {
            return Ok(Some(resolve_hits(ds, hits, sys_keys, crc, md5, sha1, "file", file_size)));
        }
    }
    Ok(None)
}

fn resolve_hits(
    ds: &RomDataset,
    hits: Vec<RowHit>,
    pref_keys: &[&str],
    crc: u32,
    md5: [u8; 16],
    sha1: [u8; 20],
    container: &'static str,
    file_size: u64,
) -> Identification {
    // Preference: the hit whose system comes first in the candidate
    // order for this extension; other distinct systems ride along.
    let mut ordered: Vec<&RowHit> = hits.iter().collect();
    ordered.sort_by_key(|h| {
        pref_keys
            .iter()
            .position(|k| ds.sys_index(k) == Some(h.sys))
            .unwrap_or(usize::MAX)
    });
    let first = ordered[0];
    let mut also: Vec<String> = Vec::new();
    for h in &ordered[1..] {
        let k = ds.sys_key(h.sys).to_string();
        if k != ds.sys_key(first.sys) && !also.contains(&k) {
            also.push(k);
        }
    }
    Identification {
        sys: ds.sys_key(first.sys).to_string(),
        also,
        dat_name: first.name.clone(),
        crc,
        md5,
        sha1,
        container,
        file_size,
        member: None,
    }
}

/// Contained-ROM matching over a member list: the first entry whose
/// stored CRC+size hits a DAT row (entry extensions nominate their
/// own systems; an entry without a known extension is tried against
/// all cartridge systems). Shared by zip and 7z/rar identification.
fn contained_hit(
    ds: &RomDataset,
    entries: &[ZipEntry],
    container: &'static str,
    file_size: u64,
) -> Option<Identification> {
    let cart: Vec<u8> = cartridge_systems()
        .iter()
        .filter_map(|k| ds.sys_index(k))
        .collect();
    for entry in entries {
        if entry.name.ends_with('/') || entry.size == 0 {
            continue;
        }
        let eext = entry
            .name
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_lowercase();
        let mut syses: Vec<u8> = systems_for_extension(&eext)
            .iter()
            .filter_map(|k| ds.sys_index(k))
            .collect();
        if syses.is_empty() {
            syses = cart.clone();
        }
        let hits = ds.lookup(entry.crc, entry.size, None, None, &syses);
        if !hits.is_empty() {
            // A zipped .nes entry: its stored CRC is of the headered
            // file, and the DAT carries headered rows too — a raw hit
            // is expected. (Headerless-content matching inside
            // containers would need decompression; not attempted.)
            let pref: Vec<&str> = systems_for_extension(&eext).to_vec();
            return Some(resolve_hits(
                ds,
                hits,
                &pref,
                entry.crc,
                [0; 16],
                [0; 20],
                container,
                file_size,
            ));
        }
    }
    None
}

fn identify_zip(
    ds: &RomDataset,
    path: &Path,
    file_size: u64,
) -> Result<Option<Identification>, String> {
    let entries = match zip_entries(path) {
        Ok(e) => e,
        Err(_) => return Ok(None), // unreadable as a zip: not a ROM zip
    };
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    if !stem.is_empty() {
        if let Some(id) = identify_set(ds, &stem, &entries, file_size) {
            return Ok(Some(id));
        }
    }
    Ok(contained_hit(ds, &entries, "zip", file_size))
}

// ---------------------------------------------------------------------------
// Archives (7z / rar) and CD sets (cue / ccd / gdi)
// ---------------------------------------------------------------------------

use std::process::{Command, Stdio};

/// Parse `7z l -slt` output: member blocks after the dashed
/// separator, each a set of "Key = Value" lines. Directory rows are
/// dropped. A member whose format carries no stored CRC gets crc 0
/// (it simply never wins a stored-CRC match).
fn parse_7z_listing(text: &str) -> Vec<ZipEntry> {
    let text = text.replace("\r\n", "\n");
    let Some(sep) = text.find("\n----------") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for block in text[sep..].split("\n\n") {
        let mut path: Option<String> = None;
        let mut size: u64 = 0;
        let mut crc: u32 = 0;
        let mut is_dir = false;
        for line in block.lines() {
            let Some((key, value)) = line.split_once(" = ") else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "Path" => path = Some(value.to_string()),
                "Size" => size = value.parse().unwrap_or(0),
                "CRC" => {
                    crc = u32::from_str_radix(value, 16).unwrap_or(0);
                }
                "Folder" => is_dir = value == "+",
                "Attributes" => {
                    if value.starts_with('D') {
                        is_dir = true;
                    }
                }
                _ => {}
            }
        }
        if let Some(name) = path {
            if !is_dir && !name.ends_with('/') {
                out.push(ZipEntry { name, crc, size });
            }
        }
    }
    out
}

/// bsdtar fallback member list (names + sizes; CRCs unknown = 0).
/// Mirrors the archive browser's parsing in commands.rs.
fn bsdtar_members(path: &Path) -> Option<Vec<ZipEntry>> {
    let names_out = Command::new("bsdtar")
        .arg("-tf")
        .arg(path)
        .output()
        .ok()?;
    if !names_out.status.success() {
        return None;
    }
    let mut sizes: HashMap<String, u64> = HashMap::new();
    if let Ok(vout) = Command::new("bsdtar").arg("-tvf").arg(path).output() {
        for line in String::from_utf8_lossy(&vout.stdout).lines() {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() >= 9 {
                if let Ok(size) = fields[4].parse::<u64>() {
                    sizes.insert(fields[8..].join(" "), size);
                }
            }
        }
    }
    let mut out = Vec::new();
    for line in String::from_utf8_lossy(&names_out.stdout).lines() {
        let p = line.trim_end_matches('\r');
        if p.is_empty() || p.ends_with('/') {
            continue;
        }
        out.push(ZipEntry {
            size: sizes.get(p).copied().unwrap_or(0),
            name: p.to_string(),
            crc: 0,
        });
    }
    Some(out)
}

/// Member list of a 7z/rar archive: 7z's technical listing first
/// (stored CRCs make set/contained matching decompression-free);
/// bsdtar when 7z is unavailable or cannot read the archive.
fn archive_members(path: &Path) -> Option<Vec<ZipEntry>> {
    if let Ok(out) = Command::new("7z")
        .arg("l")
        .arg("-slt")
        .arg("--")
        .arg(path)
        .output()
    {
        if out.status.success() {
            let entries = parse_7z_listing(&String::from_utf8_lossy(&out.stdout));
            if !entries.is_empty() {
                return Some(entries);
            }
        }
    }
    bsdtar_members(path)
}

/// Stream one archive member to stdout (7z, falling back to bsdtar).
fn spawn_member_stream(archive: &Path, member: &str) -> std::io::Result<std::process::Child> {
    match Command::new("7z")
        .args(["e", "-so", "--"])
        .arg(archive)
        .arg(member)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => Ok(c),
        Err(_) => Command::new("bsdtar")
            .arg("-xOf")
            .arg(archive)
            .arg(member)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn(),
    }
}

/// Read up to `cap` bytes of one member (cue sheets are small).
fn read_member_bytes(archive: &Path, member: &str, cap: usize) -> Option<Vec<u8>> {
    let mut child = spawn_member_stream(archive, member).ok()?;
    let mut stdout = child.stdout.take()?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    while buf.len() < cap {
        match stdout.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                let take = n.min(cap - buf.len());
                buf.extend_from_slice(&chunk[..take]);
            }
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    Some(buf)
}

/// Hash exactly [start, start+len) of a byte stream; returns the
/// hashes plus how many bytes were actually hashed (a short stream
/// simply cannot match — callers compare against `len`).
fn hash_span_reader<R: Read>(
    mut reader: R,
    start: u64,
    len: u64,
) -> std::io::Result<(u32, [u8; 16], [u8; 20], u64)> {
    let mut sink = HashSink::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut to_skip = start;
    while to_skip > 0 {
        let want = (to_skip as usize).min(buf.len());
        let n = reader.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        to_skip -= n as u64;
    }
    let mut hashed = 0u64;
    while hashed < len {
        let want = ((len - hashed) as usize).min(buf.len());
        let n = reader.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        sink.push(&buf[..n]);
        hashed += n as u64;
    }
    let (crc, md5, sha1, _) = sink.finish();
    Ok((crc, md5, sha1, hashed))
}

fn hash_file_span(
    path: &Path,
    start: u64,
    len: u64,
) -> std::io::Result<(u32, [u8; 16], [u8; 20], u64)> {
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(start))?;
    hash_span_reader(f, 0, len)
}

fn hash_member_span(
    archive: &Path,
    member: &str,
    start: u64,
    len: u64,
) -> std::io::Result<(u32, [u8; 16], [u8; 20], u64)> {
    let mut child = spawn_member_stream(archive, member)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "no stdout"))?;
    let r = hash_span_reader(stdout, start, len);
    let _ = child.kill();
    let _ = child.wait();
    r
}

// -- cue sheets --------------------------------------------------------------

#[derive(Debug, Clone)]
struct CueTrack {
    data: bool,
    sector_size: u64,
    index01: u64,
    points: Vec<u64>,
}

#[derive(Debug, Clone)]
struct CueSheet {
    file_name: String,
    tracks: Vec<CueTrack>,
}

/// "04:05:69" (mm:ss:ff at 75 frames/s) → sector number.
fn parse_msf(s: &str) -> Option<u64> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let m: u64 = parts[0].trim().parse().ok()?;
    let sec: u64 = parts[1].trim().parse().ok()?;
    let f: u64 = parts[2].trim().parse().ok()?;
    Some((m * 60 + sec) * 75 + f)
}

/// Parse a cue sheet into its FILE blocks and track maps. Lenient by
/// design: Redump/CloneCD rips are regular, and anything unparsable
/// just yields no spans to try.
fn parse_cue(text: &str) -> Vec<CueSheet> {
    let mut sheets: Vec<CueSheet> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("FILE ") {
            let rest = rest.trim();
            let name = if let Some(q) = rest.strip_prefix('"') {
                q.split('"').next().unwrap_or("").to_string()
            } else {
                rest.split_whitespace().next().unwrap_or("").to_string()
            };
            sheets.push(CueSheet {
                file_name: name,
                tracks: Vec::new(),
            });
        } else if line.starts_with("TRACK ") {
            let mode = line.split_whitespace().nth(2).unwrap_or("").to_string();
            let sector_size = mode
                .rsplit('/')
                .next()
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(2352);
            if let Some(sheet) = sheets.last_mut() {
                sheet.tracks.push(CueTrack {
                    data: !mode.eq_ignore_ascii_case("AUDIO"),
                    sector_size,
                    index01: 0,
                    points: Vec::new(),
                });
            }
        } else if line.starts_with("INDEX ") {
            let mut parts = line.split_whitespace();
            let _ = parts.next();
            let which = parts.next().unwrap_or("");
            let msf = parts.next().unwrap_or("");
            if let Some(sec) = parse_msf(msf) {
                if let Some(track) = sheets.last_mut().and_then(|s| s.tracks.last_mut()) {
                    track.points.push(sec);
                    if which == "01" {
                        track.index01 = sec;
                    }
                }
            }
        }
    }
    for sheet in &mut sheets {
        for t in &mut sheet.tracks {
            t.points.sort_unstable();
            if t.index01 == 0 {
                t.index01 = t.points.first().copied().unwrap_or(0);
            }
        }
    }
    sheets
}

/// CD-system preference for span lookups: disc systems first (in a
/// sane order), then every other dataset system — the size gate has
/// already decided a span is worth hashing, and side hashes arbitrate.
fn cd_pref(ds: &RomDataset) -> (Vec<&str>, Vec<u8>, Vec<u8>) {
    const CD_KEYS: &[&str] = &[
        "neogeocd", "psx", "saturn", "segacd", "dreamcast", "amigacd32", "cdi", "pcfx", "3do",
        "jaguar", "tg16",
    ];
    let mut pref: Vec<&str> = Vec::new();
    let mut cd_syses: Vec<u8> = Vec::new();
    for k in CD_KEYS {
        if let Some(idx) = ds.sys_index(k) {
            pref.push(k);
            cd_syses.push(idx);
        }
    }
    for (k, _) in ds.systems() {
        if !pref.contains(&k.as_str()) {
            pref.push(k.as_str());
        }
    }
    let all: Vec<u8> = (0..ds.systems().len()).map(|i| i as u8).collect();
    (pref, cd_syses, all)
}

/// Hash one whole span (a full track file, a whole image) and look
/// it up, CD systems preferred.
fn identify_whole_span<F>(
    ds: &RomDataset,
    len: u64,
    container: &'static str,
    member: Option<String>,
    file_size: u64,
    hash_span: &F,
) -> Option<Identification>
where
    F: Fn(u64, u64) -> std::io::Result<(u32, [u8; 16], [u8; 20], u64)>,
{
    let (pref, cd_syses, all) = cd_pref(ds);
    if !cd_syses.iter().any(|&s| ds.size_known(s, len)) {
        return None;
    }
    let (crc, md5, sha1, actual) = hash_span(0, len).ok()?;
    if actual != len {
        return None;
    }
    let hits = ds.lookup(crc, len, Some(&md5), Some(&sha1), &all);
    if hits.is_empty() {
        return None;
    }
    let mut id = resolve_hits(ds, hits, &pref, crc, md5, sha1, container, file_size);
    id.member = member;
    Some(id)
}

/// Identify a disc image from its cue sheet: for each data track,
/// the span runs from its INDEX 01 to the next index point in the
/// file (the following track's INDEX 00 — pregap included — is the
/// Redump per-track model, proven on TruRip) or to the image's end.
fn identify_cd_spans<F>(
    ds: &RomDataset,
    sheet: &CueSheet,
    image_len: u64,
    container: &'static str,
    member: Option<String>,
    file_size: u64,
    hash_span: &F,
) -> Option<Identification>
where
    F: Fn(u64, u64) -> std::io::Result<(u32, [u8; 16], [u8; 20], u64)>,
{
    let (pref, cd_syses, all) = cd_pref(ds);
    let mut all_points: Vec<u64> = sheet
        .tracks
        .iter()
        .flat_map(|t| t.points.iter().copied())
        .collect();
    all_points.sort_unstable();
    all_points.dedup();
    for track in &sheet.tracks {
        if !track.data {
            continue;
        }
        let ss = track.sector_size.max(1);
        let start = track.index01;
        let end_sector = all_points.iter().copied().filter(|&p| p > start).min();
        let start_b = start.saturating_mul(ss);
        let end_b = match end_sector {
            Some(e) => (e.saturating_mul(ss)).min(image_len),
            None => image_len,
        };
        if end_b <= start_b {
            continue;
        }
        let len = end_b - start_b;
        if !cd_syses.iter().any(|&s| ds.size_known(s, len)) {
            continue;
        }
        let Ok((crc, md5, sha1, actual)) = hash_span(start_b, len) else {
            continue;
        };
        if actual != len {
            continue;
        }
        let hits = ds.lookup(crc, len, Some(&md5), Some(&sha1), &all);
        if hits.is_empty() {
            continue;
        }
        let mut id = resolve_hits(ds, hits, &pref, crc, md5, sha1, container, file_size);
        id.member = member.clone();
        return Some(id);
    }
    None
}

// -- loose disc-set anchors ----------------------------------------------------

fn identify_cue_anchor(
    ds: &RomDataset,
    path: &Path,
    file_size: u64,
) -> Result<Option<Identification>, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&bytes);
    let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    for sheet in parse_cue(&text) {
        let img = dir.join(&sheet.file_name);
        let Ok(meta) = std::fs::metadata(&img) else {
            continue;
        };
        let img_len = meta.len();
        let img_disp = img.to_string_lossy().into_owned();
        let id = identify_cd_spans(
            ds,
            &sheet,
            img_len,
            "cd",
            Some(img_disp),
            file_size,
            &|s, l| hash_file_span(&img, s, l),
        );
        if id.is_some() {
            return Ok(id);
        }
    }
    Ok(None)
}

fn identify_ccd_anchor(
    ds: &RomDataset,
    path: &Path,
    file_size: u64,
) -> Result<Option<Identification>, String> {
    // A same-stem .cue beside the .ccd describes the same set.
    let cue = path.with_extension("cue");
    if cue.exists() {
        if let Some(id) = identify_cue_anchor(ds, &cue, file_size)? {
            return Ok(Some(id));
        }
    }
    // Otherwise a same-stem image whose whole length is one track.
    for ext in ["img", "bin"] {
        let img = path.with_extension(ext);
        let Ok(meta) = std::fs::metadata(&img) else {
            continue;
        };
        let len = meta.len();
        let img_disp = img.to_string_lossy().into_owned();
        let id = identify_whole_span(ds, len, "cd", Some(img_disp), file_size, &|s, l| {
            hash_file_span(&img, s, l)
        });
        if id.is_some() {
            return Ok(id);
        }
    }
    Ok(None)
}

fn identify_gdi_anchor(
    ds: &RomDataset,
    path: &Path,
    file_size: u64,
) -> Result<Option<Identification>, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&bytes);
    let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    for line in text.lines() {
        let line = line.trim();
        // 1 0 4 2352 "track01.bin" 0 — track, lba, type, sector size.
        let Some(q1) = line.find('"') else { continue };
        let Some(q2) = line[q1 + 1..].find('"') else {
            continue;
        };
        let name = &line[q1 + 1..q1 + 1 + q2];
        let fields: Vec<&str> = line[..q1].split_whitespace().collect();
        if fields.len() < 4 || fields[2] != "4" {
            continue; // data tracks only
        }
        let img = dir.join(name);
        let Ok(meta) = std::fs::metadata(&img) else {
            continue;
        };
        let len = meta.len();
        let img_disp = img.to_string_lossy().into_owned();
        let id = identify_whole_span(ds, len, "cd", Some(img_disp), file_size, &|s, l| {
            hash_file_span(&img, s, l)
        });
        if id.is_some() {
            return Ok(id);
        }
    }
    Ok(None)
}

/// Identify a CHD disc image: open it with the native reader, then
/// for each data track try the candidate span models — the raw
/// 2352-byte sector span (Redump CD model) and, for cooked MODE1
/// tracks, the 2048-byte user-data span (the PS2 DVD/ISO model) —
/// each gated on the dataset knowing the span's size somewhere, all
/// of it arbitrated by the DAT lookup as everywhere else. A CHD that
/// cannot be parsed or whose needed hunks cannot be decoded is an
/// honest Err; a parsed CHD with no matching row is Ok(None).
fn identify_chd_anchor(
    ds: &RomDataset,
    path: &Path,
    file_size: u64,
) -> Result<Option<Identification>, String> {
    let mut chd = crate::chd::ChdFile::open(path).map_err(|e| e.to_string())?;
    if chd.has_parent() {
        return Err(
            "chd: delta image \u{2014} requires its parent CHD, not identified".into(),
        );
    }
    let (pref, _cd_syses, all) = cd_pref(ds);
    // Preference order: the CD systems with PS2 ranked alongside —
    // most CHDs in the wild are PS2, Dreamcast, or PSX images.
    let mut pref2: Vec<&str> = Vec::new();
    for k in pref.iter() {
        pref2.push(k);
        if *k == "psx" {
            pref2.push("ps2");
        }
    }
    if !pref2.contains(&"ps2") {
        pref2.insert(0, "ps2");
    }
    let tracks: Vec<(u32, bool, bool)> = chd
        .tracks()
        .iter()
        .map(|t| (t.frames, t.is_data(), t.is_cooked()))
        .collect();
    for (ti, (frames, is_data, cooked)) in tracks.into_iter().enumerate() {
        if !is_data {
            continue;
        }
        let mut spans: Vec<(u64, crate::chd::SpanKind)> =
            vec![(frames as u64 * 2352, crate::chd::SpanKind::Raw2352)];
        if cooked {
            spans.insert(0, (frames as u64 * 2048, crate::chd::SpanKind::User2048));
        }
        for (len, kind) in spans {
            if !all.iter().any(|&s| ds.size_known(s, len)) {
                continue;
            }
            let (crc, md5, sha1, actual) =
                chd.hash_track_span(ti, kind).map_err(|e| e.to_string())?;
            if actual != len {
                continue;
            }
            let hits = ds.lookup(crc, len, Some(&md5), Some(&sha1), &all);
            if hits.is_empty() {
                continue;
            }
            return Ok(Some(resolve_hits(
                ds,
                hits,
                &pref2,
                crc,
                md5,
                sha1,
                "chd",
                file_size,
            )));
        }
    }
    Ok(None)
}

// -- archives ------------------------------------------------------------------

/// Variant preference among an archive's identifications: a clean
/// release beats a patched/hack variant; World/USA beats Europe
/// beats Japan beats the rest (mirrors the grouping defaults).
fn variant_rank(dat_name: &str) -> (u8, u8) {
    let l = dat_name.to_lowercase();
    let patched = if l.contains("patched") || l.contains("hack") {
        1
    } else {
        0
    };
    let region = if l.contains("(world") || l.contains("(usa") {
        0
    } else if l.contains("(europe") {
        1
    } else if l.contains("(japan") {
        2
    } else {
        3
    };
    (patched, region)
}

/// Identify a 7z/rar archive: MAME-style set match by stem, then
/// every cue/ccd disc set packed inside (one identification per
/// variant folder), then — when no disc set matched — the zip-style
/// contained-ROM match. Results are sorted best-variant-first.
fn identify_archive(
    ds: &RomDataset,
    path: &Path,
    file_size: u64,
    ext: &str,
) -> Result<Vec<Identification>, String> {
    let Some(members) = archive_members(path) else {
        return Ok(Vec::new());
    };
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    if !stem.is_empty() {
        if let Some(id) = identify_set(ds, &stem, &members, file_size) {
            return Ok(vec![id]);
        }
    }
    let container: &'static str = if ext == "7z" { "7z" } else { "rar" };
    let parent_of = |name: &str| -> String {
        name.rsplit_once('/')
            .map(|(d, _)| d.to_string())
            .unwrap_or_default()
    };
    let cue_dirs: HashSet<String> = members
        .iter()
        .filter(|m| m.name.to_lowercase().ends_with(".cue"))
        .map(|m| parent_of(&m.name))
        .collect();
    let mut ids: Vec<Identification> = Vec::new();
    for m in members.iter().filter(|m| {
        let l = m.name.to_lowercase();
        l.ends_with(".cue") || l.ends_with(".ccd")
    }) {
        let lower = m.name.to_lowercase();
        let folder = parent_of(&m.name);
        if lower.ends_with(".ccd") && cue_dirs.contains(&folder) {
            continue; // the sibling .cue describes this set
        }
        if lower.ends_with(".cue") {
            let Some(bytes) = read_member_bytes(path, &m.name, 2 * 1024 * 1024) else {
                continue;
            };
            let text = String::from_utf8_lossy(&bytes);
            for sheet in parse_cue(&text) {
                let want_base = sheet
                    .file_name
                    .rsplit('/')
                    .next()
                    .unwrap_or(&sheet.file_name)
                    .to_lowercase();
                let Some(img_member) = members.iter().find(|e| {
                    parent_of(&e.name) == folder
                        && e.name
                            .rsplit('/')
                            .next()
                            .unwrap_or("")
                            .eq_ignore_ascii_case(&want_base)
                }) else {
                    continue;
                };
                let img_len = img_member.size;
                let img_name = img_member.name.clone();
                if let Some(id) = identify_cd_spans(
                    ds,
                    &sheet,
                    img_len,
                    container,
                    Some(img_name.clone()),
                    file_size,
                    &|s, l| hash_member_span(path, &img_name, s, l),
                ) {
                    ids.push(id);
                }
            }
        } else {
            // .ccd without a .cue: whole-image attempt on the
            // same-stem image member in the same folder.
            let stem_base = m
                .name
                .rsplit('/')
                .next()
                .unwrap_or("")
                .rsplit_once('.')
                .map(|(s, _)| s.to_lowercase())
                .unwrap_or_default();
            for cand_ext in ["img", "bin"] {
                let target = format!("{stem_base}.{cand_ext}");
                let Some(img_member) = members.iter().find(|e| {
                    parent_of(&e.name) == folder
                        && e.name
                            .rsplit('/')
                            .next()
                            .unwrap_or("")
                            .eq_ignore_ascii_case(&target)
                }) else {
                    continue;
                };
                let img_len = img_member.size;
                let img_name = img_member.name.clone();
                if let Some(id) = identify_whole_span(
                    ds,
                    img_len,
                    container,
                    Some(img_name.clone()),
                    file_size,
                    &|s, l| hash_member_span(path, &img_name, s, l),
                ) {
                    ids.push(id);
                    break;
                }
            }
        }
    }
    // One identification per (system, content, member) — a set whose
    // .ccd and .cue both resolve must not double-count.
    let mut seen: HashSet<(String, u32, String)> = HashSet::new();
    ids.retain(|id| {
        seen.insert((
            id.sys.clone(),
            id.crc,
            id.member.clone().unwrap_or_default(),
        ))
    });
    if ids.is_empty() {
        if let Some(id) = contained_hit(ds, &members, container, file_size) {
            ids.push(id);
        }
    }
    ids.sort_by(|a, b| {
        variant_rank(&a.dat_name)
            .cmp(&variant_rank(&b.dat_name))
            .then_with(|| a.dat_name.cmp(&b.dat_name))
    });
    Ok(ids)
}

// -- name parsing (Needs-ID grouping now, variant grouping later) --------------

/// The display base title of a ROM file/DAT name with its tag groups
/// removed: "(USA) [p1][!]"-style parenthesized and bracketed groups
/// are variant metadata, not title. ", The" inversions are undone.
/// Pure and shared: the Needs-ID shelf groups by this today; the
/// variant grouper builds its keys from it next.
pub fn parsed_base_title(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut depth_paren = 0i32;
    let mut depth_brack = 0i32;
    for ch in raw.chars() {
        match ch {
            '(' => depth_paren += 1,
            ')' => depth_paren = (depth_paren - 1).max(0),
            '[' => depth_brack += 1,
            ']' => depth_brack = (depth_brack - 1).max(0),
            _ => {
                if depth_paren == 0 && depth_brack == 0 {
                    out.push(ch);
                }
            }
        }
    }
    let mut s = out.split_whitespace().collect::<Vec<_>>().join(" ");
    s = s.trim_matches(|c| c == ' ' || c == '-' || c == ',').to_string();
    let lower = s.to_lowercase();
    if let Some(prefix) = lower.strip_suffix(", the") {
        let keep = &s[..prefix.len()];
        s = format!("The {keep}");
    }
    s
}

/// The variant group key (Phase 3): one game = one key. Platform +
/// the tag-stripped base title, lowercased. Sequel-safe by
/// construction — title-internal numbers are never parenthesized in
/// DAT names, so stripping tag groups can't merge "… '94" with
/// "… '97" (AUDIT §8).
pub fn rom_group_key(platform: &str, title: &str) -> String {
    format!(
        "{platform}\u{1}{}",
        parsed_base_title(title).to_lowercase()
    )
}

/// One version of a game, parsed from its DAT title (or a member /
/// file name when that's all there is). Everything the variant
/// picker shows and the default-variant rule reads. The parse is
/// descriptive, never a claim: identity still comes from hashes.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RomVariant {
    /// Region group verbatim ("USA", "Japan, USA"), when named.
    pub region: Option<String>,
    /// Revision group verbatim ("Rev 1", "v1.3"), when named.
    pub revision: Option<String>,
    /// release | beta | proto | sample | demo | promo.
    pub status: String,
    /// Language group verbatim ("En,Ja"), when named.
    pub languages: Option<String>,
    /// Canonical flag tokens: pirate, unlicensed, patched, hack,
    /// trainer, bad-dump, overdump, alternate, fixed, verified.
    pub flags: Vec<String>,
    /// Disc group verbatim ("Disc 1") — multi-disc games fold into
    /// one group (Q4); the disc rides here so the picker can differ.
    pub disc: Option<String>,
    /// Human one-liner: "USA · Rev 1 · Pirate" — region, revision,
    /// status, languages, disc, flag labels, then any unrecognized
    /// groups verbatim (a publisher tag like (Tengen) distinguishes
    /// versions, so it is preserved rather than classified away).
    pub descriptor: String,
    /// Default-rule ranks: region USA 0 < World 1 < Europe 2 < other
    /// 3 < unnamed 4 (multi-region groups rank by their best part).
    pub region_rank: u8,
    /// Release 0; beta/proto/sample/demo/promo 1.
    pub status_rank: u8,
    /// major*1000 + minor; -1 when no revision is named. Newest wins.
    pub revision_key: i64,
    /// Patched / hack / trainer / bad-dump / overdump: pickable and
    /// badged (Q2), but NEVER the default while a clean variant
    /// exists (Q1 lean + Dickles' default order).
    pub never_default: bool,
}

const VARIANT_REGIONS: &[&str] = &[
    "USA", "Europe", "Japan", "World", "UK", "France", "Germany", "Spain",
    "Italy", "Australia", "Canada", "Korea", "Brazil", "Asia", "Netherlands",
    "Sweden", "Denmark", "Finland", "Norway", "Russia", "China", "Taiwan",
    "Hong Kong", "Latin America", "Mexico", "Portugal", "Greece", "Turkey",
    "India", "New Zealand", "South Africa", "Argentina", "Chile", "Peru",
    "Colombia", "Venezuela", "Belgium", "Austria", "Switzerland", "Poland",
    "Czech Republic", "Hungary", "Romania", "Bulgaria", "Croatia", "Ireland",
    "Scandinavia", "Middle East", "Africa", "Oceania", "Global", "Export",
];

const LANG_CODES: &[&str] = &[
    "En", "Ja", "Fr", "De", "Es", "It", "Ko", "Zh", "Pt", "Nl", "Sv", "Da",
    "Fi", "No", "Pl", "Ru", "Cs", "Hu", "El", "Tr", "Ar", "He",
];

fn region_rank_of(part: &str) -> u8 {
    match part {
        "USA" => 0,
        "World" => 1,
        "Europe" => 2,
        _ => 3,
    }
}

fn flag_label(token: &str) -> &str {
    match token {
        "pirate" => "Pirate",
        "unlicensed" => "Unlicensed",
        "patched" => "Patched",
        "hack" => "Hack",
        "trainer" => "Trainer",
        "bad-dump" => "Bad dump",
        "overdump" => "Overdump",
        "alternate" => "Alternate",
        "fixed" => "Fixed",
        "verified" => "Verified",
        _ => token,
    }
}

/// Parse the tag groups of a DAT title / member name into a variant
/// descriptor. Parenthesized groups classify as region / disc /
/// revision / status / languages / flags; anything unrecognized is
/// kept verbatim for the descriptor. Bracketed groups are GoodTools
/// tags ([p1], [!], [b]…) and classify by their leading letter.
pub fn variant_of_title(title: &str) -> RomVariant {
    // Collect (...) and [...] group contents, in order.
    let mut paren_groups: Vec<String> = Vec::new();
    let mut brack_groups: Vec<String> = Vec::new();
    let chars: Vec<char> = title.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        let (open, close, sink) = match chars[i] {
            '(' => ('(', ')', 0),
            '[' => ('[', ']', 1),
            _ => {
                i += 1;
                continue;
            }
        };
        let _ = open;
        let mut j = i + 1;
        let mut content = String::new();
        while j < chars.len() && chars[j] != close {
            content.push(chars[j]);
            j += 1;
        }
        if j < chars.len() {
            let g = content.trim().to_string();
            if !g.is_empty() {
                if sink == 0 {
                    paren_groups.push(g);
                } else {
                    brack_groups.push(g);
                }
            }
            i = j + 1;
        } else {
            break;
        }
    }

    let mut v = RomVariant {
        status: "release".to_string(),
        region_rank: 4,
        revision_key: -1,
        ..Default::default()
    };
    let mut others: Vec<String> = Vec::new();
    let mut push_flag = |v: &mut RomVariant, token: &str| {
        if !v.flags.iter().any(|f| f == token) {
            v.flags.push(token.to_string());
        }
    };

    for group in &paren_groups {
        let lower = group.to_lowercase();
        let parts: Vec<&str> = group.split(',').map(|p| p.trim()).collect();
        // Region: every part a known region.
        if v.region.is_none()
            && !parts.is_empty()
            && parts.iter().all(|p| VARIANT_REGIONS.contains(p))
        {
            v.region = Some(group.clone());
            v.region_rank = parts.iter().map(|p| region_rank_of(p)).min().unwrap_or(3);
            continue;
        }
        // Disc: "Disc 1", "Disk 2", "CD 1".
        let first_word = lower.split_whitespace().next().unwrap_or("");
        if matches!(first_word, "disc" | "disk" | "cd")
            && lower.chars().any(|c| c.is_ascii_digit())
            && v.disc.is_none()
        {
            v.disc = Some(group.clone());
            continue;
        }
        // Revision: "Rev …" or "v<digit>…".
        let is_rev = lower.starts_with("rev")
            || (lower.len() > 1
                && lower.starts_with('v')
                && lower.as_bytes()[1].is_ascii_digit());
        if is_rev && v.revision.is_none() {
            v.revision = Some(group.clone());
            let nums: Vec<i64> = group
                .split(|c: char| !(c.is_ascii_digit()))
                .filter(|s| !s.is_empty())
                .filter_map(|s| s.parse().ok())
                .collect();
            v.revision_key = if !nums.is_empty() {
                nums[0] * 1000 + nums.get(1).copied().unwrap_or(0)
            } else if let Some(ch) =
                group.chars().find(|c| c.is_ascii_alphabetic())
            {
                // "Rev A" style: A = 1.
                (ch.to_ascii_lowercase() as i64 - 'a' as i64 + 1) * 1000
            } else {
                0
            };
            continue;
        }
        // Status.
        let status = match lower.as_str() {
            "beta" => Some("beta"),
            "proto" | "prototype" => Some("proto"),
            "sample" => Some("sample"),
            "demo" => Some("demo"),
            "promo" => Some("promo"),
            _ if lower.starts_with("beta ") => Some("beta"),
            _ if lower.starts_with("proto ") => Some("proto"),
            _ => None,
        };
        if let Some(s) = status {
            if v.status == "release" {
                v.status = s.to_string();
                v.status_rank = 1;
            }
            continue;
        }
        // Languages: every part a 2-letter code, at least one known.
        if v.languages.is_none()
            && !parts.is_empty()
            && parts.iter().all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_alphabetic()))
            && parts.iter().any(|p| LANG_CODES.contains(p))
        {
            v.languages = Some(group.clone());
            continue;
        }
        // Flags named outright.
        let mut flagged = false;
        if lower == "pirate" {
            push_flag(&mut v, "pirate");
            flagged = true;
        }
        if lower == "unl" || lower.contains("unlicensed") {
            push_flag(&mut v, "unlicensed");
            flagged = true;
        }
        if lower.contains("patched") {
            push_flag(&mut v, "patched");
            flagged = true;
        }
        if lower == "hack" || lower.contains("hack ") {
            push_flag(&mut v, "hack");
            flagged = true;
        }
        if flagged {
            continue;
        }
        others.push(group.clone());
    }

    for group in &brack_groups {
        let lower = group.to_lowercase();
        let token = match lower.chars().next() {
            Some('!') => Some("verified"),
            Some('b') => Some("bad-dump"),
            Some('o') => Some("overdump"),
            Some('t') => Some("trainer"),
            Some('h') => Some("hack"),
            Some('p') => Some("pirate"),
            Some('a') => Some("alternate"),
            Some('f') => Some("fixed"),
            _ => None,
        };
        match token {
            Some(t) => push_flag(&mut v, t),
            None => others.push(format!("[{group}]")),
        }
    }

    v.never_default = v
        .flags
        .iter()
        .any(|f| matches!(f.as_str(), "patched" | "hack" | "trainer" | "bad-dump" | "overdump"));

    let mut parts: Vec<String> = Vec::new();
    if let Some(r) = &v.region {
        parts.push(r.clone());
    }
    if let Some(r) = &v.revision {
        parts.push(r.clone());
    }
    if v.status != "release" {
        let mut s = v.status.clone();
        s.replace_range(..1, &v.status[..1].to_uppercase());
        parts.push(s);
    }
    if let Some(l) = &v.languages {
        parts.push(l.clone());
    }
    if let Some(d) = &v.disc {
        parts.push(d.clone());
    }
    for f in &v.flags {
        parts.push(flag_label(f).to_string());
    }
    parts.extend(others);
    v.descriptor = parts.join(" · ");
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal FABLEDAT1 dataset in memory: systems nes+arcade,
    /// rows: (crc 0x1111, size 100, "Alpha (USA)") unique; ambiguous pair
    /// (crc 0x2222, size 50) in nes ("Beta") + arcade-ish second sys row
    /// with different md5; one arcade set "alpha" with roms [(0xAAAA,10)].
    fn tiny_dataset() -> RomDataset {
        let mut sys_blob: Vec<u8> = Vec::new();
        for (k, d) in [("nes", "NES"), ("arcade", "Arcade")] {
            sys_blob.push(k.len() as u8);
            sys_blob.extend_from_slice(k.as_bytes());
            sys_blob.push(d.len() as u8);
            sys_blob.extend_from_slice(d.as_bytes());
        }
        let mut names: Vec<u8> = Vec::new();
        let mut name_offs: HashMap<String, u32> = HashMap::new();
        let mut noff = |n: &str, names: &mut Vec<u8>| -> u32 {
            if let Some(o) = name_offs.get(n) {
                return *o;
            }
            let o = names.len() as u32;
            names.extend_from_slice(n.as_bytes());
            names.push(0);
            name_offs.insert(n.to_string(), o);
            o
        };
        let alpha = noff("Alpha (USA)", &mut names);
        let beta = noff("Beta", &mut names);
        let beta2 = noff("Beta Arcade", &mut names);
        let md5_a = [7u8; 16];
        let md5_b = [9u8; 16];
        // rows sorted by (crc,size,sys)
        let rows: [(u32, u64, u32, u32, u32, u8); 3] = [
            (0x1111, 100, alpha, NONE32, NONE32, 0),
            (0x2222, 50, beta, 0, NONE32, 0),      // md5 idx 0 = md5_a
            (0x2222, 50, beta2, 1, NONE32, 1),     // md5 idx 1 = md5_b
        ];
        let mut shorts: Vec<u8> = Vec::new();
        shorts.extend_from_slice(b"alpha\0");
        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(b"FABLEDAT");
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&3u32.to_le_bytes()); // rows
        out.extend_from_slice(&2u32.to_le_bytes()); // md5 count
        out.extend_from_slice(&0u32.to_le_bytes()); // sha1 count
        out.extend_from_slice(&(names.len() as u32).to_le_bytes());
        out.extend_from_slice(&(sys_blob.len() as u32).to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes()); // sets
        out.extend_from_slice(&1u32.to_le_bytes()); // set roms
        out.extend_from_slice(&(shorts.len() as u32).to_le_bytes());
        out.extend_from_slice(&sys_blob);
        for (crc, size, no, mi, si, sys) in rows {
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&size.to_le_bytes());
            out.extend_from_slice(&no.to_le_bytes());
            out.extend_from_slice(&mi.to_le_bytes());
            out.extend_from_slice(&si.to_le_bytes());
            out.push(sys);
            out.push(0);
            out.extend_from_slice(&0u16.to_le_bytes());
        }
        // set: sys 1 (arcade), count 1, short_off 0, disp_off alpha, parent none, start 0
        out.push(1);
        out.push(0);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&alpha.to_le_bytes());
        out.extend_from_slice(&NONE32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        // set rom
        out.extend_from_slice(&0xAAAAu32.to_le_bytes());
        out.extend_from_slice(&10u64.to_le_bytes());
        out.extend_from_slice(&names);
        out.extend_from_slice(&shorts);
        out.extend_from_slice(&md5_a);
        out.extend_from_slice(&md5_b);
        RomDataset::load(out).expect("tiny dataset loads")
    }

    #[test]
    fn lookup_unique_and_ambiguous() {
        let ds = tiny_dataset();
        let hits = ds.lookup(0x1111, 100, None, None, &[0]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "Alpha (USA)");
        // Ambiguous pair, md5 arbitrates: the row whose stored md5
        // disagrees with the file's is dropped.
        let hits = ds.lookup(0x2222, 50, Some(&[9u8; 16]), None, &[0, 1]);
        assert!(hits.iter().any(|h| h.name == "Beta Arcade"));
        assert!(!hits.iter().any(|h| h.name == "Beta"));
        // Size gate
        assert!(ds.size_known(0, 100));
        assert!(!ds.size_known(0, 101));
    }

    #[test]
    fn set_lookup() {
        let ds = tiny_dataset();
        let set = ds.find_set(1, "ALPHA").expect("set found (case-insensitive)");
        assert_eq!(set.display, "Alpha (USA)");
        assert_eq!(set.roms, vec![(0xAAAA, 10)]);
    }

    #[test]
    fn identify_loose_file_with_ines_header() {
        // A .nes whose headered bytes match nothing but whose CONTENT
        // (after the 16B header) must match a row: craft content of
        // 100 bytes and register its crc as the Alpha row instead.
        // Simpler: content == raw row — build a file whose post-header
        // bytes hash (crc32) equals 0x1111 is not feasible by hand, so
        // this test exercises the raw path: file of exactly the Alpha
        // row is impossible too (we don't know preimage). Instead:
        // assert the machinery on a real crc: compute crc32fast of a
        // known payload and build the dataset around it.
        let payload: Vec<u8> = (0u8..100).collect();
        let crc = {
            let mut h = crc32fast::Hasher::new();
            h.update(&payload);
            h.finalize()
        };
        // Rebuild tiny dataset with this crc for the Alpha row.
        let ds = {
            // clone the tiny builder but swap crc — easiest: rebuild
            // via the public path by writing a dataset with load().
            // (Duplication kept small: mutate approach — construct anew.)
            let mut sys_blob: Vec<u8> = Vec::new();
            for (k, d) in [("nes", "NES")] {
                sys_blob.push(k.len() as u8);
                sys_blob.extend_from_slice(k.as_bytes());
                sys_blob.push(d.len() as u8);
                sys_blob.extend_from_slice(d.as_bytes());
            }
            let mut names: Vec<u8> = Vec::new();
            names.extend_from_slice(b"Some Game (USA)\0");
            let mut out: Vec<u8> = Vec::new();
            out.extend_from_slice(b"FABLEDAT");
            out.extend_from_slice(&1u16.to_le_bytes());
            out.extend_from_slice(&1u16.to_le_bytes());
            out.extend_from_slice(&1u32.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&(names.len() as u32).to_le_bytes());
            out.extend_from_slice(&(sys_blob.len() as u32).to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&sys_blob);
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&100u64.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&NONE32.to_le_bytes());
            out.extend_from_slice(&NONE32.to_le_bytes());
            out.push(0);
            out.push(0);
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&names);
            RomDataset::load(out).unwrap()
        };
        // The payload AS a .nes needs the iNES header skipped: payload
        // here is the raw content; make a .nes = header + payload.
        let dir = std::env::temp_dir().join(format!("ufm-romdat-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let raw_path = dir.join("game.unh");
        std::fs::write(&raw_path, &payload).unwrap();
        // .unh → nes raw: matches directly
        let id = identify_file(&ds, &raw_path).unwrap().expect("raw identified");
        assert_eq!(id.dat_name, "Some Game (USA)");
        assert_eq!(id.sys, "nes");
        // .nes with header: content hash must match after the skip.
        let mut nes = b"NES\x1A".to_vec();
        nes.extend_from_slice(&[0u8; 12]);
        nes.extend_from_slice(&payload);
        let nes_path = dir.join("game.nes");
        std::fs::write(&nes_path, &nes).unwrap();
        let id = identify_file(&ds, &nes_path).unwrap().expect("headered identified");
        assert_eq!(id.dat_name, "Some Game (USA)");
        // A random .nes must NOT match.
        let bad_path = dir.join("junk.nes");
        let mut junk = b"NES\x1A".to_vec();
        junk.extend_from_slice(&[0u8; 12]);
        junk.extend_from_slice(&[42u8; 100]);
        std::fs::write(&bad_path, &junk).unwrap();
        assert!(identify_file(&ds, &bad_path).unwrap().is_none());
        // A .md (markdown!) must not even be a candidate.
        let md_path = dir.join("notes.md");
        std::fs::write(&md_path, &payload).unwrap();
        assert!(identify_file(&ds, &md_path).unwrap().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// One-system, one-row dataset: (sys_key, row crc, row size, name).
    fn one_row_dataset(sys_key: &str, crc: u32, size: u64, name: &str) -> RomDataset {
        let mut sys_blob: Vec<u8> = Vec::new();
        sys_blob.push(sys_key.len() as u8);
        sys_blob.extend_from_slice(sys_key.as_bytes());
        sys_blob.push(4u8);
        sys_blob.extend_from_slice(b"TEST");
        let mut names: Vec<u8> = Vec::new();
        names.extend_from_slice(name.as_bytes());
        names.push(0);
        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(b"FABLEDAT");
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&(names.len() as u32).to_le_bytes());
        out.extend_from_slice(&(sys_blob.len() as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&sys_blob);
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&NONE32.to_le_bytes());
        out.extend_from_slice(&NONE32.to_le_bytes());
        out.push(0);
        out.push(0);
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&names);
        RomDataset::load(out).unwrap()
    }

    fn crc_of(payload: &[u8]) -> u32 {
        let mut h = crc32fast::Hasher::new();
        h.update(payload);
        h.finalize()
    }

    #[test]
    fn nomination_includes_archives_and_disc_anchors_but_not_md() {
        let exts = candidate_extensions();
        for e in ["7z", "rar", "cue", "ccd", "chd", "gdi", "zip", "nes"] {
            assert!(exts.contains(&e), "missing {e}");
        }
        assert!(!exts.contains(&"md"), ".md must never be scanned for");
        // img nominates Neo Geo CD now (audit D3).
        assert!(systems_for_extension("img").contains(&"neogeocd"));
    }

    #[test]
    fn seven_z_listing_parses_members_and_skips_dirs() {
        let sample = "7-Zip 23.01\n\nListing archive: /x/Game.7z\n\n--\nPath = /x/Game.7z\nType = 7z\n\n----------\nPath = Game (World)\nSize = 0\nAttributes = DI\nCRC = \n\nPath = Game (World)/Game.cue\nSize = 11605\nAttributes = I\nCRC = DE0F3FE7\n\nPath = Game (World)/Game.img\nSize = 1000\nAttributes = I\nCRC = 37F64C30\n";
        let entries = parse_7z_listing(sample);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "Game (World)/Game.cue");
        assert_eq!(entries[0].crc, 0xDE0F3FE7);
        assert_eq!(entries[1].size, 1000);
        assert_eq!(entries[1].crc, 0x37F64C30);
    }

    #[test]
    fn cue_span_math_matches_the_trurip_proof() {
        // The audit's proven case: Track 01's span ends at Track 02's
        // INDEX 00 (04:05:69 = 18,444 sectors × 2352 = 43,380,288 B).
        let cue = "FILE \"3 Count Bout.img\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 00 04:05:69\n    INDEX 01 04:08:69\n";
        let sheets = parse_cue(cue);
        assert_eq!(sheets.len(), 1);
        let sheet = &sheets[0];
        assert_eq!(sheet.file_name, "3 Count Bout.img");
        assert_eq!(sheet.tracks.len(), 2);
        assert!(sheet.tracks[0].data);
        assert!(!sheet.tracks[1].data);
        assert_eq!(sheet.tracks[0].index01, 0);
        assert_eq!(sheet.tracks[1].points, vec![18444, 18669]);
        assert_eq!(18444 * 2352, 43_380_288);
        assert_eq!(parse_msf("04:05:69"), Some(18444));
    }

    #[test]
    fn parsed_base_title_strips_tags_and_undoes_inversion() {
        assert_eq!(parsed_base_title("Klax (USA) [p1][!]"), "Klax");
        assert_eq!(
            parsed_base_title("King of Fighters '97, The (Japan) (En,Ja)"),
            "The King of Fighters '97"
        );
        assert_eq!(parsed_base_title("3 Count Bout"), "3 Count Bout");
        assert_eq!(parsed_base_title("Game (Disc 1)"), "Game");
    }

    #[test]
    fn loose_cd_set_identifies_by_track_span() {
        // Payload = exactly 10 sectors; the cue's Track 02 INDEX 00
        // sits at sector 10, so Track 01's span IS the payload. The
        // image carries trailing audio bytes that must not matter.
        let payload: Vec<u8> = (0..2352 * 10).map(|i| (i % 251) as u8).collect();
        let ds = one_row_dataset("neogeocd", crc_of(&payload), payload.len() as u64, "Span Game (World)");
        let dir = std::env::temp_dir().join(format!("ufm-cd-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut img = payload.clone();
        img.extend_from_slice(&[7u8; 2352 * 4]);
        std::fs::write(dir.join("game.img"), &img).unwrap();
        std::fs::write(
            dir.join("game.cue"),
            "FILE \"game.img\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 00 00:00:10\n    INDEX 01 00:00:12\n",
        )
        .unwrap();
        let ids = identify_file_all(&ds, &dir.join("game.cue")).unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].sys, "neogeocd");
        assert_eq!(ids[0].dat_name, "Span Game (World)");
        assert_eq!(ids[0].container, "cd");
        assert!(ids[0].member.as_deref().unwrap().ends_with("game.img"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn seven_z_archive_identifies_cd_variant_inside() {
        // The TruRip shape in miniature: a variant folder holding a
        // cue set, packed in a real .7z (skipped when 7z is absent).
        if Command::new("7z").arg("--help").output().is_err() {
            return;
        }
        let payload: Vec<u8> = (0..2352 * 10).map(|i| (i % 241) as u8).collect();
        let ds = one_row_dataset("neogeocd", crc_of(&payload), payload.len() as u64, "Packed Game (World) (En,Ja)");
        let dir = std::env::temp_dir().join(format!("ufm-7z-test-{}", std::process::id()));
        let stage = dir.join("Packed Game (World)");
        std::fs::create_dir_all(&stage).unwrap();
        let mut img = payload.clone();
        img.extend_from_slice(&[3u8; 2352 * 2]);
        std::fs::write(stage.join("Packed Game.img"), &img).unwrap();
        std::fs::write(
            stage.join("Packed Game.cue"),
            "FILE \"Packed Game.img\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 00 00:00:10\n    INDEX 01 00:00:12\n",
        )
        .unwrap();
        let arch = dir.join("Packed Game.7z");
        let status = Command::new("7z")
            .arg("a")
            .arg(&arch)
            .arg("Packed Game (World)")
            .current_dir(&dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        let ids = identify_file_all(&ds, &arch).unwrap();
        assert_eq!(ids.len(), 1, "ids: {ids:?}");
        assert_eq!(ids[0].sys, "neogeocd");
        assert_eq!(ids[0].container, "7z");
        assert_eq!(
            ids[0].member.as_deref(),
            Some("Packed Game (World)/Packed Game.img")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    // -- Phase 3: variant parsing + group keys ---------------------

    #[test]
    fn variant_parse_klax_family() {
        let usa = variant_of_title("Klax (USA) (Unl)");
        assert_eq!(usa.region.as_deref(), Some("USA"));
        assert_eq!(usa.region_rank, 0);
        assert_eq!(usa.flags, vec!["unlicensed".to_string()]);
        assert_eq!(usa.descriptor, "USA · Unlicensed");
        assert!(!usa.never_default);
        let japan = variant_of_title("Klax (Japan)");
        assert_eq!(japan.region_rank, 3);
        // Same game, one group.
        assert_eq!(
            rom_group_key("nes", "Klax (USA) (Unl)"),
            rom_group_key("nes", "Klax (Japan)")
        );
    }

    #[test]
    fn variant_parse_goodtools_dumps_differ() {
        // Mario 16's two dumps: [p1][!] vs [p1][a1] — different
        // versions of one group, neither blocked from default.
        let good = variant_of_title("Mario 16 (Asia) (En) (Pirate) [p1][!]");
        let alt = variant_of_title("Mario 16 (Asia) (En) (Pirate) [p1][a1]");
        assert_ne!(good.descriptor, alt.descriptor);
        assert!(good.flags.contains(&"verified".to_string()));
        assert!(alt.flags.contains(&"alternate".to_string()));
        assert!(!good.never_default && !alt.never_default);
        assert_eq!(
            rom_group_key("nes", "Mario 16 (Asia) (En) (Pirate) [p1][!]"),
            rom_group_key("nes", "Mario 16 (Asia) (En) (Pirate) [p1][a1]")
        );
    }

    #[test]
    fn variant_parse_bad_dump_badged_never_default() {
        let bad = variant_of_title("Some Game (USA) [b]");
        assert!(bad.flags.contains(&"bad-dump".to_string()));
        assert!(bad.never_default);
        let trained = variant_of_title("Some Game (USA) [t1]");
        assert!(trained.flags.contains(&"trainer".to_string()));
        assert!(trained.never_default);
    }

    #[test]
    fn variant_parse_multi_disc_one_group() {
        let d1 = variant_of_title("Big RPG (USA) (Disc 1)");
        let d2 = variant_of_title("Big RPG (USA) (Disc 2)");
        assert_eq!(d1.disc.as_deref(), Some("Disc 1"));
        assert_eq!(d2.disc.as_deref(), Some("Disc 2"));
        assert_ne!(d1.descriptor, d2.descriptor);
        assert_eq!(
            rom_group_key("psx", "Big RPG (USA) (Disc 1)"),
            rom_group_key("psx", "Big RPG (USA) (Disc 2)")
        );
    }

    #[test]
    fn variant_parse_regions_revisions_status() {
        let w = variant_of_title("3 Count Bout (World) (En,Ja)");
        assert_eq!(w.region.as_deref(), Some("World"));
        assert_eq!(w.region_rank, 1);
        assert_eq!(w.languages.as_deref(), Some("En,Ja"));
        let punch = variant_of_title("Mike Tyson's Punch-Out!! (Japan, USA) (En) (Rev 1)");
        assert_eq!(punch.region.as_deref(), Some("Japan, USA"));
        assert_eq!(punch.region_rank, 0, "best part ranks the group");
        assert_eq!(punch.revision_key, 1000);
        let beta = variant_of_title("Game (USA) (Beta)");
        assert_eq!(beta.status, "beta");
        assert_eq!(beta.status_rank, 1);
        let v13 = variant_of_title("King of Kings, The - The Early Years (USA) (v1.3)");
        assert_eq!(v13.revision_key, 1003);
        let patched = variant_of_title("Game (World) (CDZ patched)");
        assert!(patched.flags.contains(&"patched".to_string()));
        assert!(patched.never_default);
    }

    #[test]
    fn variant_parse_publisher_tags_distinguish() {
        let tengen = variant_of_title("Pac-Man (USA) (Tengen)");
        let namco = variant_of_title("Pac-Man (USA) (Namco)");
        assert_ne!(tengen.descriptor, namco.descriptor);
        assert!(tengen.descriptor.contains("Tengen"));
    }

    #[test]
    fn group_key_is_sequel_safe() {
        assert_ne!(
            rom_group_key("neogeo", "The King of Fighters '94 (Japan)"),
            rom_group_key("neogeo", "The King of Fighters '97 (Japan)")
        );
        assert_ne!(
            rom_group_key("nes", "Mega Man 2 (USA)"),
            rom_group_key("nes", "Mega Man 3 (USA)")
        );
        // Different systems never group.
        assert_ne!(
            rom_group_key("nes", "Klax (USA)"),
            rom_group_key("genesis", "Klax (USA)")
        );
    }
}

#[cfg(test)]
mod chd_tests {
    use super::*;

    /// Minimal valid CHD v4: one cooked MODE1 track of 4 frames,
    /// one stored (type 2) hunk holding the payload.
    fn build_v4_chd(track_payload: &[u8]) -> Vec<u8> {
        assert_eq!(track_payload.len(), 9792);
        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(b"MComprHD");
        out.extend_from_slice(&108u32.to_be_bytes());
        out.extend_from_slice(&4u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes()); // flags
        out.extend_from_slice(&0u32.to_be_bytes()); // compression: none
        out.extend_from_slice(&1u32.to_be_bytes()); // total hunks
        out.extend_from_slice(&9792u64.to_be_bytes()); // logical size
        let metaoffset_pos = out.len();
        out.extend_from_slice(&0u64.to_be_bytes()); // patched below
        out.extend_from_slice(&9792u32.to_be_bytes()); // hunkbytes
        out.extend_from_slice(&[0u8; 60]); // sha1 + parent + raw sha1
        let mut crc = crc32fast::Hasher::new();
        crc.update(track_payload);
        let crcv = crc.finalize();
        let map_pos = out.len();
        out.extend_from_slice(&0u64.to_be_bytes()); // offset (patched)
        out.extend_from_slice(&crcv.to_be_bytes());
        out.extend_from_slice(&9792u16.to_be_bytes());
        out.push(0);
        out.push(2); // stored
        let meta_off = out.len() as u64;
        out[metaoffset_pos..metaoffset_pos + 8].copy_from_slice(&meta_off.to_be_bytes());
        let text = b"TRACK:1 TYPE:MODE1 SUBTYPE:NONE FRAMES:4 PREGAP:0 PGTYPE:MODE1 PGSUB:NONE POSTGAP:0";
        out.extend_from_slice(b"CHT2");
        out.extend_from_slice(&(text.len() as u32).to_be_bytes());
        out.extend_from_slice(&0u64.to_be_bytes());
        out.extend_from_slice(text);
        let data_off = out.len() as u64;
        out[map_pos..map_pos + 8].copy_from_slice(&data_off.to_be_bytes());
        out.extend_from_slice(track_payload);
        out
    }

    /// One-system (ps2), one-row FABLEDAT1 dataset around (crc, size).
    fn chd_dataset(crc: u32, size: u64, name: &str) -> RomDataset {
        let mut sys_blob: Vec<u8> = Vec::new();
        sys_blob.push(3);
        sys_blob.extend_from_slice(b"ps2");
        sys_blob.push(13);
        sys_blob.extend_from_slice(b"PlayStation 2");
        let mut names: Vec<u8> = Vec::new();
        names.extend_from_slice(name.as_bytes());
        names.push(0);
        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(b"FABLEDAT");
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // systems
        out.extend_from_slice(&1u32.to_le_bytes()); // rows
        out.extend_from_slice(&0u32.to_le_bytes()); // md5s
        out.extend_from_slice(&0u32.to_le_bytes()); // sha1s
        out.extend_from_slice(&(names.len() as u32).to_le_bytes());
        out.extend_from_slice(&(sys_blob.len() as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // sets
        out.extend_from_slice(&0u32.to_le_bytes()); // set roms
        out.extend_from_slice(&0u32.to_le_bytes()); // shorts len
        out.extend_from_slice(&sys_blob);
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // name off
        out.extend_from_slice(&NONE32.to_le_bytes());
        out.extend_from_slice(&NONE32.to_le_bytes());
        out.push(0); // sys 0
        out.extend_from_slice(&[0u8; 3]);
        out.extend_from_slice(&names);
        RomDataset::load(out).expect("chd test dataset loads")
    }

    fn cooked_crc(payload: &[u8]) -> u32 {
        let mut h = crc32fast::Hasher::new();
        for f in 0..4 {
            h.update(&payload[f * 2448..f * 2448 + 2048]);
        }
        h.finalize()
    }

    #[test]
    fn identify_chd_cooked_ps2_span() {
        let mut payload = vec![0u8; 9792];
        for (i, b) in payload.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let crc = cooked_crc(&payload);
        let ds = chd_dataset(crc, 4 * 2048, "Test Game (USA)");
        let dir = std::env::temp_dir();
        let p = dir.join("fable-romdat-test-game.chd");
        std::fs::write(&p, build_v4_chd(&payload)).unwrap();
        let ids = identify_file_all(&ds, &p).expect("identify runs");
        std::fs::remove_file(&p).ok();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].sys, "ps2");
        assert_eq!(ids[0].dat_name, "Test Game (USA)");
        assert_eq!(ids[0].container, "chd");
        assert_eq!(ids[0].crc, crc);
    }

    #[test]
    fn identify_chd_wrong_crc_no_match() {
        let mut payload = vec![0u8; 9792];
        for (i, b) in payload.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let ds = chd_dataset(cooked_crc(&payload) ^ 1, 4 * 2048, "Test Game (USA)");
        let dir = std::env::temp_dir();
        let p = dir.join("fable-romdat-test-game2.chd");
        std::fs::write(&p, build_v4_chd(&payload)).unwrap();
        let ids = identify_file_all(&ds, &p).expect("identify runs");
        std::fs::remove_file(&p).ok();
        assert!(ids.is_empty());
    }

    #[test]
    fn identify_chd_corrupt_is_err_not_silent() {
        let ds = chd_dataset(0, 8192, "Test Game (USA)");
        let dir = std::env::temp_dir();
        let p = dir.join("fable-romdat-test-bad.chd");
        std::fs::write(&p, b"MComprHD-but-not-really-a-chd").unwrap();
        let r = identify_file_all(&ds, &p);
        std::fs::remove_file(&p).ok();
        assert!(r.is_err());
    }
}
