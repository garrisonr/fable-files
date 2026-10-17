//! Native CSO (CISO) reader for ROM identification.
//!
//! Original implementation of the PSP CSO compressed-ISO format,
//! written from the format's structural documentation. A CSO stores
//! an ISO image as independently compressed blocks:
//!
//!   header (24 bytes, little-endian): magic "CISO", header_size,
//!   total_bytes (the uncompressed ISO size), block_size, version,
//!   align, reserved
//!   (num_blocks + 1) × u32 index entries: the block's file offset
//!   is (entry & 0x7FFF_FFFF) << align; bit 31 set = stored raw
//!   the blocks themselves (raw DEFLATE streams, one per block)
//!
//! Field notes pinned against the real collection during design:
//! writers commonly store header_size as 0 — the index always
//! follows the fixed 24-byte header, so the index offset is
//! max(header_size, 24). Version 1 files carry raw-deflate blocks;
//! version 2 (maxcso) may substitute LZ4 blocks, which this reader
//! refuses honestly rather than guessing at.
//!
//! Honesty rules: bad magic, inconsistent geometry, a non-monotonic
//! index, or a block that does not inflate to its expected size all
//! produce a specific error — never a guessed hash. The format
//! carries no internal checksums; the DAT lookup over the
//! decompressed stream is the arbiter, as for a plain ISO.

use std::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub const CSO_MAGIC: &[u8; 4] = b"CISO";
const RAW_FLAG: u32 = 0x8000_0000;
const OFFSET_MASK: u32 = 0x7FFF_FFFF;
/// Cap on read amplification while inflating one block.
const MAX_BLOCK: u64 = 1 << 21;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum CsoError {
    Io(std::io::Error),
    BadMagic,
    UnsupportedVersion(u8),
    Truncated(&'static str),
    Corrupt(String),
    Inflate(u32, String),
    ShortBlock(u32, usize, usize),
}

impl fmt::Display for CsoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CsoError::Io(e) => write!(f, "cso: io error: {e}"),
            CsoError::BadMagic => write!(f, "cso: bad magic (not a CSO file)"),
            CsoError::UnsupportedVersion(v) => {
                write!(f, "cso: unsupported version {v} (may carry LZ4 blocks)")
            }
            CsoError::Truncated(what) => write!(f, "cso: truncated {what}"),
            CsoError::Corrupt(msg) => write!(f, "cso: corrupt: {msg}"),
            CsoError::Inflate(i, msg) => write!(f, "cso: block {i} does not inflate: {msg}"),
            CsoError::ShortBlock(i, got, want) => {
                write!(f, "cso: block {i} inflated to {got} bytes, expected {want}")
            }
        }
    }
}

impl std::error::Error for CsoError {}

impl From<std::io::Error> for CsoError {
    fn from(e: std::io::Error) -> Self {
        CsoError::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, CsoError>;

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

pub struct CsoFile {
    file: File,
    total_bytes: u64,
    block_size: u32,
    align: u8,
    index: Vec<u32>,
    stored: Vec<u8>,
}

impl CsoFile {
    pub fn open(path: &Path) -> Result<CsoFile> {
        let mut file = File::open(path)?;
        let file_len = file.metadata()?.len();
        let mut hdr = [0u8; 24];
        let n = read_up_to(&mut file, &mut hdr)?;
        if n < 24 {
            return Err(CsoError::Truncated("header"));
        }
        if &hdr[0..4] != CSO_MAGIC {
            return Err(CsoError::BadMagic);
        }
        let header_size = u32::from_le_bytes(hdr[4..8].try_into().unwrap()) as u64;
        let total_bytes = u64::from_le_bytes(hdr[8..16].try_into().unwrap());
        let block_size = u32::from_le_bytes(hdr[16..20].try_into().unwrap());
        let version = hdr[20];
        let align = hdr[21];
        if version > 1 {
            return Err(CsoError::UnsupportedVersion(version));
        }
        if block_size < 512 || !block_size.is_power_of_two() || block_size as u64 > MAX_BLOCK {
            return Err(CsoError::Corrupt(format!(
                "implausible block size {block_size}"
            )));
        }
        if total_bytes == 0 {
            return Err(CsoError::Corrupt("empty image".into()));
        }
        if align > 16 {
            return Err(CsoError::Corrupt(format!("implausible align {align}")));
        }
        let num_blocks = total_bytes.div_ceil(block_size as u64);
        // The index follows the fixed 24-byte header even when the
        // stored header_size says 0 (the common writer quirk).
        let index_off = header_size.max(24);
        let index_bytes = (num_blocks + 1) * 4;
        if index_off + index_bytes > file_len {
            return Err(CsoError::Truncated("index"));
        }
        file.seek(SeekFrom::Start(index_off))?;
        let mut idx_raw = vec![0u8; index_bytes as usize];
        file.read_exact(&mut idx_raw)?;
        let mut index = Vec::with_capacity((num_blocks + 1) as usize);
        for i in 0..=num_blocks as usize {
            index.push(u32::from_le_bytes(
                idx_raw[i * 4..i * 4 + 4].try_into().unwrap(),
            ));
        }
        // Index validation: masked offsets are non-decreasing, the
        // data starts at or after the index end, and every span ends
        // inside the file.
        let data_start = ((index[0] & OFFSET_MASK) as u64) << align;
        if data_start < index_off + index_bytes {
            return Err(CsoError::Corrupt(format!(
                "first block offset {data_start} overlaps the index (ends {})",
                index_off + index_bytes
            )));
        }
        for i in 0..num_blocks as usize {
            let start = (index[i] & OFFSET_MASK) as u64;
            let end = (index[i + 1] & OFFSET_MASK) as u64;
            if end < start {
                return Err(CsoError::Corrupt(format!(
                    "index decreases at block {i} ({start} -> {end})"
                )));
            }
            if (end << align) > file_len {
                return Err(CsoError::Truncated("block data"));
            }
            let span = (end - start) << align;
            if index[i] & RAW_FLAG != 0 {
                let want = block_want(total_bytes, block_size, i as u64, num_blocks);
                if span != block_size as u64 && span != want {
                    return Err(CsoError::Corrupt(format!(
                        "stored block {i} spans {span} bytes, expected {want} or {block_size}"
                    )));
                }
            }
        }
        Ok(CsoFile {
            file,
            total_bytes,
            block_size,
            align,
            index,
            stored: Vec::new(),
        })
    }

    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    pub fn block_size(&self) -> u32 {
        self.block_size
    }

    pub fn num_blocks(&self) -> u32 {
        (self.index.len() - 1) as u32
    }

    fn block_span(&self, i: usize) -> (u64, u64, bool) {
        let start = ((self.index[i] & OFFSET_MASK) as u64) << self.align;
        let end = ((self.index[i + 1] & OFFSET_MASK) as u64) << self.align;
        (start, end - start, self.index[i] & RAW_FLAG != 0)
    }

    /// Decompress block `i` into `out` (exactly the block's bytes).
    pub fn read_block_into(&mut self, i: u32, out: &mut Vec<u8>) -> Result<()> {
        let idx = i as usize;
        if idx + 1 >= self.index.len() {
            return Err(CsoError::Corrupt(format!("block {i} out of range")));
        }
        let (start, span, raw) = self.block_span(idx);
        self.stored.resize(span as usize, 0);
        self.file.seek(SeekFrom::Start(start))?;
        self.file.read_exact(&mut self.stored)?;
        let want = block_want(
            self.total_bytes,
            self.block_size,
            i as u64,
            (self.index.len() - 1) as u64,
        ) as usize;
        if raw {
            out.clear();
            out.extend_from_slice(&self.stored);
            if out.len() != want && out.len() != self.block_size as usize {
                return Err(CsoError::ShortBlock(i, out.len(), want));
            }
            return Ok(());
        }
        out.clear();
        {
            use std::io::Read as _;
            let mut dec = flate2::read::DeflateDecoder::new(&self.stored[..]);
            let mut buf = [0u8; 8192];
            loop {
                let n = dec
                    .read(&mut buf)
                    .map_err(|e| CsoError::Inflate(i, e.to_string()))?;
                if n == 0 {
                    break;
                }
                out.extend_from_slice(&buf[..n]);
                if out.len() > self.block_size as usize {
                    break;
                }
            }
        }
        if out.len() != want && out.len() != self.block_size as usize {
            return Err(CsoError::ShortBlock(i, out.len(), want));
        }
        Ok(())
    }

    /// Stream the whole decompressed image through CRC32 + MD5 +
    /// SHA1 (exactly total_bytes). Returns (crc, md5, sha1, bytes).
    pub fn hash_image(&mut self) -> Result<(u32, [u8; 16], [u8; 20], u64)> {
        use sha1::Digest as _;
        let mut crc = crc32fast::Hasher::new();
        let mut md5h = md5::Context::new();
        let mut sha1h = sha1::Sha1::new();
        let mut hashed: u64 = 0;
        let mut block_buf: Vec<u8> = Vec::new();
        for i in 0..self.num_blocks() {
            self.read_block_into(i, &mut block_buf)?;
            let remaining = self.total_bytes - hashed;
            let take = (block_buf.len() as u64).min(remaining) as usize;
            let seg = &block_buf[..take];
            crc.update(seg);
            md5h.consume(seg);
            sha1h.update(seg);
            hashed += take as u64;
        }
        if hashed != self.total_bytes {
            return Err(CsoError::Corrupt(format!(
                "stream produced {hashed} bytes, header promises {}",
                self.total_bytes
            )));
        }
        let md5_out: [u8; 16] = md5h.compute().0;
        let sha1_out: [u8; 20] = sha1h.finalize().into();
        Ok((crc.finalize(), md5_out, sha1_out, hashed))
    }
}

/// The final block's true length (the remainder); every other block
/// is a full block_size.
fn block_want(total: u64, block_size: u32, i: u64, num_blocks: u64) -> u64 {
    if i + 1 == num_blocks {
        total - (num_blocks - 1) * block_size as u64
    } else {
        block_size as u64
    }
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
    use std::io::Write as _;

    fn deflate_raw(data: &[u8]) -> Vec<u8> {
        let mut enc =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    /// Build a CSO around `iso`; `header_size_field` is written
    /// verbatim (0 reproduces the common writer quirk). Blocks in
    /// `raw_blocks` are stored uncompressed.
    fn build_cso(iso: &[u8], block_size: u32, header_size_field: u32, raw_blocks: &[usize]) -> Vec<u8> {
        let num_blocks = (iso.len() as u64).div_ceil(block_size as u64) as usize;
        let index_off = 24u64;
        let data_start = index_off + ((num_blocks + 1) * 4) as u64;
        let mut index: Vec<u32> = Vec::new();
        let mut data: Vec<u8> = Vec::new();
        for i in 0..num_blocks {
            let s = i * block_size as usize;
            let e = ((i + 1) * block_size as usize).min(iso.len());
            let block = &iso[s..e];
            let off = (data_start + data.len() as u64) as u32;
            if raw_blocks.contains(&i) {
                index.push(off | RAW_FLAG);
                data.extend_from_slice(block);
            } else {
                index.push(off);
                data.extend_from_slice(&deflate_raw(block));
            }
        }
        index.push((data_start + data.len() as u64) as u32);
        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(CSO_MAGIC);
        out.extend_from_slice(&header_size_field.to_le_bytes());
        out.extend_from_slice(&(iso.len() as u64).to_le_bytes());
        out.extend_from_slice(&block_size.to_le_bytes());
        out.push(1); // version
        out.push(0); // align
        out.extend_from_slice(&[0u8; 2]);
        for e in &index {
            out.extend_from_slice(&e.to_le_bytes());
        }
        out.extend_from_slice(&data);
        out
    }

    fn sample_iso() -> Vec<u8> {
        // 20.5 blocks of 2048 — big enough to hold the ISO9660
        // signature at 0x8001, with a short final block.
        let mut iso = vec![0u8; 41984];
        for (i, b) in iso.iter_mut().enumerate() {
            *b = ((i * 17 + 3) % 251) as u8;
        }
        iso[0x8001..0x8006].copy_from_slice(b"CD001");
        iso
    }

    fn write_tmp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn roundtrip_with_zero_header_size_quirk() {
        let iso = sample_iso();
        for field in [0u32, 24] {
            let bytes = build_cso(&iso, 2048, field, &[1]);
            let p = write_tmp("fable-cso-roundtrip.cso", &bytes);
            let mut c = CsoFile::open(&p).expect("opens");
            assert_eq!(c.total_bytes(), iso.len() as u64);
            assert_eq!(c.num_blocks(), 21);
            let (crc, md5, sha1, n) = c.hash_image().expect("hashes");
            assert_eq!(n, iso.len() as u64);
            let mut h = crc32fast::Hasher::new();
            h.update(&iso);
            assert_eq!(crc, h.finalize());
            assert_eq!(md5, md5::compute(&iso).0);
            use sha1::Digest as _;
            let mut s1 = sha1::Sha1::new();
            s1.update(&iso);
            assert_eq!(sha1, <[u8; 20]>::from(s1.finalize()));
            std::fs::remove_file(&p).ok();
        }
    }

    #[test]
    fn bad_magic_truncation_and_version_are_errors() {
        let p = write_tmp("fable-cso-bad.cso", b"NOPE-not-a-cso-file-padpadpad12");
        assert!(matches!(CsoFile::open(&p), Err(CsoError::BadMagic)));
        std::fs::remove_file(&p).ok();
        let p = write_tmp("fable-cso-trunc.cso", b"CISO\x18\0");
        assert!(matches!(CsoFile::open(&p), Err(CsoError::Truncated(_))));
        std::fs::remove_file(&p).ok();
        let mut bytes = build_cso(&sample_iso(), 2048, 0, &[]);
        bytes[20] = 2; // version 2: may carry LZ4 — refused honestly
        let p = write_tmp("fable-cso-v2.cso", &bytes);
        assert!(matches!(
            CsoFile::open(&p),
            Err(CsoError::UnsupportedVersion(2))
        ));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn truncated_file_is_an_error_not_a_guess() {
        // CSO carries no internal checksums, so a truncated file is
        // caught structurally: the index promises spans the file
        // does not contain.
        let iso = sample_iso();
        let bytes = build_cso(&iso, 2048, 0, &[]);
        let cut = bytes.len() * 3 / 5;
        let p = write_tmp("fable-cso-trunc2.cso", &bytes[..cut]);
        assert!(matches!(CsoFile::open(&p), Err(CsoError::Truncated(_))));
        std::fs::remove_file(&p).ok();
        // Garbage where a deflate stream should be: inflate either
        // errors or under-produces — both are errors, never a hash.
        let mut bytes = build_cso(&iso, 2048, 0, &[]);
        let idx0 = u32::from_le_bytes(bytes[24..28].try_into().unwrap()) as usize;
        let idx1 = u32::from_le_bytes(bytes[28..32].try_into().unwrap()) as usize;
        for b in &mut bytes[idx0..idx1] {
            *b = 0xFF;
        }
        let p = write_tmp("fable-cso-garbage.cso", &bytes);
        let mut c = CsoFile::open(&p).expect("opens");
        assert!(c.hash_image().is_err());
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn decreasing_index_is_rejected_at_open() {
        let iso = sample_iso();
        let mut bytes = build_cso(&iso, 2048, 0, &[]);
        // Second index entry (offset 28) below the first.
        bytes[28..32].copy_from_slice(&0u32.to_le_bytes());
        let p = write_tmp("fable-cso-index.cso", &bytes);
        assert!(matches!(CsoFile::open(&p), Err(CsoError::Corrupt(_))));
        std::fs::remove_file(&p).ok();
    }
}
