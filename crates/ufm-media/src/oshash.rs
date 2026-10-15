//! The OpenSubtitles movie hash — the fingerprint that makes
//! hash-matched subtitles actually sync (Media Completion,
//! Dickles 2026-10-15).
//!
//! The algorithm is publicly specified by OpenSubtitles:
//!
//!   hash = file_size
//!        + Σ (64-bit little-endian unsigned words of the first 64 KiB)
//!        + Σ (64-bit little-endian unsigned words of the last 64 KiB)
//!
//! all arithmetic modulo 2^64, rendered as 16 lowercase hex digits.
//! Files smaller than 128 KiB have no hash (the two chunks would
//! overlap) — callers fall back to id/name search for those, exactly
//! as the reference implementations do.
//!
//! Proof (see tests): the expected values are NOT produced by this
//! code — they come from an independent Python implementation written
//! from the same published spec, plus edge vectors derivable from the
//! spec by hand (an all-zero file's word sums are all zero, so its
//! hash is exactly its size). No official (file → hash) pair is
//! published in the API docs; the cross-implementation agreement is
//! the evidence, and the module docs say so rather than dress it up.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// One hash chunk: 64 KiB, read as 8192 little-endian u64 words.
pub const CHUNK: u64 = 65536;

/// Sum one 64 KiB chunk as little-endian u64 words (wrapping).
/// A short final read is zero-padded — only reachable for the
/// in-memory core; the file path enforces the ≥128 KiB rule first.
pub fn chunk_sum(chunk: &[u8]) -> u64 {
    let mut sum: u64 = 0;
    let mut words = chunk.chunks_exact(8);
    for w in &mut words {
        sum = sum.wrapping_add(u64::from_le_bytes(w.try_into().unwrap()));
    }
    let rem = words.remainder();
    if !rem.is_empty() {
        let mut buf = [0u8; 8];
        buf[..rem.len()].copy_from_slice(rem);
        sum = sum.wrapping_add(u64::from_le_bytes(buf));
    }
    sum
}

/// The pure core: hash from (size, head chunk, tail chunk).
pub fn hash_from_parts(size: u64, head: &[u8], tail: &[u8]) -> String {
    let h = size
        .wrapping_add(chunk_sum(head))
        .wrapping_add(chunk_sum(tail));
    format!("{h:016x}")
}

/// Compute the OpenSubtitles hash of a file. `None` when the file is
/// smaller than 128 KiB (unhashable per the spec) or unreadable.
pub fn opensubtitles_hash(path: &Path) -> Option<String> {
    let mut f = File::open(path).ok()?;
    let size = f.metadata().ok()?.len();
    if size < CHUNK * 2 {
        return None;
    }
    let mut head = vec![0u8; CHUNK as usize];
    f.read_exact(&mut head).ok()?;
    f.seek(SeekFrom::End(-(CHUNK as i64))).ok()?;
    let mut tail = vec![0u8; CHUNK as usize];
    f.read_exact(&mut tail).ok()?;
    Some(hash_from_parts(size, &head, &tail))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Deterministic pseudo-random fixture bytes (xorshift64*), the
    /// same generator the independent Python checker uses.
    fn fixture_bytes(n: usize) -> Vec<u8> {
        let mut state: u64 = 0x9E3779B97F4A7C15;
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            let v = state.wrapping_mul(0x2545F4914F6CDD1D);
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.truncate(n);
        out
    }

    #[test]
    fn zero_file_hash_is_exactly_its_size() {
        // Derivable from the spec by hand: every word is 0, so
        // hash == size. 200_000 = 0x30D40.
        let head = vec![0u8; CHUNK as usize];
        let tail = vec![0u8; CHUNK as usize];
        assert_eq!(hash_from_parts(200_000, &head, &tail), "0000000000030d40");
    }

    #[test]
    fn first_word_contributes_little_endian() {
        // One word of value 1 at the very start: hash == size + 1.
        let mut head = vec![0u8; CHUNK as usize];
        head[0] = 1;
        let tail = vec![0u8; CHUNK as usize];
        assert_eq!(hash_from_parts(131_072, &head, &tail), "0000000000020001");
        // The same byte at offset 1 is the word 0x100, not 1.
        let mut head2 = vec![0u8; CHUNK as usize];
        head2[1] = 1;
        assert_eq!(hash_from_parts(131_072, &head2, &tail), "0000000000020100");
    }

    #[test]
    fn matches_independent_python_implementation() {
        // Expected value computed by an independent Python
        // implementation of the published algorithm (written from
        // the spec text, sharing only the fixture generator seed):
        //   size=300000, head/tail = first/last 65536 fixture bytes.
        // The Python source + output live in the build checkpoint
        // (fable-mediacomplete) so the check is reproducible.
        let bytes = fixture_bytes(300_000);
        let head = &bytes[..CHUNK as usize];
        let tail = &bytes[bytes.len() - CHUNK as usize..];
        let got = hash_from_parts(300_000, head, tail);
        assert_eq!(got, python_expected());
    }

    /// The value the independent Python implementation printed for
    /// this exact fixture (see hash cross-check in the build
    /// checkpoint): 0037385a07b3f376.
    fn python_expected() -> String {
        "0037385a07b3f376".to_string()
    }

    #[test]
    fn file_roundtrip_and_too_small() {
        let dir = std::env::temp_dir().join(format!(
            "ufm-oshash-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let big = dir.join("big.bin");
        let bytes = fixture_bytes(300_000);
        let mut f = File::create(&big).unwrap();
        f.write_all(&bytes).unwrap();
        drop(f);
        let head = &bytes[..CHUNK as usize];
        let tail = &bytes[bytes.len() - CHUNK as usize..];
        assert_eq!(
            opensubtitles_hash(&big).as_deref(),
            Some(hash_from_parts(300_000, head, tail).as_str())
        );
        let small = dir.join("small.bin");
        std::fs::write(&small, vec![7u8; 100_000]).unwrap();
        assert_eq!(opensubtitles_hash(&small), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
