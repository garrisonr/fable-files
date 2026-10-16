//! A minimal in-memory zip reader — just enough to pull one entry
//! (a libretro core `.so`) out of a buildbot zip: stored + deflate
//! entries, central-directory parsing, no zip64 (buildbot zips are
//! small), a hard size cap so a hostile archive can't balloon.

use std::io::Read;

const MAX_ENTRY_BYTES: u64 = 512 * 1024 * 1024;

fn u16le(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(at)?, *b.get(at + 1)?]))
}

fn u32le(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *b.get(at)?,
        *b.get(at + 1)?,
        *b.get(at + 2)?,
        *b.get(at + 3)?,
    ]))
}

/// Extract the first entry whose name ends with `suffix`
/// (case-insensitive) from a zip held in memory.
pub fn extract_entry(zip: &[u8], suffix: &str) -> Result<Vec<u8>, String> {
    // Locate the End Of Central Directory record (scan the tail;
    // the comment field can push it back up to 64 KiB).
    let tail_start = zip.len().saturating_sub(65_557 + 22);
    let mut eocd = None;
    let mut i = zip.len().saturating_sub(22);
    while i >= tail_start {
        if u32le(zip, i) == Some(0x0605_4b50) {
            eocd = Some(i);
            break;
        }
        if i == 0 {
            break;
        }
        i -= 1;
    }
    let eocd = eocd.ok_or("not a zip file (no end-of-directory record)")?;
    let count = u16le(zip, eocd + 10).ok_or("truncated zip directory")? as usize;
    let mut at = u32le(zip, eocd + 16).ok_or("truncated zip directory")? as usize;

    for _ in 0..count {
        if u32le(zip, at) != Some(0x0201_4b50) {
            return Err("corrupt zip directory".to_string());
        }
        let method = u16le(zip, at + 10).ok_or("truncated zip entry")?;
        let comp_size = u32le(zip, at + 20).ok_or("truncated zip entry")? as usize;
        let raw_size = u32le(zip, at + 24).ok_or("truncated zip entry")? as u64;
        let name_len = u16le(zip, at + 28).ok_or("truncated zip entry")? as usize;
        let extra_len = u16le(zip, at + 30).ok_or("truncated zip entry")? as usize;
        let comment_len = u16le(zip, at + 32).ok_or("truncated zip entry")? as usize;
        let local_at = u32le(zip, at + 42).ok_or("truncated zip entry")? as usize;
        let name_bytes = zip
            .get(at + 46..at + 46 + name_len)
            .ok_or("truncated zip entry")?;
        let name = String::from_utf8_lossy(name_bytes).into_owned();
        if name.to_lowercase().ends_with(&suffix.to_lowercase()) {
            if raw_size > MAX_ENTRY_BYTES {
                return Err(format!("{name} is implausibly large; refusing"));
            }
            // Local header: skip its name + extra to find the data.
            if u32le(zip, local_at) != Some(0x0403_4b50) {
                return Err("corrupt zip local header".to_string());
            }
            let l_name = u16le(zip, local_at + 26).ok_or("truncated local header")? as usize;
            let l_extra = u16le(zip, local_at + 28).ok_or("truncated local header")? as usize;
            let data_at = local_at + 30 + l_name + l_extra;
            let data = zip
                .get(data_at..data_at + comp_size)
                .ok_or("truncated zip data")?;
            return match method {
                0 => Ok(data.to_vec()),
                8 => {
                    let mut dec = flate2::read::DeflateDecoder::new(data);
                    let mut out = Vec::with_capacity(raw_size as usize);
                    dec.read_to_end(&mut out)
                        .map_err(|e| format!("couldn't unpack {name}: {e}"))?;
                    Ok(out)
                }
                other => Err(format!("unsupported zip compression ({other})")),
            };
        }
        at += 46 + name_len + extra_len + comment_len;
    }
    Err(format!("no entry ending in {suffix} in this zip"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a tiny zip by hand (one deflated entry + one stored).
    fn make_zip() -> Vec<u8> {
        let payload = b"FAKE-CORE-BYTES".repeat(64);
        let mut encoder = flate2::write::DeflateEncoder::new(
            Vec::new(),
            flate2::Compression::default(),
        );
        encoder.write_all(&payload).unwrap();
        let deflated = encoder.finish().unwrap();

        let mut zip: Vec<u8> = Vec::new();
        // Local header + data for entry 1 (deflated .so).
        let name1 = b"mesen_libretro.so";
        let mut local1_at = 0usize;
        zip.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        zip.extend_from_slice(&20u16.to_le_bytes()); // version
        zip.extend_from_slice(&0u16.to_le_bytes()); // flags
        zip.extend_from_slice(&8u16.to_le_bytes()); // method deflate
        zip.extend_from_slice(&0u16.to_le_bytes()); // time
        zip.extend_from_slice(&0u16.to_le_bytes()); // date
        zip.extend_from_slice(&0u32.to_le_bytes()); // crc (unchecked)
        zip.extend_from_slice(&(deflated.len() as u32).to_le_bytes());
        zip.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        zip.extend_from_slice(&(name1.len() as u16).to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes()); // extra len
        zip.extend_from_slice(name1);
        zip.extend_from_slice(&deflated);
        local1_at = 0;
        let central_at = zip.len();
        // Central directory entry.
        zip.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        zip.extend_from_slice(&20u16.to_le_bytes());
        zip.extend_from_slice(&20u16.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&8u16.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&0u32.to_le_bytes());
        zip.extend_from_slice(&(deflated.len() as u32).to_le_bytes());
        zip.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        zip.extend_from_slice(&(name1.len() as u16).to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes()); // extra
        zip.extend_from_slice(&0u16.to_le_bytes()); // comment
        zip.extend_from_slice(&0u16.to_le_bytes()); // disk
        zip.extend_from_slice(&0u16.to_le_bytes()); // int attrs
        zip.extend_from_slice(&0u32.to_le_bytes()); // ext attrs
        zip.extend_from_slice(&(local1_at as u32).to_le_bytes());
        zip.extend_from_slice(name1);
        let central_len = zip.len() - central_at;
        // EOCD.
        zip.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&1u16.to_le_bytes());
        zip.extend_from_slice(&1u16.to_le_bytes());
        zip.extend_from_slice(&(central_len as u32).to_le_bytes());
        zip.extend_from_slice(&(central_at as u32).to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip
    }

    #[test]
    fn extracts_the_core_from_a_buildbot_shaped_zip() {
        let zip = make_zip();
        let out = extract_entry(&zip, ".so").unwrap();
        assert_eq!(out, b"FAKE-CORE-BYTES".repeat(64));
        assert!(extract_entry(&zip, ".dll").is_err());
        assert!(extract_entry(b"not a zip", ".so").is_err());
    }
}
