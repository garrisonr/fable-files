//! ROM identification against the bundled DAT dataset (rom-dats.bin,
//! format FABLEDAT1, compiled by tools/datc from No-Intro / Redump /
//! MAME DATs as redistributed by libretro-database — see ROM-DATS.md).
//!
//! A file is a member of a built-in ROM collection ONLY when its bytes
//! hash-match a DAT row (or, for MAME-style zip sets, when the zip's
//! filename is a known set whose contained ROM CRCs are all present).
//! Extensions merely nominate candidates; the DAT decides.
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
        "bin" => &["genesis", "segacd", "saturn", "psx", "tg16", "dreamcast"],
        "ccd" => &[],
        "chd" => &[],
        "cia" | "3ds" => &["n3ds"],
        "ciso" => &["gamecube"],
        "col" | "cv" => &["colecovision"],
        "crt" => &["c64"],
        "cso" => &["psp"],
        "cue" => &[],
        "d64" | "prg" | "t64" => &["c64"],
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
        "img" => &["psx", "saturn", "segacd"],
        "int" => &["intellivision"],
        "iso" => &[
            "psx", "ps2", "saturn", "segacd", "dreamcast", "psp", "gamecube", "wii", "xbox",
            "xbox360", "ps3",
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
        "sms" => &["mastersystem"],
        "sna" | "szx" | "tzx" | "z80" => &["zxspectrum"],
        "tap" => &["c64", "zxspectrum"],
        "vb" | "vboy" => &["virtualboy"],
        "vec" => &["vectrex"],
        "ws" | "wsc" => &["wonderswan"],
        "xbe" | "xiso" => &["xbox"],
        _ => &[],
    }
}

/// Every extension that can nominate a file for identification
/// (the union of `systems_for_extension` keys, plus "zip").
pub fn candidate_extensions() -> Vec<&'static str> {
    vec![
        "32x", "3ds", "a26", "a52", "a78", "adf", "atr", "atx", "bin", "cdi", "cia", "ciso",
        "col", "crt", "cso", "cv", "d64", "dms", "dsk", "fig", "gb", "gba", "gbc", "gc",
        "gcm", "gdi", "gen", "gg", "img", "int", "iso", "j64", "jag", "lha", "lnx", "md",
        "mdf", "mx1", "mx2", "n64", "nds", "neo", "nes", "ngc", "ngp", "pbp", "pce", "pkg",
        "prg", "rom", "rpx", "sfc", "sgb", "smc", "smd", "sms", "sna", "swc", "szx", "t64",
        "tap", "tzx", "unh", "v64", "vb", "vboy", "vec", "wad", "wbfs", "ws", "wsc", "wua",
        "wux", "xbe", "xex", "xfd", "xiso", "z64", "z80", "zip",
    ]
}

/// All dataset system keys that accept loose files (for zip containers:
/// a zip can hold any cartridge system's ROM).
fn cartridge_systems() -> &'static [&'static str] {
    &[
        "atari2600", "atari5200", "atari7800", "atari8bit", "lynx", "colecovision", "c64",
        "gb", "gba", "gbc", "gamegear", "genesis", "intellivision", "jaguar", "mastersystem",
        "msx", "neogeopocket", "nes", "n64", "nds", "n3ds", "sega32x", "snes", "tg16",
        "vectrex", "virtualboy", "wonderswan", "zxspectrum", "amiga",
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
    let mut skip = match t {
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
        skip = 0;
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
            });
        }
    }
    None
}

/// Identify one file. Err = the file could not be read at all;
/// Ok(None) = read fine, no DAT match (not a known ROM).
pub fn identify_file(ds: &RomDataset, path: &Path) -> Result<Option<Identification>, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Ok(None);
    }
    let file_size = meta.len();
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    if ext == "zip" {
        return identify_zip(ds, path, file_size);
    }

    let sys_keys = systems_for_extension(&ext);
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
    }
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
    // Contained-ROM matching: any entry whose stored CRC+size hits a
    // DAT row (entry extensions nominate their own systems; an entry
    // without a known extension is tried against all cartridge systems).
    let cart: Vec<u8> = cartridge_systems()
        .iter()
        .filter_map(|k| ds.sys_index(k))
        .collect();
    for entry in &entries {
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
            // is expected. (Headerless-content matching inside zips
            // would need decompression; not attempted.)
            let pref: Vec<&str> = systems_for_extension(&eext).to_vec();
            return Ok(Some(resolve_hits(
                ds,
                hits,
                &pref,
                entry.crc,
                [0; 16],
                [0; 20],
                "zip",
                file_size,
            )));
        }
    }
    Ok(None)
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
}
