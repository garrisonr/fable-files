//! litedb: a minimal READ-ONLY LiteDB v4 reader — just enough of the
//! format to read Playnite's per-collection `.db` files (PC Games
//! Library P1, 2026-10-17; DESIGN.md §1 route (a)).
//!
//! The format is pinned from the LiteDB 4.1.4 source
//! (Engine/Pages/{BasePage,DataPage,ExtendPage}.cs) and proven
//! byte-for-byte against the real Hyperbase library before any of
//! this shipped — see ~/workspace/fable-pc-games/P1-CHECKPOINT.md:
//!   * 4096-byte pages; 25-byte page header (PageID u32, Type u8,
//!     Prev u32, Next u32, ItemCount u16, FreeBytes u16, 8 reserved).
//!   * Header page: "** This is a LiteDB file **" at offset 25,
//!     FILE_VERSION byte right after it (7 = v4; 8 = v5, a different
//!     layout this reader refuses politely rather than guessing).
//!   * A data page (type 4) holds ItemCount blocks sequentially
//!     from offset 25; each block is [Index u16][ExtendPageID u32]
//!     [Size u16][Data]. A block's full bytes are its Data plus the
//!     extend chain: each extend page (type 5) contributes ItemCount
//!     raw bytes at offset 25, chained by the header's NextPageID.
//!   * One block = one BSON document, parsed strictly.
//!
//! Read-only is a hard law: the file is opened with a read-only
//! handle and copied into memory; nothing here can write. Corrupt
//! or truncated input produces a named error, never a panic —
//! every offset is bounds-checked.

use std::fmt;
use std::path::Path;

pub const PAGE_SIZE: usize = 4096;
pub const PAGE_HEADER_SIZE: usize = 25;
const PAGE_AVAILABLE: usize = PAGE_SIZE - PAGE_HEADER_SIZE;
const FILE_MAGIC: &[u8] = b"** This is a LiteDB file **";
/// FILE_VERSION for the v4 layout (LiteDB 4.x). v5 files carry 8
/// and an 8192-byte page — a different format entirely.
pub const FILE_VERSION_V4: u8 = 7;
const NO_PAGE: u32 = u32::MAX;
/// Sanity cap for extend chains (LiteDB's own max document size is
/// ~2047 pages; a chain longer than the file has pages is corrupt).
const MAX_EXTEND_HOPS: usize = 4096;
const MAX_BSON_DEPTH: usize = 64;
/// Import-side guard: a Playnite collection file is small (the
/// real games.db is 1.6 MB). Refuse absurd inputs instead of
/// trying to swallow them.
const MAX_DB_BYTES: u64 = 512 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum LiteDbError {
    Io(std::io::Error),
    /// Not a LiteDB file at all (bad magic / bad header page).
    NotLiteDb(String),
    /// A LiteDB file, but a format version this reader does not
    /// speak (e.g. v5). The message is written for the user.
    UnsupportedVersion(u8),
    Truncated(&'static str),
    Corrupt(String),
}

impl fmt::Display for LiteDbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LiteDbError::Io(e) => write!(f, "litedb: io error: {e}"),
            LiteDbError::NotLiteDb(m) => write!(f, "litedb: not a LiteDB file: {m}"),
            LiteDbError::UnsupportedVersion(v) => write!(
                f,
                "litedb: this Playnite library uses a newer database format \
                 (LiteDB file version {v}); this version of Fable reads the \
                 classic format (version 7)"
            ),
            LiteDbError::Truncated(what) => write!(f, "litedb: truncated {what}"),
            LiteDbError::Corrupt(m) => write!(f, "litedb: corrupt: {m}"),
        }
    }
}

impl std::error::Error for LiteDbError {}

impl From<std::io::Error> for LiteDbError {
    fn from(e: std::io::Error) -> Self {
        LiteDbError::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, LiteDbError>;

fn corrupt<T>(msg: impl Into<String>) -> Result<T> {
    Err(LiteDbError::Corrupt(msg.into()))
}

// ---------------------------------------------------------------------------
// BSON values
// ---------------------------------------------------------------------------

/// A BSON value, document order preserved. GUIDs arrive pre-formatted
/// in canonical .NET string form (LiteDB stores .NET Guids as binary
/// subtype 3/4 in mixed-endian order).
#[derive(Debug, Clone, PartialEq)]
pub enum BsonValue {
    Null,
    Bool(bool),
    Int32(i32),
    Int64(i64),
    Double(f64),
    Str(String),
    /// Milliseconds since the Unix epoch (BSON datetime).
    DateTime(i64),
    Guid(String),
    Binary { subtype: u8, data: Vec<u8> },
    ObjectId([u8; 12]),
    Array(Vec<BsonValue>),
    Doc(Vec<(String, BsonValue)>),
    Decimal128([u8; 16]),
    Timestamp(u64),
    Regex(String, String),
}

impl BsonValue {
    pub fn get<'a>(&'a self, key: &str) -> Option<&'a BsonValue> {
        match self {
            BsonValue::Doc(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            BsonValue::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            BsonValue::Int32(n) => Some(*n as i64),
            BsonValue::Int64(n) => Some(*n),
            BsonValue::Double(d) if d.fract() == 0.0 && *d >= i64::MIN as f64 && *d <= i64::MAX as f64 => {
                Some(*d as i64)
            }
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            BsonValue::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_guid(&self) -> Option<&str> {
        match self {
            BsonValue::Guid(g) => Some(g),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[BsonValue]> {
        match self {
            BsonValue::Array(items) => Some(items),
            _ => None,
        }
    }

    /// The string elements of an array of GUIDs (Playnite's *Ids
    /// fields), skipping anything that is not a GUID.
    pub fn guid_list(&self) -> Vec<String> {
        self.as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_guid().map(|g| g.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The string elements of a BSON array, skipping non-strings.
    pub fn str_list(&self) -> Vec<String> {
        self.as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Format 16 raw bytes as a .NET Guid string: the first three
/// components are stored little-endian, the last eight bytes as-is.
pub fn dotnet_guid_string(raw: &[u8]) -> Option<String> {
    if raw.len() != 16 {
        return None;
    }
    let d1 = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
    let d2 = u16::from_le_bytes([raw[4], raw[5]]);
    let d3 = u16::from_le_bytes([raw[6], raw[7]]);
    Some(format!(
        "{d1:08x}-{d2:04x}-{d3:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        raw[8], raw[9], raw[10], raw[11], raw[12], raw[13], raw[14], raw[15]
    ))
}

// ---------------------------------------------------------------------------
// BSON parsing (strict: lengths, terminators, exact consumption)
// ---------------------------------------------------------------------------

struct BsonReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> BsonReader<'a> {
    fn take(&mut self, n: usize, what: &'static str) -> Result<&'a [u8]> {
        if self.pos + n > self.buf.len() {
            return Err(LiteDbError::Truncated(what));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1, "bson byte")?[0])
    }

    fn i32(&mut self) -> Result<i32> {
        let b = self.take(4, "bson int32")?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i64(&mut self) -> Result<i64> {
        let b = self.take(8, "bson int64")?;
        Ok(i64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn cstring(&mut self) -> Result<String> {
        let start = self.pos;
        let mut end = start;
        while end < self.buf.len() && self.buf[end] != 0 {
            end += 1;
        }
        if end >= self.buf.len() {
            return Err(LiteDbError::Truncated("bson cstring"));
        }
        let s = String::from_utf8_lossy(&self.buf[start..end]).into_owned();
        self.pos = end + 1;
        Ok(s)
    }

    fn string(&mut self) -> Result<String> {
        let len = self.i32()?;
        if len < 1 {
            return corrupt(format!("bson string length {len}"));
        }
        let bytes = self.take(len as usize, "bson string")?;
        if bytes[len as usize - 1] != 0 {
            return corrupt("bson string missing NUL terminator");
        }
        Ok(String::from_utf8_lossy(&bytes[..len as usize - 1]).into_owned())
    }

    /// Parse one document starting at the reader's position; the
    /// declared length governs. Returns the fields in order.
    fn document(&mut self, depth: usize) -> Result<Vec<(String, BsonValue)>> {
        if depth > MAX_BSON_DEPTH {
            return corrupt("bson nesting too deep");
        }
        let len = self.i32()?;
        if len < 5 {
            return corrupt(format!("bson document length {len}"));
        }
        let doc_end = self
            .pos
            .checked_sub(4)
            .and_then(|p| p.checked_add(len as usize))
            .ok_or(LiteDbError::Truncated("bson document"))?;
        if doc_end > self.buf.len() {
            return Err(LiteDbError::Truncated("bson document"));
        }
        let mut fields = Vec::new();
        loop {
            if self.pos >= doc_end {
                return corrupt("bson document overran its length");
            }
            let t = self.u8()?;
            if t == 0 {
                if self.pos != doc_end {
                    return corrupt("bson document terminator before its length");
                }
                break;
            }
            let name = self.cstring()?;
            let value = self.value(t, depth)?;
            fields.push((name, value));
        }
        Ok(fields)
    }

    fn value(&mut self, t: u8, depth: usize) -> Result<BsonValue> {
        Ok(match t {
            0x01 => {
                let b = self.take(8, "bson double")?;
                BsonValue::Double(f64::from_le_bytes([
                    b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
                ]))
            }
            0x02 => BsonValue::Str(self.string()?),
            0x03 => BsonValue::Doc(self.document(depth + 1)?),
            0x04 => {
                let fields = self.document(depth + 1)?;
                let mut items: Vec<BsonValue> = Vec::with_capacity(fields.len());
                for (i, (k, v)) in fields.into_iter().enumerate() {
                    if k != i.to_string() {
                        return corrupt(format!("bson array key {k:?} out of order"));
                    }
                    items.push(v);
                }
                BsonValue::Array(items)
            }
            0x05 => {
                let len = self.i32()?;
                if len < 0 {
                    return corrupt(format!("bson binary length {len}"));
                }
                let subtype = self.u8()?;
                if subtype == 2 {
                    // Old-style binary wraps the payload in its own
                    // length; validate and unwrap.
                    let inner = self.i32()?;
                    if inner != len - 4 {
                        return corrupt("bson old-binary length mismatch");
                    }
                    let data = self.take(inner as usize, "bson binary")?.to_vec();
                    BsonValue::Binary { subtype, data }
                } else {
                    let data = self.take(len as usize, "bson binary")?.to_vec();
                    if (subtype == 3 || subtype == 4) && data.len() == 16 {
                        match dotnet_guid_string(&data) {
                            Some(g) => BsonValue::Guid(g),
                            None => BsonValue::Binary { subtype, data },
                        }
                    } else {
                        BsonValue::Binary { subtype, data }
                    }
                }
            }
            0x06 => BsonValue::Null, // undefined
            0x07 => {
                let b = self.take(12, "bson objectid")?;
                let mut oid = [0u8; 12];
                oid.copy_from_slice(b);
                BsonValue::ObjectId(oid)
            }
            0x08 => match self.u8()? {
                0 => BsonValue::Bool(false),
                1 => BsonValue::Bool(true),
                other => return corrupt(format!("bson bool byte {other}")),
            },
            0x09 => BsonValue::DateTime(self.i64()?),
            0x0A => BsonValue::Null,
            0x0B => {
                let pattern = self.cstring()?;
                let options = self.cstring()?;
                BsonValue::Regex(pattern, options)
            }
            0x0D => BsonValue::Str(self.string()?), // javascript code
            0x10 => BsonValue::Int32(self.i32()?),
            0x11 => {
                let b = self.take(8, "bson timestamp")?;
                BsonValue::Timestamp(u64::from_le_bytes([
                    b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
                ]))
            }
            0x12 => BsonValue::Int64(self.i64()?),
            0x13 => {
                let b = self.take(16, "bson decimal128")?;
                let mut d = [0u8; 16];
                d.copy_from_slice(b);
                BsonValue::Decimal128(d)
            }
            other => {
                return corrupt(format!("unsupported bson type 0x{other:02x}"));
            }
        })
    }
}

/// Parse a buffer holding exactly one BSON document.
pub fn parse_bson_document(buf: &[u8]) -> Result<BsonValue> {
    let mut reader = BsonReader { buf, pos: 0 };
    let fields = reader.document(0)?;
    if reader.pos != buf.len() {
        return corrupt(format!(
            "bson document consumed {} of {} bytes",
            reader.pos,
            buf.len()
        ));
    }
    Ok(BsonValue::Doc(fields))
}

// ---------------------------------------------------------------------------
// The LiteDB v4 file
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct PageHeader {
    page_type: u8,
    next: u32,
    item_count: u16,
    free_bytes: u16,
}

/// One LiteDB v4 collection file (Playnite keeps one collection per
/// `.db` file), loaded read-only into memory.
#[derive(Debug)]
pub struct LiteDbFile {
    data: Vec<u8>,
    page_count: usize,
    pub file_version: u8,
}

impl LiteDbFile {
    /// Open and validate a LiteDB v4 file. The handle is read-only
    /// and the bytes are copied out — the source is never written,
    /// locked, or journaled by this reader.
    pub fn open(path: &Path) -> Result<Self> {
        let meta = std::fs::metadata(path)?;
        if meta.len() > MAX_DB_BYTES {
            return corrupt(format!(
                "{} is {} bytes — too large for a Playnite collection file",
                path.display(),
                meta.len()
            ));
        }
        let data = std::fs::read(path)?;
        Self::from_bytes(data)
    }

    pub fn from_bytes(data: Vec<u8>) -> Result<Self> {
        if data.len() < PAGE_SIZE {
            return Err(LiteDbError::Truncated("file (smaller than one page)"));
        }
        if data.len() % PAGE_SIZE != 0 {
            return corrupt(format!(
                "file size {} is not a whole number of {}-byte pages",
                data.len(),
                PAGE_SIZE
            ));
        }
        let page_count = data.len() / PAGE_SIZE;
        let header = Self::page_header_in(&data, 0)?;
        if header.page_type != 1 {
            return Err(LiteDbError::NotLiteDb(format!(
                "page 0 has type {}, expected the header page",
                header.page_type
            )));
        }
        let magic_end = PAGE_HEADER_SIZE + FILE_MAGIC.len();
        if &data[PAGE_HEADER_SIZE..magic_end] != FILE_MAGIC {
            return Err(LiteDbError::NotLiteDb(
                "missing \"** This is a LiteDB file **\" marker".into(),
            ));
        }
        let file_version = data[magic_end];
        if file_version != FILE_VERSION_V4 {
            return Err(LiteDbError::UnsupportedVersion(file_version));
        }
        Ok(Self {
            data,
            page_count,
            file_version,
        })
    }

    fn page_header_in(data: &[u8], page: usize) -> Result<PageHeader> {
        let base = page * PAGE_SIZE;
        if base + PAGE_HEADER_SIZE > data.len() {
            return Err(LiteDbError::Truncated("page header"));
        }
        let stored_id = u32::from_le_bytes([
            data[base],
            data[base + 1],
            data[base + 2],
            data[base + 3],
        ]);
        let page_type = data[base + 4];
        // Empty pages are zeroed by the writer (their stored id is
        // 0, not their offset) — the id check applies to every
        // page that carries content.
        if page_type != 0 && stored_id as usize != page {
            return corrupt(format!(
                "page at offset {page} carries id {stored_id}"
            ));
        }
        Ok(PageHeader {
            page_type,
            next: u32::from_le_bytes([
                data[base + 9],
                data[base + 10],
                data[base + 11],
                data[base + 12],
            ]),
            item_count: u16::from_le_bytes([data[base + 13], data[base + 14]]),
            free_bytes: u16::from_le_bytes([data[base + 15], data[base + 16]]),
        })
    }

    fn page_header(&self, page: usize) -> Result<PageHeader> {
        Self::page_header_in(&self.data, page)
    }

    fn page_slice(&self, page: usize) -> &[u8] {
        let base = page * PAGE_SIZE;
        &self.data[base..base + PAGE_SIZE]
    }

    /// Assemble one block's full bytes: its data-page segment plus
    /// the extend chain, when it has one.
    fn assemble_block(&self, first_segment: &[u8], extend_page: u32) -> Result<Vec<u8>> {
        let mut out = first_segment.to_vec();
        let mut next = extend_page;
        let mut hops = 0usize;
        while next != NO_PAGE {
            hops += 1;
            if hops > MAX_EXTEND_HOPS {
                return corrupt("extend chain too long");
            }
            let page = next as usize;
            if page >= self.page_count {
                return corrupt(format!("extend chain points at missing page {page}"));
            }
            let header = self.page_header(page)?;
            if header.page_type != 5 {
                return corrupt(format!(
                    "extend chain points at page {page} of type {} (expected extend)",
                    header.page_type
                ));
            }
            let n = header.item_count as usize;
            if n > PAGE_AVAILABLE {
                return corrupt(format!(
                    "extend page {page} claims {n} data bytes (max {PAGE_AVAILABLE})"
                ));
            }
            let slice = self.page_slice(page);
            out.extend_from_slice(&slice[PAGE_HEADER_SIZE..PAGE_HEADER_SIZE + n]);
            next = header.next;
        }
        Ok(out)
    }

    /// Every document in the collection, in page/block order. Only
    /// data pages' live block tables are read — bytes stranded on
    /// freed or empty pages are never resurrected.
    pub fn documents(&self) -> Result<Vec<BsonValue>> {
        let mut docs = Vec::new();
        for page in 0..self.page_count {
            let header = self.page_header(page)?;
            if header.page_type != 4 {
                continue;
            }
            let slice = self.page_slice(page);
            let mut cursor = PAGE_HEADER_SIZE;
            // Blocks are a dictionary on the page, keyed by their
            // stored index — a page whose block 0 was deleted keeps
            // blocks 1..n, serialized in dictionary order. Order is
            // not meaning; a duplicated index is.
            let mut seen_indexes = std::collections::HashSet::new();
            for block_index in 0..header.item_count {
                if cursor + 8 > PAGE_SIZE {
                    return corrupt(format!(
                        "page {page} block {block_index} header overruns the page"
                    ));
                }
                let stored_index =
                    u16::from_le_bytes([slice[cursor], slice[cursor + 1]]);
                if !seen_indexes.insert(stored_index) {
                    return corrupt(format!(
                        "page {page} carries block index {stored_index} twice"
                    ));
                }
                let extend = u32::from_le_bytes([
                    slice[cursor + 2],
                    slice[cursor + 3],
                    slice[cursor + 4],
                    slice[cursor + 5],
                ]);
                let size =
                    u16::from_le_bytes([slice[cursor + 6], slice[cursor + 7]]) as usize;
                let start = cursor + 8;
                if start + size > PAGE_SIZE {
                    return corrupt(format!(
                        "page {page} block {block_index} data overruns the page"
                    ));
                }
                let bytes = self.assemble_block(&slice[start..start + size], extend)?;
                let doc = parse_bson_document(&bytes).map_err(|e| match e {
                    LiteDbError::Corrupt(m) => LiteDbError::Corrupt(format!(
                        "page {page} block {block_index}: {m}"
                    )),
                    other => other,
                })?;
                docs.push(doc);
                cursor = start + size;
            }
            // The header's free-space accounting must close: content
            // bytes + free bytes == the page's available bytes. A
            // mismatch means the block table and the header disagree
            // about the page — trust neither.
            let used = cursor - PAGE_HEADER_SIZE;
            if used + header.free_bytes as usize != PAGE_AVAILABLE {
                return corrupt(format!(
                    "page {page} block table uses {used} bytes but the header \
                     claims {} free (expected {})",
                    header.free_bytes,
                    PAGE_AVAILABLE - used
                ));
            }
        }
        Ok(docs)
    }

    /// (data pages, extend pages, empty pages) — the shape census,
    /// handy for import reports and tests.
    pub fn page_census(&self) -> (usize, usize, usize) {
        let (mut data, mut extend, mut empty) = (0usize, 0usize, 0usize);
        for page in 0..self.page_count {
            if let Ok(h) = self.page_header(page) {
                match h.page_type {
                    4 => data += 1,
                    5 => extend += 1,
                    0 => empty += 1,
                    _ => {}
                }
            }
        }
        (data, extend, empty)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a one-page LiteDB file holding a single data page with
    /// the given blocks (index assigned sequentially). Test fixture
    /// carpentry — the production reader never writes.
    fn fixture_file(blocks: &[(u32, &[u8])], page_type: u8) -> Vec<u8> {
        let mut data = vec![0u8; PAGE_SIZE * 2];
        // Header page.
        data[4] = 1;
        data[PAGE_HEADER_SIZE..PAGE_HEADER_SIZE + FILE_MAGIC.len()]
            .copy_from_slice(FILE_MAGIC);
        data[PAGE_HEADER_SIZE + FILE_MAGIC.len()] = FILE_VERSION_V4;
        // Data page at page 1.
        let base = PAGE_SIZE;
        data[base..base + 4].copy_from_slice(&1u32.to_le_bytes());
        data[base + 4] = page_type;
        data[base + 5..base + 9].copy_from_slice(&NO_PAGE.to_le_bytes());
        data[base + 9..base + 13].copy_from_slice(&NO_PAGE.to_le_bytes());
        data[base + 13..base + 15]
            .copy_from_slice(&(blocks.len() as u16).to_le_bytes());
        let mut cursor = base + PAGE_HEADER_SIZE;
        let mut used = 0usize;
        for (i, (extend, payload)) in blocks.iter().enumerate() {
            data[cursor..cursor + 2].copy_from_slice(&(i as u16).to_le_bytes());
            data[cursor + 2..cursor + 6].copy_from_slice(&extend.to_le_bytes());
            data[cursor + 6..cursor + 8]
                .copy_from_slice(&(payload.len() as u16).to_le_bytes());
            cursor += 8;
            data[cursor..cursor + payload.len()].copy_from_slice(payload);
            cursor += payload.len();
            used += 8 + payload.len();
        }
        let free = PAGE_AVAILABLE - used;
        data[base + 15..base + 17].copy_from_slice(&(free as u16).to_le_bytes());
        data
    }

    /// A BSON doc {_id: guid, Name: "X", N: 7} built by hand.
    fn sample_bson() -> Vec<u8> {
        let mut body = Vec::new();
        // _id: binary subtype 4, 16 bytes
        body.push(0x05);
        body.extend_from_slice(b"_id\0");
        body.extend_from_slice(&16i32.to_le_bytes());
        body.push(4);
        body.extend_from_slice(&[
            0x78, 0x56, 0x34, 0x12, 0xbc, 0x9a, 0xf0, 0xde, 0x11, 0x22, 0x33, 0x44, 0x55,
            0x66, 0x77, 0x88,
        ]);
        // Name: string
        body.push(0x02);
        body.extend_from_slice(b"Name\0");
        body.extend_from_slice(&2i32.to_le_bytes());
        body.extend_from_slice(b"X\0");
        // N: int32
        body.push(0x10);
        body.extend_from_slice(b"N\0");
        body.extend_from_slice(&7i32.to_le_bytes());
        body.push(0);
        let len = (body.len() + 4) as i32;
        let mut out = len.to_le_bytes().to_vec();
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn reads_a_single_block_document() {
        let bson = sample_bson();
        let file = LiteDbFile::from_bytes(fixture_file(&[(NO_PAGE, &bson)], 4)).unwrap();
        let docs = file.documents().unwrap();
        assert_eq!(docs.len(), 1);
        let doc = &docs[0];
        assert_eq!(
            doc.get("_id").and_then(|v| v.as_guid()),
            Some("12345678-9abc-def0-1122-334455667788")
        );
        assert_eq!(doc.get("Name").and_then(|v| v.as_str()), Some("X"));
        assert_eq!(doc.get("N").and_then(|v| v.as_i64()), Some(7));
    }

    #[test]
    fn guid_byte_order_is_dotnet_mixed_endian() {
        let raw: [u8; 16] = [
            0x78, 0x56, 0x34, 0x12, 0xbc, 0x9a, 0xf0, 0xde, 0x11, 0x22, 0x33, 0x44, 0x55,
            0x66, 0x77, 0x88,
        ];
        assert_eq!(
            dotnet_guid_string(&raw).as_deref(),
            Some("12345678-9abc-def0-1122-334455667788")
        );
    }

    #[test]
    fn refuses_v5_files_politely() {
        let mut data = vec![0u8; PAGE_SIZE];
        data[4] = 1;
        data[PAGE_HEADER_SIZE..PAGE_HEADER_SIZE + FILE_MAGIC.len()]
            .copy_from_slice(FILE_MAGIC);
        data[PAGE_HEADER_SIZE + FILE_MAGIC.len()] = 8;
        match LiteDbFile::from_bytes(data) {
            Err(LiteDbError::UnsupportedVersion(8)) => {}
            other => panic!("expected UnsupportedVersion(8), got {other:?}"),
        }
    }

    #[test]
    fn refuses_non_litedb_bytes() {
        let data = vec![0u8; PAGE_SIZE];
        assert!(matches!(
            LiteDbFile::from_bytes(data),
            Err(LiteDbError::NotLiteDb(_))
        ));
    }

    #[test]
    fn corrupt_block_table_is_a_named_error_not_a_panic() {
        let bson = sample_bson();
        let mut bytes = fixture_file(&[(NO_PAGE, &bson)], 4);
        // Lie about the free bytes so the accounting cannot close.
        bytes[PAGE_SIZE + 15] = 0;
        bytes[PAGE_SIZE + 16] = 0;
        let file = LiteDbFile::from_bytes(bytes).unwrap();
        match file.documents() {
            Err(LiteDbError::Corrupt(m)) => assert!(m.contains("free"), "{m}"),
            other => panic!("expected Corrupt, got {other:?}"),
        }
    }

    #[test]
    fn truncated_file_is_a_named_error() {
        let mut bytes = fixture_file(&[(NO_PAGE, &sample_bson())], 4);
        bytes.truncate(PAGE_SIZE + 100);
        assert!(LiteDbFile::from_bytes(bytes).is_err());
    }

    #[test]
    fn garbage_bson_is_a_named_error() {
        let junk: &[u8] = &[1, 2, 3, 4, 5, 6, 7, 8];
        let file = LiteDbFile::from_bytes(fixture_file(&[(NO_PAGE, junk)], 4)).unwrap();
        assert!(file.documents().is_err());
    }
}
