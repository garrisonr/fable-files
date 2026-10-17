//! Native CHD (Compressed Hunks of Data) reader for ROM identification.
//!
//! Original implementation of the CHD disc-image format (v3/v4/v5),
//! written from the format's structural documentation. A CHD stores a
//! disc as independently compressed "hunks" of CD frames (2448 bytes
//! per frame: 2352 bytes of sector data + 96 bytes of subcode), a
//! metadata chain describing the tracks, and a hunk map. This reader
//! parses the header, metadata, and map; decompresses hunks (codecs:
//! zlib/deflate, CD-zlib, CD-LZMA, CD-FLAC, stored, self-references);
//! regenerates the CD error-correction bytes the CD codecs strip; and
//! streams per-track spans into the DAT hashers in `romdat`.
//!
//! Honesty rules: anything malformed, truncated, encrypted-by-omission
//! (a delta/parent CHD without its parent), or compressed with a codec
//! outside the supported set produces a specific error — never a
//! guessed hash. Every decompressed v5 hunk is verified against the
//! map's CRC-16 and every v3/v4 hunk against its CRC-32 (when the map
//! carries one) before a single byte is hashed.

use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::Path;

pub const FRAME: u32 = 2448; // bytes per frame in the logical stream
pub const SECTOR: u32 = 2352; // sector-data bytes per frame
pub const SUBCODE: u32 = 96; // subcode bytes per frame

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum ChdError {
    Io(std::io::Error),
    BadMagic,
    UnsupportedVersion(u32),
    Truncated(&'static str),
    BadMapCrc,
    HunkCrcMismatch(u32),
    UnsupportedCodec(String),
    ParentRequired,
    Corrupt(String),
}

impl fmt::Display for ChdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChdError::Io(e) => write!(f, "chd: io error: {e}"),
            ChdError::BadMagic => write!(f, "chd: bad magic (not a CHD file)"),
            ChdError::UnsupportedVersion(v) => write!(f, "chd: unsupported version {v}"),
            ChdError::Truncated(what) => write!(f, "chd: truncated {what}"),
            ChdError::BadMapCrc => write!(f, "chd: hunk map CRC mismatch (corrupt map)"),
            ChdError::HunkCrcMismatch(i) => {
                write!(f, "chd: hunk {i} failed its CRC check (corrupt data)")
            }
            ChdError::UnsupportedCodec(c) => {
                write!(f, "chd: unsupported codec '{c}'")
            }
            ChdError::ParentRequired => {
                write!(f, "chd: delta image — requires its parent CHD, not identified")
            }
            ChdError::Corrupt(msg) => write!(f, "chd: corrupt: {msg}"),
        }
    }
}

impl std::error::Error for ChdError {}

impl From<std::io::Error> for ChdError {
    fn from(e: std::io::Error) -> Self {
        ChdError::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, ChdError>;

// ---------------------------------------------------------------------------
// CRC-16 (CCITT-FALSE: poly 0x1021, init 0xFFFF, no reflection) — the CHD
// map and per-hunk checksum, pinned against real discs during design.
// ---------------------------------------------------------------------------

const fn crc16_table() -> [u16; 256] {
    let mut table = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = (i as u16) << 8;
        let mut b = 0;
        while b < 8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
            b += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

static CRC16_TABLE: [u16; 256] = crc16_table();

pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc = (crc << 8) ^ CRC16_TABLE[(((crc >> 8) ^ b as u16) & 0xFF) as usize];
    }
    crc
}

// ---------------------------------------------------------------------------
// GF(2^8) Reed–Solomon parity for CD ECC regeneration. The disc ECC the
// CD codecs strip: 12-byte sync + P parity (172 bytes) + Q parity
// (104 bytes) are recomputed from the surviving bytes. Parameters
// (field 0x11D, generator alpha^1, no parity swap, header bytes of
// mode-2 sectors masked to zero) were pinned empirically against
// per-hunk CRCs of real discs during design.
// ---------------------------------------------------------------------------

const fn gf_tables() -> ([u8; 512], [u8; 256]) {
    let mut exp = [0u8; 512];
    let mut log = [0u8; 256];
    let mut x: u16 = 1;
    let mut i = 0;
    while i < 255 {
        exp[i] = x as u8;
        log[x as usize] = i as u8;
        x <<= 1;
        if x & 0x100 != 0 {
            x ^= 0x11D;
        }
        i += 1;
    }
    let mut j = 255;
    while j < 512 {
        exp[j] = exp[j - 255];
        j += 1;
    }
    (exp, log)
}

static GF: ([u8; 512], [u8; 256]) = gf_tables();

fn gf_mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        0
    } else {
        GF.0[(GF.1[a as usize] as usize + GF.1[b as usize] as usize) % 512]
    }
}

fn gf_div(a: u8, b: u8) -> u8 {
    if a == 0 {
        0
    } else {
        GF.0[(GF.1[a as usize] as i32 - GF.1[b as usize] as i32 + 255) as usize]
    }
}

/// Two parity bytes for `data` under the degree-2 generator whose roots
/// are 1 and alpha^root_b_exp.
fn rs_parity(data: &[u8], root_b_exp: usize) -> (u8, u8) {
    let n = data.len();
    let mut a1: u8 = 0; // sum of data bytes
    let mut a2: u8 = 0; // sum weighted by alpha^(root*(position from end))
    let rb = GF.0[root_b_exp % 255];
    for (i, &d) in data.iter().enumerate() {
        a1 ^= d;
        let pw = (root_b_exp * (n + 1 - i)) % 255;
        a2 ^= gf_mul(d, GF.0[pw]);
    }
    let p = gf_div(a1 ^ a2, rb ^ 1);
    let q = a1 ^ p;
    (p, q)
}

const SYNC: [u8; 12] = [0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0];

/// If a sector's sync + P + Q bytes were stripped (zeroed) by the CD
/// compressor, regenerate them in place; otherwise leave the sector
/// untouched. ECC offsets are relative to sector byte 12 (the covered
/// window is [12, 2352)); mode-2 header bytes count as zero.
pub fn restore_cd_sector(sector: &mut [u8]) {
    debug_assert_eq!(sector.len(), SECTOR as usize);
    let stripped = sector[0..12].iter().all(|&b| b == 0)
        && sector[2076..2248].iter().all(|&b| b == 0)
        && sector[2248..2352].iter().all(|&b| b == 0);
    if !stripped {
        return;
    }
    sector[0..12].copy_from_slice(&SYNC);
    let mode2 = sector[15] == 2;
    let src = |s: &[u8], off: usize| -> u8 {
        if mode2 && off < 4 {
            0
        } else {
            s[12 + off]
        }
    };
    // P parity: 86 columns over the 24x86 matrix of offsets [0, 2064).
    let mut col = [0u8; 24];
    for c in 0..86 {
        for r in 0..24 {
            col[r] = src(sector, r * 86 + c);
        }
        let (p, q) = rs_parity(&col, 1);
        sector[2076 + c] = p;
        sector[2076 + 86 + c] = q;
    }
    // Q parity: 52 diagonals over the 26x86 matrix (data rows plus the
    // P rows at relative [2064, 2236)); reads the freshly written P.
    let mut diag = [0u8; 43];
    for j in 0..52 {
        let (r0, c0) = (j / 2, j % 2);
        for t in 0..43 {
            let off = ((r0 + t) % 26) * 86 + ((c0 + 2 * t) % 86);
            diag[t] = src(sector, off);
        }
        let (p, q) = rs_parity(&diag, 1);
        sector[2248 + j] = p;
        sector[2248 + 52 + j] = q;
    }
}

// ---------------------------------------------------------------------------
// MSB-first bit reader + the CHD canonical Huffman scheme (v5 hunk map).
// Code assignment: per-length starting codes are computed walking from
// the LONGEST length downward (start = (prev + count) >> 1 at each
// step); symbols in increasing order then take successive codes of
// their length. This scheme was verified against real v5 maps.
// ---------------------------------------------------------------------------

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize, // bit position
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0 }
    }

    fn read(&mut self, nbits: usize) -> Result<u32> {
        let mut val: u32 = 0;
        for _ in 0..nbits {
            let byte = self
                .data
                .get(self.pos >> 3)
                .copied()
                .ok_or(ChdError::Truncated("huffman stream"))?;
            let bit = (byte >> (7 - (self.pos & 7))) & 1;
            val = (val << 1) | bit as u32;
            self.pos += 1;
        }
        Ok(val)
    }
}

struct CanonicalHuffman {
    codes: HashMap<(u8, u32), u16>,
    maxlen: u8,
}

impl CanonicalHuffman {
    fn new(lengths: &[u8]) -> CanonicalHuffman {
        let maxlen = lengths.iter().copied().max().unwrap_or(0);
        let mut histo = vec![0u32; maxlen as usize + 2];
        for &ln in lengths {
            if ln > 0 {
                histo[ln as usize] += 1;
            }
        }
        let mut start = vec![0u32; maxlen as usize + 2];
        let mut curstart: u32 = 0;
        for codelen in (1..=maxlen).rev() {
            let nextstart = (curstart + histo[codelen as usize]) >> 1;
            start[codelen as usize] = curstart;
            curstart = nextstart;
        }
        let mut codes = HashMap::new();
        let mut nxt = start.clone();
        for (sym, &ln) in lengths.iter().enumerate() {
            if ln > 0 {
                codes.insert((ln, nxt[ln as usize]), sym as u16);
                nxt[ln as usize] += 1;
            }
        }
        CanonicalHuffman { codes, maxlen }
    }

    fn decode_one(&self, br: &mut BitReader) -> Result<u16> {
        let mut code: u32 = 0;
        for ln in 1..=self.maxlen {
            code = (code << 1) | br.read(1)?;
            if let Some(&sym) = self.codes.get(&(ln, code)) {
                return Ok(sym);
            }
        }
        Err(ChdError::Corrupt("bad huffman code in hunk map".into()))
    }
}

/// Import an RLE-compressed Huffman tree (16 symbols, 4 bits per entry;
/// value 1 escapes: (1,1) means a literal length-1 entry, (1,x) a run
/// of x's whose count follows in 4 more bits, plus 3).
fn import_tree_rle(br: &mut BitReader) -> Result<CanonicalHuffman> {
    let mut lengths: Vec<u8> = Vec::with_capacity(16);
    while lengths.len() < 16 {
        let v = br.read(4)? as u8;
        if v != 1 {
            lengths.push(v);
        } else {
            let v2 = br.read(4)? as u8;
            if v2 == 1 {
                lengths.push(1);
            } else {
                let rep = br.read(4)? as usize + 3;
                if lengths.len() + rep > 16 {
                    return Err(ChdError::Corrupt("huffman tree overrun".into()));
                }
                lengths.extend(std::iter::repeat_n(v2, rep));
            }
        }
    }
    Ok(CanonicalHuffman::new(&lengths))
}

// ---------------------------------------------------------------------------
// Codec primitives. Each returns exactly `destlen` bytes or an error.
// ---------------------------------------------------------------------------

/// Raw-deflate (or zlib-wrapped) inflate producing exactly destlen
/// bytes; streams carrying a trailing pad byte are capped, as the
/// reference decoder stops at the expected length.
fn inflate_exact(raw: &[u8], destlen: usize) -> Result<Vec<u8>> {
    use flate2::read::{DeflateDecoder, ZlibDecoder};
    for wrapped in [false, true] {
        let mut out = Vec::with_capacity(destlen);
        let res = if wrapped {
            ZlibDecoder::new(Cursor::new(raw))
                .take(destlen as u64 + 1)
                .read_to_end(&mut out)
        } else {
            DeflateDecoder::new(Cursor::new(raw))
                .take(destlen as u64 + 1)
                .read_to_end(&mut out)
        };
        if res.is_ok() && out.len() == destlen {
            return Ok(out);
        }
    }
    Err(ChdError::Corrupt("deflate decode failed".into()))
}

/// Raw LZMA1 with the CD codec's fixed properties (lc=3, lp=0, pb=2).
fn lzma_exact(raw: &[u8], destlen: usize) -> Result<Vec<u8>> {
    let mut reader = lzma_rust::LZMAReader::new(
        Cursor::new(raw.to_vec()),
        destlen as u64,
        3,
        0,
        2,
        1 << 26,
        None::<&[u8]>,
    )
    .map_err(|e| ChdError::Corrupt(format!("lzma init: {e}")))?;
    let mut out = Vec::with_capacity(destlen);
    reader
        .take(destlen as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|e| ChdError::Corrupt(format!("lzma decode: {e}")))?;
    if out.len() != destlen {
        return Err(ChdError::Corrupt(format!(
            "lzma short: {} != {destlen}",
            out.len()
        )));
    }
    Ok(out)
}

/// The CD codec template: an ECC-flag bitmap, a big-endian base-stream
/// length, the base stream (all frames' 2352 data bytes), then the
/// subcode stream (all frames' 96 subcode bytes). `base_codec` selects
/// the base decoder ("zlib" or "lzma"); the subcode stream is always
/// deflate. Frames whose bitmap bit is set had their ECC stripped —
/// sync + P/Q parity are regenerated after assembly.
fn decompress_cd_template(base_codec: &str, raw: &[u8], hunkbytes: u32) -> Result<Vec<u8>> {
    let frames = (hunkbytes / FRAME) as usize;
    let ecc_bytes = frames.div_ceil(8);
    let complen_bytes = if hunkbytes < 65536 { 2 } else { 3 };
    let header = ecc_bytes + complen_bytes;
    if raw.len() < header {
        return Err(ChdError::Truncated("cd codec header"));
    }
    let mut base_len = 0usize;
    for i in 0..complen_bytes {
        base_len = (base_len << 8) | raw[ecc_bytes + i] as usize;
    }
    let base_end = header
        .checked_add(base_len)
        .filter(|&e| e <= raw.len())
        .ok_or(ChdError::Corrupt("cd codec base overruns hunk".into()))?;
    let base_raw = &raw[header..base_end];
    let sub_raw = &raw[base_end..];
    let base = match base_codec {
        "zlib" => inflate_exact(base_raw, frames * SECTOR as usize)?,
        "lzma" => lzma_exact(base_raw, frames * SECTOR as usize)?,
        _ => return Err(ChdError::UnsupportedCodec(base_codec.into())),
    };
    let sub = inflate_exact(sub_raw, frames * SUBCODE as usize)?;
    let mut out = vec![0u8; hunkbytes as usize];
    for i in 0..frames {
        let f = i * FRAME as usize;
        out[f..f + SECTOR as usize]
            .copy_from_slice(&base[i * SECTOR as usize..(i + 1) * SECTOR as usize]);
        out[f + SECTOR as usize..f + FRAME as usize]
            .copy_from_slice(&sub[i * SUBCODE as usize..(i + 1) * SUBCODE as usize]);
        if raw[i >> 3] & (1 << (i & 7)) != 0 {
            let (sector, _) = out.split_at_mut(f + SECTOR as usize);
            restore_cd_sector(&mut sector[f..]);
        }
    }
    Ok(out)
}

/// CD-FLAC: the hunk holds one or more standard FLAC frames (the
/// sector bytes encoded as 44.1 kHz stereo 16-bit samples, each sample
/// byte-swapped) followed by a raw-deflate subcode stream. Stock FLAC
/// decoders need a stream header the CHD does not store, so one is
/// synthesized; decoding stops at the exact sample count — the map CRC
/// then arbitrates the result.
fn decompress_cd_flac(raw: &[u8], hunkbytes: u32) -> Result<Vec<u8>> {
    let frames = (hunkbytes / FRAME) as usize;
    let target = frames * SECTOR as usize;
    let mut stream = Vec::with_capacity(raw.len() + 42);
    stream.extend_from_slice(b"fLaC");
    stream.extend_from_slice(&[0x80, 0, 0, 34]); // last block, STREAMINFO, 34 bytes
    let mut si = [0u8; 34];
    si[0..2].copy_from_slice(&16u16.to_be_bytes());
    si[2..4].copy_from_slice(&65535u16.to_be_bytes());
    let packed: u64 = (44100u64 << 44) | (1u64 << 41) | (15u64 << 36);
    si[10..18].copy_from_slice(&packed.to_be_bytes());
    stream.extend_from_slice(&si);
    stream.extend_from_slice(raw);
    let mut reader = claxon::FlacReader::new(Cursor::new(stream))
        .map_err(|e| ChdError::Corrupt(format!("flac open: {e}")))?;
    let mut blocks = reader.blocks();
    let mut buf: Vec<i32> = Vec::new();
    let mut base: Vec<u8> = Vec::with_capacity(target);
    while base.len() < target {
        match blocks.read_next_or_eof(buf) {
            Ok(Some(block)) => {
                if block.channels() != 2 {
                    return Err(ChdError::Corrupt("flac stream is not stereo".into()));
                }
                for (l, r) in block.stereo_samples() {
                    base.extend_from_slice(&(l as i16).to_be_bytes());
                    base.extend_from_slice(&(r as i16).to_be_bytes());
                }
                buf = block.into_buffer();
            }
            Ok(None) => break,
            Err(e) => return Err(ChdError::Corrupt(format!("flac decode: {e}"))),
        }
    }
    if base.len() != target {
        return Err(ChdError::Corrupt(format!(
            "flac decoded {} bytes, want {target}",
            base.len()
        )));
    }
    // The subcode stream is the deflate stream that ends exactly at the
    // end of the hunk and yields exactly frames*96 bytes; scan from the
    // end (it is short) for its start.
    let sub_len = frames * SUBCODE as usize;
    let mut sub: Option<Vec<u8>> = None;
    for off in (0..raw.len()).rev() {
        if raw.len() - off > sub_len * 4 + 64 {
            break; // a deflate stream for sub_len bytes cannot be longer
        }
        if let Ok(s) = inflate_exact(&raw[off..], sub_len) {
            sub = Some(s);
            break;
        }
    }
    let sub = sub.ok_or(ChdError::Corrupt("flac subcode stream not found".into()))?;
    let mut out = vec![0u8; hunkbytes as usize];
    for i in 0..frames {
        let f = i * FRAME as usize;
        out[f..f + SECTOR as usize]
            .copy_from_slice(&base[i * SECTOR as usize..(i + 1) * SECTOR as usize]);
        out[f + SECTOR as usize..f + FRAME as usize]
            .copy_from_slice(&sub[i * SUBCODE as usize..(i + 1) * SUBCODE as usize]);
    }
    Ok(out)
}

/// Decompress one stored hunk by codec tag (v5 map slots).
fn decompress(codec: &str, raw: &[u8], hunkbytes: u32) -> Result<Vec<u8>> {
    match codec {
        "cdzl" => decompress_cd_template("zlib", raw, hunkbytes),
        "cdlz" => decompress_cd_template("lzma", raw, hunkbytes),
        "cdfl" => decompress_cd_flac(raw, hunkbytes),
        "zlib" => inflate_exact(raw, hunkbytes as usize),
        "lzma" => lzma_exact(raw, hunkbytes as usize),
        "none" => {
            if raw.len() == hunkbytes as usize {
                Ok(raw.to_vec())
            } else {
                Err(ChdError::Corrupt("stored hunk has wrong length".into()))
            }
        }
        other => Err(ChdError::UnsupportedCodec(other.to_string())),
    }
}

// ---------------------------------------------------------------------------
// Tracks
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChdTrack {
    /// Track number from the metadata (1-based as authored).
    pub number: u32,
    /// CHD track type string ("MODE1_RAW", "MODE1", "MODE2_RAW",
    /// "AUDIO", ...).
    pub kind: String,
    /// Frames stored for this track (pregap frames included as
    /// authored — the span model hashes the track exactly as stored).
    pub frames: u32,
    pub pregap: u32,
}

impl ChdTrack {
    pub fn is_data(&self) -> bool {
        self.kind != "AUDIO"
    }
    /// Cooked MODE1 tracks (authored from 2048-byte sectors) store the
    /// user data at the START of each frame's data area; raw track
    /// types store whole sectors.
    pub fn is_cooked(&self) -> bool {
        self.kind == "MODE1"
    }
}

fn parse_track_text(text: &str) -> Option<ChdTrack> {
    let mut number = 0u32;
    let mut kind = String::new();
    let mut frames = 0u32;
    let mut pregap = 0u32;
    for part in text.split_whitespace() {
        let Some((k, v)) = part.split_once(':') else {
            continue;
        };
        match k {
            "TRACK" => number = v.parse().unwrap_or(0),
            "TYPE" => kind = v.to_string(),
            "FRAMES" => frames = v.parse().unwrap_or(0),
            "PREGAP" => pregap = v.parse().unwrap_or(0),
            _ => {}
        }
    }
    if kind.is_empty() || frames == 0 {
        return None;
    }
    Some(ChdTrack {
        number,
        kind,
        frames,
        pregap,
    })
}

// ---------------------------------------------------------------------------
// Hunk map entries
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MapEntry {
    /// v5: map symbol (0..3 codec slot, 4 stored, 5 self, 6 parent).
    /// v3/v4: entry type (0 invalid, 1 compressed, 2 stored, 3 mini,
    /// 4 self, 5 parent, 6 second-codec).
    kind: u8,
    offset: u64,
    length: u32,
    /// v5: CRC-16 of the decompressed hunk. v3/v4: CRC-32.
    crc: u32,
    no_crc: bool,
}

// ---------------------------------------------------------------------------
// The CHD file
// ---------------------------------------------------------------------------

pub struct ChdFile {
    file: File,
    version: u32,
    codecs: [String; 4],
    v34_compression: u32,
    logical_size: u64,
    hunkbytes: u32,
    unitbytes: u32,
    has_parent: bool,
    raw_sha1: [u8; 20],
    tracks: Vec<ChdTrack>,
    map: Vec<MapEntry>,
    cache: Option<(u32, Vec<u8>)>,
}

fn be_u32(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn be_u64(b: &[u8], off: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[off..off + 8]);
    u64::from_be_bytes(a)
}

impl ChdFile {
    pub fn open(path: &Path) -> Result<ChdFile> {
        let mut file = File::open(path)?;
        let mut head = [0u8; 124];
        let n = read_up_to(&mut file, &mut head)?;
        if n < 8 || &head[0..8] != b"MComprHD" {
            return Err(ChdError::BadMagic);
        }
        if n < 108 {
            return Err(ChdError::Truncated("header"));
        }
        let header_len = be_u32(&head, 8) as u64;
        let version = be_u32(&head, 12);

        let mut chd = ChdFile {
            file,
            version,
            codecs: Default::default(),
            v34_compression: 0,
            logical_size: 0,
            hunkbytes: 0,
            unitbytes: FRAME,
            has_parent: false,
            raw_sha1: [0; 20],
            tracks: Vec::new(),
            map: Vec::new(),
            cache: None,
        };

        let (metaoffset, mapoffset): (u64, u64);
        match version {
            5 => {
                for i in 0..4 {
                    let tag = &head[16 + 4 * i..20 + 4 * i];
                    chd.codecs[i] = String::from_utf8_lossy(tag)
                        .trim_matches('\0')
                        .trim()
                        .to_string();
                }
                chd.logical_size = be_u64(&head, 32);
                let mo = be_u64(&head, 40);
                metaoffset = be_u64(&head, 48);
                chd.hunkbytes = be_u32(&head, 56);
                chd.unitbytes = be_u32(&head, 60);
                chd.raw_sha1.copy_from_slice(&head[64..84]);
                chd.has_parent = head[104..124].iter().any(|&b| b != 0);
                mapoffset = mo;
            }
            3 | 4 => {
                chd.v34_compression = be_u32(&head, 20);
                chd.logical_size = be_u64(&head, 28);
                metaoffset = be_u64(&head, 36);
                chd.hunkbytes = be_u32(&head, 44);
                chd.raw_sha1.copy_from_slice(&head[88..108]);
                chd.has_parent = head[68..88].iter().any(|&b| b != 0);
                chd.unitbytes = FRAME;
                mapoffset = header_len; // the map follows the header
            }
            v => return Err(ChdError::UnsupportedVersion(v)),
        }
        if chd.hunkbytes == 0 || chd.logical_size == 0 {
            return Err(ChdError::Corrupt("zero hunk size or length".into()));
        }
        let total = chd.logical_size.div_ceil(chd.hunkbytes as u64) as usize;
        if total == 0 || total > (1 << 28) {
            return Err(ChdError::Corrupt("implausible hunk count".into()));
        }

        chd.read_metadata(metaoffset)?;
        match version {
            5 => chd.read_map_v5(mapoffset, total)?,
            _ => chd.read_map_v34(mapoffset, total)?,
        }
        Ok(chd)
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn logical_size(&self) -> u64 {
        self.logical_size
    }

    pub fn hunk_bytes(&self) -> u32 {
        self.hunkbytes
    }

    pub fn has_parent(&self) -> bool {
        self.has_parent
    }

    /// SHA-1 of the full decompressed logical stream, as recorded in
    /// the header at authoring time.
    pub fn raw_sha1(&self) -> [u8; 20] {
        self.raw_sha1
    }

    pub fn tracks(&self) -> &[ChdTrack] {
        &self.tracks
    }

    fn read_metadata(&mut self, mut off: u64) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        while off != 0 && seen.insert(off) {
            self.file.seek(SeekFrom::Start(off))?;
            let mut hdr = [0u8; 16];
            if read_up_to(&mut self.file, &mut hdr)? < 16 {
                break;
            }
            let tag = String::from_utf8_lossy(&hdr[0..4]).into_owned();
            let dlen = (be_u32(&hdr, 4) & 0x00FF_FFFF) as usize;
            let next = be_u64(&hdr, 8);
            if dlen > 1_000_000 {
                return Err(ChdError::Corrupt("metadata entry implausibly large".into()));
            }
            let mut data = vec![0u8; dlen];
            read_up_to(&mut self.file, &mut data)?;
            if tag == "CHT2" || tag == "CHGD" {
                let text = String::from_utf8_lossy(&data);
                let text = text.trim_matches('\0');
                if let Some(tr) = parse_track_text(text) {
                    self.tracks.push(tr);
                }
            }
            off = next;
        }
        Ok(())
    }

    fn read_map_v34(&mut self, mapoffset: u64, total: usize) -> Result<()> {
        self.file.seek(SeekFrom::Start(mapoffset))?;
        let mut raw = vec![0u8; total * 16];
        read_exact_or_trunc(&mut self.file, &mut raw, "v34 hunk map")?;
        let mut map = Vec::with_capacity(total);
        for i in 0..total {
            let e = &raw[i * 16..i * 16 + 16];
            let flags = e[15];
            map.push(MapEntry {
                kind: flags & 0x0F,
                offset: be_u64(e, 0),
                crc: be_u32(e, 8),
                // The v3/v4 length is a u24 stored low-16-bits
                // big-endian first, then the high byte.
                length: (u16::from_be_bytes([e[12], e[13]]) as u32) | ((e[14] as u32) << 16),
                no_crc: flags & 0x10 != 0,
            });
        }
        self.map = map;
        Ok(())
    }

    fn read_map_v5(&mut self, mapoffset: u64, total: usize) -> Result<()> {
        self.file.seek(SeekFrom::Start(mapoffset))?;
        let mut mh = [0u8; 16];
        read_exact_or_trunc(&mut self.file, &mut mh, "v5 map header")?;
        let mapbytes = be_u32(&mh, 0) as usize;
        let firstoffs = {
            let mut a = [0u8; 8];
            a[2..8].copy_from_slice(&mh[4..10]);
            u64::from_be_bytes(a)
        };
        let mapcrc = u16::from_be_bytes([mh[10], mh[11]]);
        let (lengthbits, selfbits, parentbits) = (mh[12] as usize, mh[13] as usize, mh[14] as usize);
        if mapbytes > 64 * 1024 * 1024 {
            return Err(ChdError::Corrupt("v5 map implausibly large".into()));
        }
        let mut stream = vec![0u8; mapbytes];
        read_exact_or_trunc(&mut self.file, &mut stream, "v5 hunk map")?;
        let mut br = BitReader::new(&stream);
        let huff = import_tree_rle(&mut br)?;

        // Pass 1: compression types, with the map's RLE symbols.
        const RLE_SMALL: u16 = 7;
        const RLE_LARGE: u16 = 8;
        let mut types: Vec<u8> = Vec::with_capacity(total);
        let mut lastcomp: u8 = 0;
        let mut repcount: usize = 0;
        while types.len() < total {
            if repcount > 0 {
                types.push(lastcomp);
                repcount -= 1;
                continue;
            }
            let sym = huff.decode_one(&mut br)?;
            match sym {
                RLE_SMALL => {
                    types.push(lastcomp);
                    repcount = 2 + huff.decode_one(&mut br)? as usize;
                }
                RLE_LARGE => {
                    types.push(lastcomp);
                    repcount = 2 + 16 + ((huff.decode_one(&mut br)? as usize) << 4)
                        + huff.decode_one(&mut br)? as usize;
                }
                s if s <= 6 || (9..=13).contains(&s) => {
                    lastcomp = s as u8;
                    types.push(lastcomp);
                }
                s => return Err(ChdError::Corrupt(format!("unknown map symbol {s}"))),
            }
        }

        // Pass 2: per-hunk details from the bit fields.
        let units_per_hunk = (self.hunkbytes / self.unitbytes.max(1)) as u64;
        let mut map: Vec<MapEntry> = Vec::with_capacity(total);
        let mut curoffset = firstoffs;
        let mut last_self: u64 = 0;
        let mut last_parent: u64 = 0;
        for (hnum, &t) in types.iter().enumerate() {
            match t {
                0..=3 => {
                    let length = br.read(lengthbits)? as u32;
                    let crc = br.read(16)? as u32;
                    map.push(MapEntry {
                        kind: t,
                        offset: curoffset,
                        length,
                        crc,
                        no_crc: false,
                    });
                    curoffset += length as u64;
                }
                4 => {
                    let crc = br.read(16)? as u32;
                    map.push(MapEntry {
                        kind: 4,
                        offset: curoffset,
                        length: self.hunkbytes,
                        crc,
                        no_crc: false,
                    });
                    curoffset += self.hunkbytes as u64;
                }
                5 => {
                    last_self = br.read(selfbits)? as u64;
                    map.push(MapEntry {
                        kind: 5,
                        offset: last_self,
                        length: 0,
                        crc: 0,
                        no_crc: true,
                    });
                }
                6 => {
                    last_parent = br.read(parentbits)? as u64;
                    map.push(MapEntry {
                        kind: 6,
                        offset: last_parent,
                        length: 0,
                        crc: 0,
                        no_crc: true,
                    });
                }
                9 => map.push(MapEntry {
                    kind: 5,
                    offset: last_self,
                    length: 0,
                    crc: 0,
                    no_crc: true,
                }),
                10 => {
                    last_self += 1;
                    map.push(MapEntry {
                        kind: 5,
                        offset: last_self,
                        length: 0,
                        crc: 0,
                        no_crc: true,
                    });
                }
                11 => {
                    last_parent = hnum as u64 * units_per_hunk;
                    map.push(MapEntry {
                        kind: 6,
                        offset: last_parent,
                        length: 0,
                        crc: 0,
                        no_crc: true,
                    });
                }
                12 => map.push(MapEntry {
                    kind: 6,
                    offset: last_parent,
                    length: 0,
                    crc: 0,
                    no_crc: true,
                }),
                13 => {
                    last_parent += units_per_hunk;
                    map.push(MapEntry {
                        kind: 6,
                        offset: last_parent,
                        length: 0,
                        crc: 0,
                        no_crc: true,
                    });
                }
                _ => unreachable!(),
            }
        }

        // The expanded map (12 bytes per entry) must match the stored
        // CRC-16 before any entry is trusted.
        let mut expanded = Vec::with_capacity(total * 12);
        for e in &map {
            expanded.push(e.kind);
            expanded.extend_from_slice(&e.length.to_be_bytes()[1..4]);
            expanded.extend_from_slice(&e.offset.to_be_bytes()[2..8]);
            expanded.extend_from_slice(&(e.crc as u16).to_be_bytes());
        }
        if crc16(&expanded) != mapcrc {
            return Err(ChdError::BadMapCrc);
        }
        self.map = map;
        Ok(())
    }

    /// Read and decompress one hunk (hunkbytes long). Sequential
    /// readers (the span hasher) hit a one-entry cache.
    pub fn read_hunk(&mut self, idx: u32) -> Result<Vec<u8>> {
        if let Some((ci, data)) = &self.cache {
            if *ci == idx {
                return Ok(data.clone());
            }
        }
        let data = self.read_hunk_inner(idx, 0)?;
        self.cache = Some((idx, data.clone()));
        Ok(data)
    }

    fn read_hunk_inner(&mut self, idx: u32, depth: u32) -> Result<Vec<u8>> {
        if depth > 8 {
            return Err(ChdError::Corrupt("self-reference loop in hunk map".into()));
        }
        let entry = *self
            .map
            .get(idx as usize)
            .ok_or(ChdError::Corrupt("hunk index out of range".into()))?;
        let out = match self.version {
            5 => self.read_hunk_v5(idx, &entry, depth)?,
            _ => self.read_hunk_v34(idx, &entry, depth)?,
        };
        // A short final hunk is padded with zeros up to hunk size;
        // tracks never address frames past the logical end.
        if out.len() < self.hunkbytes as usize {
            if idx as usize + 1 == self.map.len() {
                let mut padded = out;
                padded.resize(self.hunkbytes as usize, 0);
                return Ok(padded);
            }
            return Err(ChdError::Corrupt("short hunk mid-file".into()));
        }
        Ok(out)
    }

    fn stored_bytes(&mut self, entry: &MapEntry) -> Result<Vec<u8>> {
        self.file.seek(SeekFrom::Start(entry.offset))?;
        let mut raw = vec![0u8; entry.length as usize];
        read_exact_or_trunc(&mut self.file, &mut raw, "hunk data")?;
        Ok(raw)
    }

    fn read_hunk_v5(&mut self, idx: u32, entry: &MapEntry, depth: u32) -> Result<Vec<u8>> {
        let out = match entry.kind {
            5 => return self.read_hunk_inner(entry.offset as u32, depth + 1),
            6 => return Err(ChdError::ParentRequired),
            4 => self.stored_bytes(entry)?,
            0..=3 => {
                let codec = self.codecs[entry.kind as usize].clone();
                let raw = self.stored_bytes(entry)?;
                decompress(&codec, &raw, self.hunkbytes)?
            }
            k => return Err(ChdError::Corrupt(format!("bad v5 hunk kind {k}"))),
        };
        if crc16(&out) != entry.crc as u16 {
            return Err(ChdError::HunkCrcMismatch(idx));
        }
        Ok(out)
    }

    fn read_hunk_v34(&mut self, idx: u32, entry: &MapEntry, depth: u32) -> Result<Vec<u8>> {
        let out = match entry.kind {
            4 => return self.read_hunk_inner(entry.offset as u32, depth + 1),
            5 => return Err(ChdError::ParentRequired),
            3 => {
                // Mini: the offset field IS the data — an 8-byte
                // big-endian pattern repeated across the hunk.
                let pat = entry.offset.to_be_bytes();
                let mut out = vec![0u8; self.hunkbytes as usize];
                for chunk in out.chunks_mut(8) {
                    let n = chunk.len();
                    chunk.copy_from_slice(&pat[..n]);
                }
                out
            }
            2 => self.stored_bytes(entry)?,
            1 => {
                let raw = self.stored_bytes(entry)?;
                match self.v34_compression {
                    0 => raw,
                    1 | 2 => inflate_exact(&raw, self.hunkbytes as usize)?,
                    3 => return Err(ChdError::UnsupportedCodec("avhuff".into())),
                    c => {
                        return Err(ChdError::UnsupportedCodec(format!(
                            "v34 compression {c}"
                        )))
                    }
                }
            }
            6 => {
                let raw = self.stored_bytes(entry)?;
                decompress_cd_flac(&raw, self.hunkbytes)?
            }
            k => return Err(ChdError::Corrupt(format!("bad v34 hunk kind {k}"))),
        };
        if !entry.no_crc {
            let mut h = crc32fast::Hasher::new();
            h.update(&out);
            if h.finalize() != entry.crc {
                return Err(ChdError::HunkCrcMismatch(idx));
            }
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Track span hashing (the identification surface used by romdat)
// ---------------------------------------------------------------------------

/// Which bytes of each frame a span hash covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanKind {
    /// The frame's full 2352-byte sector data (raw CD dumps: the
    /// Redump per-track model).
    Raw2352,
    /// The first 2048 bytes of the frame's data area (cooked MODE1
    /// tracks: the ISO/DVD user-data model).
    User2048,
}

impl ChdFile {
    /// Stream one track's span through CRC32 + MD5 + SHA1. The span is
    /// the track's frames exactly as stored (each track in the logical
    /// stream is padded up to a 4-frame boundary). Returns
    /// (crc, md5, sha1, bytes hashed).
    pub fn hash_track_span(
        &mut self,
        track_index: usize,
        kind: SpanKind,
    ) -> Result<(u32, [u8; 16], [u8; 20], u64)> {
        if self.hunkbytes % FRAME != 0 {
            return Err(ChdError::Corrupt(
                "hunk size is not a whole number of CD frames".into(),
            ));
        }
        let frames_per_hunk = (self.hunkbytes / FRAME) as u64;
        // Locate the track's first frame in the logical stream.
        let mut start: u64 = 0;
        let mut track: Option<&ChdTrack> = None;
        for (i, tr) in self.tracks.iter().enumerate() {
            if i == track_index {
                track = Some(tr);
                break;
            }
            let fr = tr.frames as u64;
            start += fr + (4 - fr % 4) % 4;
        }
        let track = track.ok_or(ChdError::Corrupt("track index out of range".into()))?;
        let frames = track.frames as u64;
        let seg_len = match kind {
            SpanKind::Raw2352 => SECTOR as usize,
            SpanKind::User2048 => 2048,
        };
        use sha1::Digest as _;
        let mut crc = crc32fast::Hasher::new();
        let mut md5h = md5::Context::new();
        let mut sha1h = sha1::Sha1::new();
        let mut hashed: u64 = 0;
        let first_hunk = start / frames_per_hunk;
        let last_hunk = (start + frames - 1) / frames_per_hunk;
        for h in first_hunk..=last_hunk {
            let hunk = self.read_hunk(h as u32)?;
            let hunk_first_frame = h * frames_per_hunk;
            for fi in 0..frames_per_hunk {
                let gf = hunk_first_frame + fi;
                if gf < start || gf >= start + frames {
                    continue;
                }
                let f = (fi * FRAME as u64) as usize;
                let seg = &hunk[f..f + seg_len];
                crc.update(seg);
                md5h.consume(seg);
                sha1h.update(seg);
                hashed += seg_len as u64;
            }
        }
        let md5_out: [u8; 16] = md5h.compute().0;
        let sha1_out: [u8; 20] = sha1h.finalize().into();
        Ok((crc.finalize(), md5_out, sha1_out, hashed))
    }

    /// SHA-1 over the entire decompressed logical stream (the
    /// header's recorded raw SHA-1 is the oracle). Used by proofs and
    /// diagnostics; identification hashes only gated track spans.
    pub fn stream_sha1(&mut self) -> Result<([u8; 20], u64)> {
        use sha1::Digest as _;
        let mut sha1h = sha1::Sha1::new();
        let total = self.map.len() as u32;
        let mut bytes: u64 = 0;
        for i in 0..total {
            let hunk = self.read_hunk(i)?;
            let take = if i + 1 == total {
                let rem = self.logical_size % self.hunkbytes as u64;
                if rem == 0 {
                    self.hunkbytes as usize
                } else {
                    rem as usize
                }
            } else {
                hunk.len()
            };
            sha1h.update(&hunk[..take]);
            bytes += take as u64;
        }
        Ok((sha1h.finalize().into(), bytes))
    }
}

// ---------------------------------------------------------------------------
// IO helpers
// ---------------------------------------------------------------------------

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

fn read_exact_or_trunc(r: &mut File, buf: &mut [u8], what: &'static str) -> Result<()> {
    if read_up_to(r, buf)? < buf.len() {
        return Err(ChdError::Truncated(what));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests. Codec and map fixtures are real bytes captured from production
// discs during design validation (tiny slices: individual hunks and
// one hunk map); every fixture is verified by the format's own CRCs.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const IKARUGA_HUNK0: &[u8] = include_bytes!("../tests/fixtures/chd/ikaruga_hunk0.bin");
    const TACTICS_HUNK2294: &[u8] = include_bytes!("../tests/fixtures/chd/tactics_hunk2294.bin");
    const CDFL_HUNK1523: &[u8] = include_bytes!("../tests/fixtures/chd/cdfl_hunk1523.bin");
    const TACTICS_MAP: &[u8] = include_bytes!("../tests/fixtures/chd/tactics-map.bin");

    #[test]
    fn crc16_check_vector() {
        // CRC-16/CCITT-FALSE standard check value.
        assert_eq!(crc16(b"123456789"), 0x29B1);
        assert_eq!(crc16(b""), 0xFFFF);
    }

    #[test]
    fn ecc_regenerate_is_self_consistent() {
        // Make a sector with valid parity, strip sync+P+Q, regenerate:
        // the result must equal the unstripped sector byte for byte.
        let mut sector: Vec<u8> = (0..SECTOR as usize).map(|i| (i * 37 % 251) as u8).collect();
        sector[15] = 1; // mode byte (header bytes are ECC inputs)
        sector[0..12].fill(0);
        sector[2076..2352].fill(0);
        restore_cd_sector(&mut sector); // computes genuine sync + P + Q
        assert_eq!(&sector[0..12], &SYNC[..]);
        let mut stripped = sector.clone();
        stripped[0..12].fill(0);
        stripped[2076..2352].fill(0);
        restore_cd_sector(&mut stripped);
        assert_eq!(stripped, sector);
        // An unstripped sector is left untouched.
        let mut intact = sector.clone();
        restore_cd_sector(&mut intact);
        assert_eq!(intact, sector);
    }

    #[test]
    fn v34_whole_hunk_deflate_fixture() {
        // Ikaruga (Dreamcast, CHD v4) hunk 0: a single raw-deflate
        // stream over the whole 9792-byte hunk; CRC-32 0x5e10f209.
        let out = inflate_exact(IKARUGA_HUNK0, 9792).expect("inflate");
        let mut h = crc32fast::Hasher::new();
        h.update(&out);
        assert_eq!(h.finalize(), 0x5e10f209);
    }

    #[test]
    fn cdlz_template_fixture() {
        // Tactics Ogre (PSX, v5) hunk 2294: CD-LZMA template hunk;
        // its map CRC-16 is 0xf7e5.
        let out = decompress("cdlz", TACTICS_HUNK2294, 19584).expect("cdlz decode");
        assert_eq!(crc16(&out), 0xf7e5);
    }

    #[test]
    fn cdzl_and_cdfl_fixtures() {
        // CD-FLAC hunk 1523: map CRC-16 0x6541.
        let out = decompress("cdfl", CDFL_HUNK1523, 19584).expect("cdfl decode");
        assert_eq!(crc16(&out), 0x6541);
    }

    #[test]
    fn unsupported_codec_is_honest() {
        let err = decompress("zstd", b"whatever", 19584).unwrap_err();
        assert!(matches!(err, ChdError::UnsupportedCodec(_)), "{err}");
    }

    #[test]
    fn v5_map_fixture_decodes_and_verifies() {
        // The full compressed hunk map of Tactics Ogre (4284 hunks):
        // decode it through the same code path open() uses.
        let dir = std::env::temp_dir();
        let p = dir.join("fable-chd-test-map.bin");
        std::fs::write(&p, TACTICS_MAP).unwrap();
        let mut chd = ChdFile {
            file: File::open(&p).unwrap(),
            version: 5,
            codecs: Default::default(),
            v34_compression: 0,
            logical_size: 0,
            hunkbytes: 19584,
            unitbytes: 2448,
            has_parent: false,
            raw_sha1: [0; 20],
            tracks: Vec::new(),
            map: Vec::new(),
            cache: None,
        };
        chd.read_map_v5(0, 4284).expect("map decodes + CRC verifies");
        assert_eq!(chd.map.len(), 4284);
        let kinds: std::collections::HashMap<u8, usize> =
            chd.map.iter().fold(Default::default(), |mut m, e| {
                *m.entry(e.kind).or_default() += 1;
                m
            });
        // Census from design validation: cdlz 1475 (slot 0), cdzl 31
        // (slot 1), cdfl 2367 (slot 2), self refs 411 (kind 5).
        assert_eq!(kinds.get(&0), Some(&1475));
        assert_eq!(kinds.get(&1), Some(&31));
        assert_eq!(kinds.get(&2), Some(&2367));
        assert_eq!(kinds.get(&5), Some(&411));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn v5_map_corruption_is_rejected() {
        let mut bad = TACTICS_MAP.to_vec();
        bad[100] ^= 0xFF;
        let dir = std::env::temp_dir();
        let p = dir.join("fable-chd-test-map-bad.bin");
        std::fs::write(&p, &bad).unwrap();
        let mut chd = ChdFile {
            file: File::open(&p).unwrap(),
            version: 5,
            codecs: Default::default(),
            v34_compression: 0,
            logical_size: 0,
            hunkbytes: 19584,
            unitbytes: 2448,
            has_parent: false,
            raw_sha1: [0; 20],
            tracks: Vec::new(),
            map: Vec::new(),
            cache: None,
        };
        let r = chd.read_map_v5(0, 4284);
        assert!(r.is_err(), "corrupt map must not decode");
        std::fs::remove_file(&p).ok();
    }

    /// Build a minimal valid CHD v4 in memory: one cooked MODE1
    /// track of 4 frames, one stored (type 2) hunk.
    fn build_v4_chd(track_payload: &[u8]) -> Vec<u8> {
        assert_eq!(track_payload.len(), 9792);
        let mut out: Vec<u8> = Vec::new();
        // Header (108 bytes).
        out.extend_from_slice(b"MComprHD");
        out.extend_from_slice(&108u32.to_be_bytes()); // header length
        out.extend_from_slice(&4u32.to_be_bytes()); // version
        out.extend_from_slice(&0u32.to_be_bytes()); // flags
        out.extend_from_slice(&0u32.to_be_bytes()); // compression: none
        out.extend_from_slice(&1u32.to_be_bytes()); // total hunks
        out.extend_from_slice(&9792u64.to_be_bytes()); // logical size
        let metaoffset_pos = out.len();
        out.extend_from_slice(&0u64.to_be_bytes()); // metaoffset (patched)
        out.extend_from_slice(&9792u32.to_be_bytes()); // hunkbytes
        out.extend_from_slice(&[0u8; 20]); // sha1
        out.extend_from_slice(&[0u8; 20]); // parent sha1 (none)
        out.extend_from_slice(&[0u8; 20]); // raw sha1
        assert_eq!(out.len(), 108);
        // Map (1 entry x 16 bytes): stored hunk; its offset field is
        // patched once the layout is known.
        let mut crc = crc32fast::Hasher::new();
        crc.update(track_payload);
        let crcv = crc.finalize();
        let map_pos = out.len();
        out.extend_from_slice(&0u64.to_be_bytes()); // offset (patched)
        out.extend_from_slice(&crcv.to_be_bytes());
        out.extend_from_slice(&9792u16.to_be_bytes()); // length low 16
        out.push(0); // length high byte
        out.push(2); // flags: type 2 = stored
        // Metadata: one CHT2 entry, no successor.
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

    #[test]
    fn v4_end_to_end_open_and_span() {
        // Track payload: 4 frames; cooked span = first 2048 of each.
        let mut payload = vec![0u8; 9792];
        for (i, b) in payload.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let bytes = build_v4_chd(&payload);
        let dir = std::env::temp_dir();
        let p = dir.join("fable-chd-test-v4.chd");
        std::fs::write(&p, &bytes).unwrap();
        let mut chd = ChdFile::open(&p).expect("v4 chd opens");
        assert_eq!(chd.version(), 4);
        assert_eq!(chd.tracks().len(), 1);
        assert!(chd.tracks()[0].is_data());
        assert!(chd.tracks()[0].is_cooked());
        let (crc, _md5, _sha1, n) = chd
            .hash_track_span(0, SpanKind::User2048)
            .expect("span hashes");
        assert_eq!(n, 4 * 2048);
        let mut want = crc32fast::Hasher::new();
        for f in 0..4 {
            want.update(&payload[f * 2448..f * 2448 + 2048]);
        }
        assert_eq!(crc, want.finalize());
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn bad_magic_and_truncation_are_honest() {
        let dir = std::env::temp_dir();
        let p = dir.join("fable-chd-test-bad.chd");
        std::fs::write(&p, b"NOTACHD!").unwrap();
        assert!(matches!(ChdFile::open(&p), Err(ChdError::BadMagic)));
        std::fs::write(&p, b"MComprHD").unwrap();
        assert!(matches!(ChdFile::open(&p), Err(ChdError::Truncated(_))));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn track_text_parsing() {
        let t = parse_track_text(
            "TRACK:3 TYPE:MODE2_RAW SUBTYPE:NONE FRAMES:15907 PREGAP:150 PGTYPE:MODE2 PGSUB:NONE POSTGAP:0",
        )
        .expect("track parses");
        assert_eq!(t.number, 3);
        assert_eq!(t.kind, "MODE2_RAW");
        assert_eq!(t.frames, 15907);
        assert!(t.is_data());
        assert!(!t.is_cooked());
        let a = parse_track_text("TRACK:2 TYPE:AUDIO SUBTYPE:NONE FRAMES:6328 PREGAP:150").unwrap();
        assert!(!a.is_data());
    }
}
