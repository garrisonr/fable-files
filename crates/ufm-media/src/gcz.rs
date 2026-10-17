//! Native GCZ reader for ROM identification.
//!
//! Original implementation of Dolphin's GCZ block-compressed disc
//! format, written from the format's structural documentation. A GCZ
//! stores a disc image as independently compressed blocks:
//!
//!   header (32 bytes, little-endian): magic 0xB10BC001, sub_type,
//!   compressed_data_size, disc_size, block_size, num_blocks
//!   num_blocks × u64 block pointers (offset into the compressed
//!   data; bit 63 set = the block is stored raw, not deflated)
//!   num_blocks × u32 Adler-32 of each block's STORED bytes
//!   the blocks themselves (zlib streams, one per block)
//!
//! The format's own per-block Adler-32 table is the honesty gate:
//! every block's stored bytes are verified against it before a
//! single byte is inflated or hashed. The decompressed stream is the
//! disc image itself (for GameCube, 1,459,978,240 bytes), whose first
//! bytes are the disc header: game ID at 0x00, the GameCube magic
//! 0xC2339F3D at 0x1C, the title at 0x20.
//!
//! Honesty rules: bad magic, inconsistent header geometry, invalid
//! block pointers, an Adler mismatch, or a block that does not
//! inflate to its expected size all produce a specific error —
//! never a guessed hash.

use std::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub const GCZ_MAGIC: u32 = 0xB10B_C001;
pub const GC_MAGIC: u32 = 0xC233_9F3D; // at disc offset 0x1C, big-endian
const RAW_FLAG: u64 = 1 << 63;
/// Dolphin sizes its inflate buffer block_size + 64; a compressed
/// span larger than that cannot be a real block.
const COMPRESSED_SLACK: u64 = 64;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum GczError {
    Io(std::io::Error),
    BadMagic,
    Truncated(&'static str),
    Corrupt(String),
    AdlerMismatch(u32),
    Inflate(u32, String),
    ShortBlock(u32, usize, usize),
}

impl fmt::Display for GczError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GczError::Io(e) => write!(f, "gcz: io error: {e}"),
            GczError::BadMagic => write!(f, "gcz: bad magic (not a GCZ file)"),
            GczError::Truncated(what) => write!(f, "gcz: truncated {what}"),
            GczError::Corrupt(msg) => write!(f, "gcz: corrupt: {msg}"),
            GczError::AdlerMismatch(i) => {
                write!(f, "gcz: block {i} failed its Adler-32 check (corrupt data)")
            }
            GczError::Inflate(i, msg) => write!(f, "gcz: block {i} does not inflate: {msg}"),
            GczError::ShortBlock(i, got, want) => {
                write!(f, "gcz: block {i} inflated to {got} bytes, expected {want}")
            }
        }
    }
}

impl std::error::Error for GczError {}

impl From<std::io::Error> for GczError {
    fn from(e: std::io::Error) -> Self {
        GczError::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, GczError>;

// ---------------------------------------------------------------------------
// Adler-32 (the format's per-block checksum, over the stored bytes)
// ---------------------------------------------------------------------------

pub fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    // 5552 is the largest chunk for which the sums cannot overflow u32.
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    (b << 16) | a
}

// ---------------------------------------------------------------------------
// GameCube disc header (the first bytes of the decompressed stream)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcDiscHeader {
    /// Six-byte game ID, e.g. "GHNE71".
    pub game_id: String,
    /// Title at 0x20, up to the first NUL.
    pub title: String,
    /// True when the GameCube magic sits at 0x1C — the platform gate.
    pub is_gamecube: bool,
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

pub struct GczFile {
    file: File,
    sub_type: u32,
    compressed_size: u64,
    disc_size: u64,
    block_size: u32,
    pointers: Vec<u64>,
    hashes: Vec<u32>,
    data_offset: u64,
    stored: Vec<u8>,
    out: Vec<u8>,
}

impl GczFile {
    pub fn open(path: &Path) -> Result<GczFile> {
        let mut file = File::open(path)?;
        let file_len = file.metadata()?.len();
        let mut hdr = [0u8; 32];
        let n = read_up_to(&mut file, &mut hdr)?;
        if n < 32 {
            return Err(GczError::Truncated("header"));
        }
        let magic = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
        if magic != GCZ_MAGIC {
            return Err(GczError::BadMagic);
        }
        let sub_type = u32::from_le_bytes(hdr[4..8].try_into().unwrap());
        let compressed_size = u64::from_le_bytes(hdr[8..16].try_into().unwrap());
        let disc_size = u64::from_le_bytes(hdr[16..24].try_into().unwrap());
        let block_size = u32::from_le_bytes(hdr[24..28].try_into().unwrap());
        let num_blocks = u32::from_le_bytes(hdr[28..32].try_into().unwrap()) as u64;
        if block_size < 512 || !block_size.is_power_of_two() {
            return Err(GczError::Corrupt(format!(
                "implausible block size {block_size}"
            )));
        }
        if disc_size == 0 || num_blocks == 0 {
            return Err(GczError::Corrupt("empty disc image".into()));
        }
        if num_blocks != disc_size.div_ceil(block_size as u64) {
            return Err(GczError::Corrupt(format!(
                "num_blocks {num_blocks} inconsistent with disc size {disc_size} / block size {block_size}"
            )));
        }
        let data_offset = 32 + num_blocks * 12;
        if data_offset > file_len || data_offset + compressed_size > file_len {
            return Err(GczError::Truncated("block data"));
        }
        let mut ptr_bytes = vec![0u8; (num_blocks * 8) as usize];
        file.read_exact(&mut ptr_bytes)?;
        let mut pointers = Vec::with_capacity(num_blocks as usize);
        for i in 0..num_blocks as usize {
            pointers.push(u64::from_le_bytes(
                ptr_bytes[i * 8..i * 8 + 8].try_into().unwrap(),
            ));
        }
        let mut hash_bytes = vec![0u8; (num_blocks * 4) as usize];
        file.read_exact(&mut hash_bytes)?;
        let mut hashes = Vec::with_capacity(num_blocks as usize);
        for i in 0..num_blocks as usize {
            hashes.push(u32::from_le_bytes(
                hash_bytes[i * 4..i * 4 + 4].try_into().unwrap(),
            ));
        }
        // Pointer validation, Dolphin's rules: spans must stay inside
        // the compressed data, stored-raw spans are exactly one block,
        // and a compressed span can never exceed the inflate buffer.
        for i in 0..num_blocks as usize {
            let start = pointers[i] & !RAW_FLAG;
            let end = if i + 1 < pointers.len() {
                pointers[i + 1] & !RAW_FLAG
            } else {
                compressed_size
            };
            if start > compressed_size || end > compressed_size || end < start {
                return Err(GczError::Corrupt(format!(
                    "block {i} pointer span [{start}, {end}) outside compressed data ({compressed_size})"
                )));
            }
            let span = end - start;
            if pointers[i] & RAW_FLAG != 0 {
                if span != block_size as u64 {
                    return Err(GczError::Corrupt(format!(
                        "stored block {i} spans {span} bytes, expected {block_size}"
                    )));
                }
            } else if span > block_size as u64 + COMPRESSED_SLACK {
                return Err(GczError::Corrupt(format!(
                    "compressed block {i} spans {span} bytes, over the {block_size}-byte block size"
                )));
            }
        }
        Ok(GczFile {
            file,
            sub_type,
            compressed_size,
            disc_size,
            block_size,
            pointers,
            hashes,
            data_offset,
            stored: Vec::new(),
            out: Vec::new(),
        })
    }

    pub fn sub_type(&self) -> u32 {
        self.sub_type
    }

    pub fn disc_size(&self) -> u64 {
        self.disc_size
    }

    pub fn block_size(&self) -> u32 {
        self.block_size
    }

    pub fn num_blocks(&self) -> u32 {
        self.pointers.len() as u32
    }

    pub fn compressed_size(&self) -> u64 {
        self.compressed_size
    }

    fn block_span(&self, i: usize) -> (u64, u64, bool) {
        let start = self.pointers[i] & !RAW_FLAG;
        let end = if i + 1 < self.pointers.len() {
            self.pointers[i + 1] & !RAW_FLAG
        } else {
            self.compressed_size
        };
        (start, end - start, self.pointers[i] & RAW_FLAG != 0)
    }

    /// Bytes the block must produce: a full block, except the final
    /// block of a disc whose size is not a block multiple (there the
    /// remainder; a writer may also have padded it to a full block —
    /// read_block accepts either and the stream truncates at
    /// disc_size).
    fn block_want(&self, i: usize) -> usize {
        if i + 1 == self.pointers.len() {
            let rem = self.disc_size - (self.pointers.len() as u64 - 1) * self.block_size as u64;
            rem as usize
        } else {
            self.block_size as usize
        }
    }

    /// Decompress block `i` into `out` (exactly the block's bytes,
    /// padding included when the writer padded the final block).
    /// The stored bytes must pass their Adler-32 first.
    pub fn read_block_into(&mut self, i: u32, out: &mut Vec<u8>) -> Result<()> {
        let idx = i as usize;
        if idx >= self.pointers.len() {
            return Err(GczError::Corrupt(format!("block {i} out of range")));
        }
        let (start, span, raw) = self.block_span(idx);
        self.stored.resize(span as usize, 0);
        self.file.seek(SeekFrom::Start(self.data_offset + start))?;
        self.file.read_exact(&mut self.stored)?;
        if adler32(&self.stored) != self.hashes[idx] {
            return Err(GczError::AdlerMismatch(i));
        }
        let bs = self.block_size as usize;
        if raw {
            out.clear();
            out.extend_from_slice(&self.stored);
            return Ok(());
        }
        out.clear();
        {
            use std::io::Read as _;
            let mut dec = flate2::read::ZlibDecoder::new(&self.stored[..]);
            let mut buf = [0u8; 8192];
            loop {
                let n = dec
                    .read(&mut buf)
                    .map_err(|e| GczError::Inflate(i, e.to_string()))?;
                if n == 0 {
                    break;
                }
                out.extend_from_slice(&buf[..n]);
                if out.len() > bs {
                    break;
                }
            }
        }
        let want = self.block_want(idx);
        if out.len() != bs && out.len() != want {
            return Err(GczError::ShortBlock(i, out.len(), want));
        }
        Ok(())
    }

    /// Read `buf.len()` bytes of the decompressed stream at `offset`.
    pub fn read_stream(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let bs = self.block_size as u64;
        let mut done = 0usize;
        while done < buf.len() {
            let pos = offset + done as u64;
            if pos >= self.disc_size {
                return Err(GczError::Truncated("stream read past disc size"));
            }
            let block = (pos / bs) as u32;
            let within = (pos % bs) as usize;
            // Take from the scratch block buffer.
            let mut block_buf = std::mem::take(&mut self.out);
            self.read_block_into(block, &mut block_buf)?;
            let avail = block_buf.len() - within;
            let take = avail.min(buf.len() - done);
            buf[done..done + take].copy_from_slice(&block_buf[within..within + take]);
            self.out = block_buf;
            done += take;
        }
        Ok(())
    }

    /// Parse the disc header at the start of the decompressed stream.
    pub fn disc_header(&mut self) -> Result<GcDiscHeader> {
        let mut head = [0u8; 0x80];
        self.read_stream(0, &mut head)?;
        let game_id = String::from_utf8_lossy(&head[0..6]).into_owned();
        let magic = u32::from_be_bytes(head[0x1C..0x20].try_into().unwrap());
        let raw_title = &head[0x20..0x80];
        let end = raw_title.iter().position(|&b| b == 0).unwrap_or(raw_title.len());
        let title = String::from_utf8_lossy(&raw_title[..end])
            .trim()
            .to_string();
        Ok(GcDiscHeader {
            game_id,
            title,
            is_gamecube: magic == GC_MAGIC,
        })
    }

    /// Stream the whole decompressed image through CRC32 + MD5 +
    /// SHA1 (exactly disc_size bytes). Returns
    /// (crc, md5, sha1, bytes hashed).
    pub fn hash_image(&mut self) -> Result<(u32, [u8; 16], [u8; 20], u64)> {
        use sha1::Digest as _;
        let mut crc = crc32fast::Hasher::new();
        let mut md5h = md5::Context::new();
        let mut sha1h = sha1::Sha1::new();
        let mut hashed: u64 = 0;
        let mut block_buf: Vec<u8> = Vec::new();
        for i in 0..self.pointers.len() as u32 {
            self.read_block_into(i, &mut block_buf)?;
            let remaining = self.disc_size - hashed;
            let take = (block_buf.len() as u64).min(remaining) as usize;
            let seg = &block_buf[..take];
            crc.update(seg);
            md5h.consume(seg);
            sha1h.update(seg);
            hashed += take as u64;
        }
        if hashed != self.disc_size {
            return Err(GczError::Corrupt(format!(
                "stream produced {hashed} bytes, header promises {}",
                self.disc_size
            )));
        }
        let md5_out: [u8; 16] = md5h.compute().0;
        let sha1_out: [u8; 20] = sha1h.finalize().into();
        Ok((crc.finalize(), md5_out, sha1_out, hashed))
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

    fn deflate(data: &[u8]) -> Vec<u8> {
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    /// Build a GCZ around `disc` with the given block size; block
    /// indices in `raw_blocks` are stored uncompressed.
    fn build_gcz(disc: &[u8], block_size: u32, raw_blocks: &[usize]) -> Vec<u8> {
        let num_blocks = (disc.len() as u64).div_ceil(block_size as u64) as usize;
        let mut pointers = Vec::new();
        let mut hashes = Vec::new();
        let mut data: Vec<u8> = Vec::new();
        for i in 0..num_blocks {
            let s = i * block_size as usize;
            let e = ((i + 1) * block_size as usize).min(disc.len());
            let mut block = disc[s..e].to_vec();
            if block.len() < block_size as usize {
                block.resize(block_size as usize, 0); // writer padding
            }
            let stored = if raw_blocks.contains(&i) {
                pointers.push((data.len() as u64) | RAW_FLAG);
                block
            } else {
                pointers.push(data.len() as u64);
                deflate(&block)
            };
            hashes.push(adler32(&stored));
            data.extend_from_slice(&stored);
        }
        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(&GCZ_MAGIC.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&(data.len() as u64).to_le_bytes());
        out.extend_from_slice(&(disc.len() as u64).to_le_bytes());
        out.extend_from_slice(&block_size.to_le_bytes());
        out.extend_from_slice(&(num_blocks as u32).to_le_bytes());
        for p in &pointers {
            out.extend_from_slice(&p.to_le_bytes());
        }
        for h in &hashes {
            out.extend_from_slice(&h.to_le_bytes());
        }
        out.extend_from_slice(&data);
        out
    }

    fn sample_disc() -> Vec<u8> {
        // 4.5 blocks of 2048; a GameCube-style header at the start.
        let mut disc = vec![0u8; 9216];
        for (i, b) in disc.iter_mut().enumerate() {
            *b = ((i * 31 + 7) % 251) as u8;
        }
        disc[0..6].copy_from_slice(b"GTST01");
        disc[0x1C..0x20].copy_from_slice(&GC_MAGIC.to_be_bytes());
        disc[0x20..0x80].fill(0); // real title fields are NUL-padded
        disc[0x20..0x20 + 9].copy_from_slice(b"Test Disc");
        disc
    }

    fn write_tmp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn roundtrip_blocks_and_hash() {
        let disc = sample_disc();
        let bytes = build_gcz(&disc, 2048, &[2]);
        let p = write_tmp("fable-gcz-roundtrip.gcz", &bytes);
        let mut g = GczFile::open(&p).expect("opens");
        assert_eq!(g.disc_size(), disc.len() as u64);
        assert_eq!(g.num_blocks(), 5);
        let mut block = Vec::new();
        for i in 0..5u32 {
            g.read_block_into(i, &mut block).expect("block reads");
            let s = (i as usize) * 2048;
            let e = ((i as usize + 1) * 2048).min(disc.len());
            assert_eq!(&block[..e - s], &disc[s..e], "block {i} payload");
        }
        let (crc, md5, sha1, n) = g.hash_image().expect("hashes");
        assert_eq!(n, disc.len() as u64);
        let mut c = crc32fast::Hasher::new();
        c.update(&disc);
        assert_eq!(crc, c.finalize());
        assert_eq!(md5, md5::compute(&disc).0);
        use sha1::Digest as _;
        let mut s1 = sha1::Sha1::new();
        s1.update(&disc);
        assert_eq!(sha1, <[u8; 20]>::from(s1.finalize()));
        let head = g.disc_header().expect("header");
        assert_eq!(head.game_id, "GTST01");
        assert_eq!(head.title, "Test Disc");
        assert!(head.is_gamecube);
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn bad_magic_and_truncation_are_errors() {
        let p = write_tmp("fable-gcz-bad.gcz", b"NOPE-not-a-gcz-at-all-padpadpad12");
        assert!(matches!(GczFile::open(&p), Err(GczError::BadMagic)));
        std::fs::remove_file(&p).ok();
        let p = write_tmp("fable-gcz-trunc.gcz", &GCZ_MAGIC.to_le_bytes());
        assert!(matches!(GczFile::open(&p), Err(GczError::Truncated(_))));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn adler_mismatch_is_an_error_not_a_guess() {
        let disc = sample_disc();
        let mut bytes = build_gcz(&disc, 2048, &[]);
        // Corrupt the last stored byte (inside block data).
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        let p = write_tmp("fable-gcz-adler.gcz", &bytes);
        let mut g = GczFile::open(&p).expect("opens");
        let r = g.hash_image();
        assert!(matches!(r, Err(GczError::AdlerMismatch(_))), "{r:?}");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn corrupt_pointer_is_rejected_at_open() {
        let disc = sample_disc();
        let mut bytes = build_gcz(&disc, 2048, &[]);
        // Second pointer (offset 32+8) → beyond the compressed data.
        bytes[40..48].copy_from_slice(&u64::MAX.to_le_bytes());
        let p = write_tmp("fable-gcz-ptr.gcz", &bytes);
        assert!(matches!(GczFile::open(&p), Err(GczError::Corrupt(_))));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn garbage_deflate_with_valid_adler_errors() {
        // A block whose stored bytes pass Adler (recomputed) but are
        // not a zlib stream must fail as Inflate/ShortBlock, never hash.
        let disc = sample_disc();
        let mut bytes = build_gcz(&disc, 2048, &[]);
        let data_off = 32 + 5 * 12;
        // Overwrite block 0's stored bytes with garbage, fix its adler.
        let span = {
            let p0 = u64::from_le_bytes(bytes[32..40].try_into().unwrap()) & !RAW_FLAG;
            let p1 = u64::from_le_bytes(bytes[40..48].try_into().unwrap()) & !RAW_FLAG;
            (p0 as usize, (p1 - p0) as usize)
        };
        let (start, len) = span;
        for i in 0..len {
            bytes[data_off + start + i] = 0xAB;
        }
        let fixed = adler32(&bytes[data_off + start..data_off + start + len]);
        bytes[32 + 5 * 8..32 + 5 * 8 + 4].copy_from_slice(&fixed.to_le_bytes());
        let p = write_tmp("fable-gcz-garbage.gcz", &bytes);
        let mut g = GczFile::open(&p).expect("opens");
        let mut block = Vec::new();
        let r = g.read_block_into(0, &mut block);
        assert!(r.is_err(), "{r:?}");
        std::fs::remove_file(&p).ok();
    }
}
