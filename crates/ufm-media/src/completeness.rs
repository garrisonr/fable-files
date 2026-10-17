//! Collection Completeness (2026-10-17, Dickles' ruling): what a
//! library owns versus the DAT reference universe in rom-dats.bin —
//! and, more importantly, what it is MISSING, per system, in the
//! format the collector actually collects in.
//!
//! Semantics mirror the shipped Games Home shelf scan exactly, so
//! the numbers here can never contradict the shelves:
//!   * the unit is the TITLE GROUP (one game = one group; regions,
//!     revisions, discs and hacks fold in-group via `rom_group_key`,
//!     the same function the catalog folds with);
//!   * a group is owned when ANY of its releases is owned;
//!   * ownership arrives as (system, DAT title) pairs, already
//!     membership-expanded by the caller (primary platform + the
//!     evidence `also` list, exactly like `rom_home_counts`).
//!
//! Reference units: DAT rows for the flat systems; MAME SETS for
//! the set systems (arcade / neogeo — a MAME collection is a set
//! collection, and set displays are what identifications carry).
//! A system whose reference side is empty (wiiu ships an empty
//! DAT) has NO reference set: completeness is never claimed for
//! it, and no list is invented.

use crate::romdat::{parsed_base_title, rom_group_key, variant_of_title, RomDataset, RomVariant};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::io::Write;

// ---------------------------------------------------------------------
// Reference side
// ---------------------------------------------------------------------

/// One known release of a game: a DAT row (flat systems) or one
/// MAME set (set systems).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RefRelease {
    /// Full DAT name ("Game (USA) (Rev 1)") or set display name.
    pub name: String,
    /// Region parsed from the DAT name, when it names one.
    pub region: Option<String>,
    /// Content CRC32 of the reference (0 for MAME sets — their
    /// reference is the set short name below).
    pub crc: u32,
    /// Content size in bytes (for sets: the sum of its ROMs).
    pub size: u64,
    /// MAME set short name for set-based systems.
    pub set_short: Option<String>,
    /// Variant notes with the region factored out (revision, disc,
    /// status, languages, flags, parent set) — the spreadsheet's
    /// Notes material.
    pub notes: String,
    /// Region preference rank (USA 0 < World 1 < Europe 2 < other
    /// 3 < unnamed 4) — display order only.
    #[serde(skip)]
    pub region_rank: u8,
}

/// One game: every known release of it, folded together.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefGroup {
    /// rom_group_key(system, any release name) — the shelf's key.
    pub group_key: String,
    /// Tag-stripped display title.
    pub base_title: String,
    pub releases: Vec<RefRelease>,
}

/// The reference universe of one system.
#[derive(Debug, Clone)]
pub struct ReferenceSystem {
    pub key: String,
    pub display: String,
    /// True when the units are MAME sets rather than DAT rows.
    pub uses_sets: bool,
    pub groups: Vec<RefGroup>,
    pub release_count: usize,
}

/// Every system's reference universe, in dataset order.
pub struct ReferenceIndex {
    pub systems: Vec<ReferenceSystem>,
}

fn release_notes(v: &RomVariant) -> String {
    let desc = v.descriptor.trim();
    if let Some(region) = &v.region {
        if let Some(rest) = desc.strip_prefix(region.as_str()) {
            return rest
                .trim_start_matches(|c| c == ' ' || c == '·')
                .trim()
                .to_string();
        }
    }
    desc.to_string()
}

impl ReferenceIndex {
    pub fn build(ds: &RomDataset) -> ReferenceIndex {
        let rows = ds.rows_by_system();
        let sets = ds.sets_by_system();
        let mut systems = Vec::with_capacity(ds.systems().len());
        for (idx, (key, display)) in ds.systems().iter().enumerate() {
            let sys_sets = sets.get(idx).cloned().unwrap_or_default();
            let uses_sets = !sys_sets.is_empty();
            let mut releases: Vec<RefRelease> = Vec::new();
            if uses_sets {
                for s in &sys_sets {
                    let v = variant_of_title(&s.display);
                    let mut notes = release_notes(&v);
                    if let Some(parent) = &s.parent {
                        let bit = format!("clone of {parent}");
                        notes = if notes.is_empty() {
                            bit
                        } else {
                            format!("{notes} · {bit}")
                        };
                    }
                    releases.push(RefRelease {
                        name: s.display.clone(),
                        region: v.region.clone(),
                        crc: 0,
                        size: s.roms.iter().map(|r| r.1).sum(),
                        set_short: Some(s.short.clone()),
                        notes,
                        region_rank: v.region_rank,
                    });
                }
            } else if let Some(sys_rows) = rows.get(idx) {
                for r in sys_rows {
                    let v = variant_of_title(&r.name);
                    releases.push(RefRelease {
                        name: r.name.clone(),
                        region: v.region.clone(),
                        crc: r.crc,
                        size: r.size,
                        set_short: None,
                        notes: release_notes(&v),
                        region_rank: v.region_rank,
                    });
                }
            }
            let release_count = releases.len();
            // Fold into title groups with the shelf's own key.
            let mut by_key: HashMap<String, usize> = HashMap::new();
            let mut groups: Vec<RefGroup> = Vec::new();
            for rel in releases {
                let gk = rom_group_key(key, &rel.name);
                match by_key.get(&gk) {
                    Some(&gi) => groups[gi].releases.push(rel),
                    None => {
                        by_key.insert(gk.clone(), groups.len());
                        groups.push(RefGroup {
                            base_title: parsed_base_title(&rel.name),
                            group_key: gk,
                            releases: vec![rel],
                        });
                    }
                }
            }
            for g in groups.iter_mut() {
                g.releases.sort_by(|a, b| {
                    a.region_rank
                        .cmp(&b.region_rank)
                        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                });
            }
            groups.sort_by(|a, b| {
                a.base_title
                    .to_lowercase()
                    .cmp(&b.base_title.to_lowercase())
                    .then_with(|| a.group_key.cmp(&b.group_key))
            });
            systems.push(ReferenceSystem {
                key: key.clone(),
                display: display.clone(),
                uses_sets,
                groups,
                release_count,
            });
        }
        ReferenceIndex { systems }
    }

    pub fn system(&self, key: &str) -> Option<&ReferenceSystem> {
        self.systems.iter().find(|s| s.key == key)
    }
}

// ---------------------------------------------------------------------
// Owned side + computation
// ---------------------------------------------------------------------

/// Owned title groups per system, keyed like the shelves.
pub type OwnedGroups = HashMap<String, HashSet<String>>;

/// Fold (system, DAT title) pairs into owned group keys — the one
/// place pair→group semantics live for this feature.
pub fn owned_group_keys(owned: &[(String, String)]) -> OwnedGroups {
    let mut out: OwnedGroups = HashMap::new();
    for (sys, title) in owned {
        out.entry(sys.clone())
            .or_default()
            .insert(rom_group_key(sys, title));
    }
    out
}

/// Per-system scoreboard row.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SystemSummary {
    pub key: String,
    pub display: String,
    /// False when the system's reference side is empty — no score
    /// and no list may be claimed for it (honesty clause).
    pub has_reference: bool,
    pub uses_sets: bool,
    pub total_groups: usize,
    pub owned_groups: usize,
    pub missing_groups: usize,
    pub total_releases: usize,
    /// Owned groups the reference doesn't know (identify ran
    /// against a different dataset vintage) — reported, never
    /// silently absorbed into the score.
    pub owned_unmatched: usize,
}

/// Score every dataset system against the owned set, in dataset
/// order. Systems absent from `owned` score zero — the empty-system
/// rule: they are reported, never hidden.
pub fn summarize(index: &ReferenceIndex, owned: &OwnedGroups) -> Vec<SystemSummary> {
    index
        .systems
        .iter()
        .map(|sys| {
            let mine = owned.get(&sys.key);
            let owned_here = mine.map(|s| s.len()).unwrap_or(0);
            let owned_matched = match mine {
                Some(set) => sys
                    .groups
                    .iter()
                    .filter(|g| set.contains(&g.group_key))
                    .count(),
                None => 0,
            };
            SystemSummary {
                key: sys.key.clone(),
                display: sys.display.clone(),
                has_reference: !sys.groups.is_empty(),
                uses_sets: sys.uses_sets,
                total_groups: sys.groups.len(),
                owned_groups: owned_matched,
                missing_groups: sys.groups.len() - owned_matched,
                total_releases: sys.release_count,
                owned_unmatched: owned_here - owned_matched,
            }
        })
        .collect()
}

/// The missing games of one system: reference groups with no owned
/// release, in title order, releases attached.
pub fn missing_groups(
    index: &ReferenceIndex,
    owned: &OwnedGroups,
    sys_key: &str,
) -> Vec<RefGroup> {
    let Some(sys) = index.system(sys_key) else {
        return Vec::new();
    };
    let mine = owned.get(sys_key);
    sys.groups
        .iter()
        .filter(|g| !mine.map(|s| s.contains(&g.group_key)).unwrap_or(false))
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------
// Spreadsheet export — the hunting list
// ---------------------------------------------------------------------

/// One spreadsheet row = one missing RELEASE (a title with three
/// regional releases is three rows; filter the Region column and
/// the list is exactly what to hunt). Columns as ruled:
/// System / Title / Region / Format wanted / Reference / Notes.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExportRow {
    pub system: String,
    pub title: String,
    pub region: String,
    pub format: String,
    pub reference: String,
    pub notes: String,
}

pub const EXPORT_HEADER: [&str; 6] = [
    "System",
    "Title",
    "Region",
    "Format wanted",
    "Reference",
    "Notes",
];

fn human_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let b = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if b < KIB * KIB {
        format!("{:.1} KiB", b / KIB)
    } else if b < KIB * KIB * KIB {
        format!("{:.1} MiB", b / (KIB * KIB))
    } else {
        format!("{:.2} GiB", b / (KIB * KIB * KIB))
    }
}

/// Build the export rows. `scope` = Some(system key) for one
/// system, None for every system with a reference set. Systems
/// without a reference set contribute NO rows (honesty clause —
/// there is no list to give).
pub fn export_rows(
    index: &ReferenceIndex,
    owned: &OwnedGroups,
    scope: Option<&str>,
    formats: &HashMap<String, String>,
) -> Vec<ExportRow> {
    let mut out = Vec::new();
    for sys in &index.systems {
        if scope.map(|s| s != sys.key.as_str()).unwrap_or(false) {
            continue;
        }
        if sys.groups.is_empty() {
            continue;
        }
        let format = formats
            .get(&sys.key)
            .cloned()
            .unwrap_or_else(|| "Any".to_string());
        let mine = owned.get(&sys.key);
        for g in &sys.groups {
            if mine.map(|s| s.contains(&g.group_key)).unwrap_or(false) {
                continue;
            }
            for rel in &g.releases {
                let reference = match &rel.set_short {
                    Some(short) => format!("MAME set {short}"),
                    None => format!("{} · CRC {:08X}", rel.name, rel.crc),
                };
                let mut notes = rel.notes.clone();
                if rel.size > 0 {
                    let sz = human_size(rel.size);
                    notes = if notes.is_empty() {
                        sz
                    } else {
                        format!("{notes} · {sz}")
                    };
                }
                out.push(ExportRow {
                    system: sys.display.clone(),
                    title: g.base_title.clone(),
                    region: rel.region.clone().unwrap_or_default(),
                    format: format.clone(),
                    reference,
                    notes,
                });
            }
        }
    }
    out
}

fn csv_cell(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Write rows as CSV (header + one line per row, CRLF endings).
/// Returns the number of data rows written.
pub fn write_csv<W: Write>(rows: &[ExportRow], mut w: W) -> std::io::Result<usize> {
    let mut line = EXPORT_HEADER.map(csv_cell).join(",");
    line.push_str("\r\n");
    w.write_all(line.as_bytes())?;
    for r in rows {
        let cells = [
            csv_cell(&r.system),
            csv_cell(&r.title),
            csv_cell(&r.region),
            csv_cell(&r.format),
            csv_cell(&r.reference),
            csv_cell(&r.notes),
        ];
        let mut line = cells.join(",");
        line.push_str("\r\n");
        w.write_all(line.as_bytes())?;
    }
    Ok(rows.len())
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            // XML 1.0 forbids most C0 controls even escaped.
            c if (c as u32) < 0x20 && c != '\t' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

fn sheet_xml(rows: &[ExportRow]) -> String {
    let mut x = String::with_capacity(1 << 16);
    x.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n");
    x.push_str("<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">");
    x.push_str(
        "<sheetViews><sheetView workbookViewId=\"0\">\
         <pane ySplit=\"1\" topLeftCell=\"A2\" activePane=\"bottomLeft\" state=\"frozen\"/>\
         <selection pane=\"bottomLeft\" activeCell=\"A2\" sqref=\"A2\"/>\
         </sheetView></sheetViews>",
    );
    x.push_str(
        "<cols>\
         <col min=\"1\" max=\"1\" width=\"22\" customWidth=\"1\"/>\
         <col min=\"2\" max=\"2\" width=\"52\" customWidth=\"1\"/>\
         <col min=\"3\" max=\"3\" width=\"16\" customWidth=\"1\"/>\
         <col min=\"4\" max=\"4\" width=\"15\" customWidth=\"1\"/>\
         <col min=\"5\" max=\"5\" width=\"58\" customWidth=\"1\"/>\
         <col min=\"6\" max=\"6\" width=\"40\" customWidth=\"1\"/>\
         </cols>",
    );
    x.push_str("<sheetData>");
    let cell = |x: &mut String, col: char, r: usize, style: Option<u8>, text: &str| {
        x.push_str(&format!("<c r=\"{col}{r}\""));
        if let Some(s) = style {
            x.push_str(&format!(" s=\"{s}\""));
        }
        x.push_str(&format!(
            " t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>",
            xml_escape(text)
        ));
    };
    x.push_str("<row r=\"1\">");
    for (i, h) in EXPORT_HEADER.iter().enumerate() {
        cell(&mut x, (b'A' + i as u8) as char, 1, Some(1), h);
    }
    x.push_str("</row>");
    for (i, r) in rows.iter().enumerate() {
        let rn = i + 2;
        x.push_str(&format!("<row r=\"{rn}\">"));
        let vals = [
            &r.system, &r.title, &r.region, &r.format, &r.reference, &r.notes,
        ];
        for (j, v) in vals.iter().enumerate() {
            cell(&mut x, (b'A' + j as u8) as char, rn, None, v);
        }
        x.push_str("</row>");
    }
    x.push_str("</sheetData>");
    if !rows.is_empty() {
        x.push_str(&format!(
            "<autoFilter ref=\"A1:F{}\"/>",
            rows.len() + 1
        ));
    }
    x.push_str("</worksheet>");
    x
}

/// Stored (uncompressed) ZIP archive — enough for XLSX, which is
/// a zip of XML parts. Deterministic: fixed DOS date 1980-01-01.
fn zip_stored(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    for (name, data) in entries {
        let crc = crc32fast::hash(data);
        let offset = out.len() as u32;
        let name_b = name.as_bytes();
        // Local file header.
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0x0800u16.to_le_bytes()); // UTF-8 names
        out.extend_from_slice(&0u16.to_le_bytes()); // stored
        out.extend_from_slice(&0u16.to_le_bytes()); // time
        out.extend_from_slice(&0x21u16.to_le_bytes()); // date 1980-01-01
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name_b.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra len
        out.extend_from_slice(name_b);
        out.extend_from_slice(data);
        // Central directory entry.
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes()); // made by
        central.extend_from_slice(&20u16.to_le_bytes()); // needed
        central.extend_from_slice(&0x0800u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0x21u16.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(name_b.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // extra
        central.extend_from_slice(&0u16.to_le_bytes()); // comment
        central.extend_from_slice(&0u16.to_le_bytes()); // disk
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        central.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name_b);
    }
    let cd_offset = out.len() as u32;
    let cd_len = central.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&cd_len.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// Write rows as a minimal, valid XLSX workbook (one sheet,
/// "Missing games", bold frozen header, autofilter). Returns the
/// number of data rows written.
pub fn write_xlsx<W: Write>(rows: &[ExportRow], mut w: W) -> std::io::Result<usize> {
    let content_types = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
        "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">",
        "<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>",
        "<Default Extension=\"xml\" ContentType=\"application/xml\"/>",
        "<Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/>",
        "<Override PartName=\"/xl/worksheets/sheet1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>",
        "<Override PartName=\"/xl/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml\"/>",
        "</Types>"
    );
    let rels = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
        "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
        "<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/>",
        "</Relationships>"
    );
    let workbook = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
        "<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" ",
        "xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">",
        "<sheets><sheet name=\"Missing games\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>"
    );
    let workbook_rels = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
        "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
        "<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet1.xml\"/>",
        "<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>",
        "</Relationships>"
    );
    let styles = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
        "<styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">",
        "<fonts count=\"2\"><font><sz val=\"11\"/><name val=\"Calibri\"/></font>",
        "<font><b/><sz val=\"11\"/><name val=\"Calibri\"/></font></fonts>",
        "<fills count=\"2\"><fill><patternFill patternType=\"none\"/></fill>",
        "<fill><patternFill patternType=\"gray125\"/></fill></fills>",
        "<borders count=\"1\"><border/></borders>",
        "<cellStyleXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/></cellStyleXfs>",
        "<cellXfs count=\"2\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\"/>",
        "<xf numFmtId=\"0\" fontId=\"1\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyFont=\"1\"/></cellXfs>",
        "</styleSheet>"
    );
    let sheet = sheet_xml(rows);
    let bytes = zip_stored(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", rels.as_bytes()),
        ("xl/workbook.xml", workbook.as_bytes()),
        ("xl/_rels/workbook.xml.rels", workbook_rels.as_bytes()),
        ("xl/styles.xml", styles.as_bytes()),
        ("xl/worksheets/sheet1.xml", sheet.as_bytes()),
    ]);
    w.write_all(&bytes)?;
    Ok(rows.len())
}

// ---------------------------------------------------------------------
// Tests — hand-built FABLEDAT1 fixture, hand-checkable math
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::romdat::RomDataset;

    struct FRow {
        sys: usize,
        name: &'static str,
        crc: u32,
        size: u64,
    }
    struct FSet {
        sys: usize,
        short: &'static str,
        display: &'static str,
        parent: Option<&'static str>,
        roms: Vec<(u32, u64)>,
    }

    fn intern(blob: &mut Vec<u8>, map: &mut HashMap<String, u32>, s: &str) -> u32 {
        if let Some(&off) = map.get(s) {
            return off;
        }
        let off = blob.len() as u32;
        blob.extend_from_slice(s.as_bytes());
        blob.push(0);
        map.insert(s.to_string(), off);
        off
    }

    /// Serialize a fixture dataset in the FABLEDAT1 layout
    /// RomDataset::load parses (44-byte header; systems blob;
    /// 28-byte rows; 20-byte sets; 12-byte setroms; names; shorts;
    /// empty md5/sha1 tables).
    fn build_dataset(
        systems: &[(&str, &str)],
        rows: &[FRow],
        sets: &[FSet],
    ) -> RomDataset {
        let mut sys_blob: Vec<u8> = Vec::new();
        for (k, d) in systems {
            sys_blob.push(k.len() as u8);
            sys_blob.extend_from_slice(k.as_bytes());
            sys_blob.push(d.len() as u8);
            sys_blob.extend_from_slice(d.as_bytes());
        }
        let mut names: Vec<u8> = Vec::new();
        let mut name_map: HashMap<String, u32> = HashMap::new();
        let mut shorts: Vec<u8> = Vec::new();
        let mut short_map: HashMap<String, u32> = HashMap::new();
        let mut setrom_count = 0usize;
        for s in sets {
            setrom_count += s.roms.len();
        }
        let total_len = 44
            + sys_blob.len()
            + rows.len() * 28
            + sets.len() * 20
            + setrom_count * 12
            + 0; // names/shorts appended below, accounted by construction
        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(b"FABLEDAT");
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&(systems.len() as u16).to_le_bytes());
        out.extend_from_slice(&(rows.len() as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // md5 count
        out.extend_from_slice(&0u32.to_le_bytes()); // sha1 count
        // names_len / shorts_len patched in after interning; reserve
        // their slots now (offsets 24 and 40), sys_len at 28.
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&(sys_blob.len() as u32).to_le_bytes());
        out.extend_from_slice(&(sets.len() as u32).to_le_bytes());
        out.extend_from_slice(&(setrom_count as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        debug_assert_eq!(out.len(), 44);
        out.extend_from_slice(&sys_blob);
        for r in rows {
            let noff = intern(&mut names, &mut name_map, r.name);
            out.extend_from_slice(&r.crc.to_le_bytes());
            out.extend_from_slice(&r.size.to_le_bytes());
            out.extend_from_slice(&noff.to_le_bytes());
            out.extend_from_slice(&u32::MAX.to_le_bytes());
            out.extend_from_slice(&u32::MAX.to_le_bytes());
            out.push(r.sys as u8);
            out.extend_from_slice(&[0, 0, 0]);
        }
        let mut setroms: Vec<u8> = Vec::new();
        let mut start = 0u32;
        for s in sets {
            let soff = intern(&mut shorts, &mut short_map, s.short);
            let doff = intern(&mut names, &mut name_map, s.display);
            let poff = match s.parent {
                Some(p) => intern(&mut shorts, &mut short_map, p),
                None => u32::MAX,
            };
            out.push(s.sys as u8);
            out.push(0);
            out.extend_from_slice(&(s.roms.len() as u16).to_le_bytes());
            out.extend_from_slice(&soff.to_le_bytes());
            out.extend_from_slice(&doff.to_le_bytes());
            out.extend_from_slice(&poff.to_le_bytes());
            out.extend_from_slice(&start.to_le_bytes());
            start += s.roms.len() as u32;
            for (crc, size) in &s.roms {
                setroms.extend_from_slice(&crc.to_le_bytes());
                setroms.extend_from_slice(&size.to_le_bytes());
            }
        }
        out.extend_from_slice(&setroms);
        out.extend_from_slice(&names);
        out.extend_from_slice(&shorts);
        // Patch names_len (offset 24) and shorts_len (offset 40).
        out[24..28].copy_from_slice(&(names.len() as u32).to_le_bytes());
        out[40..44].copy_from_slice(&(shorts.len() as u32).to_le_bytes());
        let _ = total_len;
        RomDataset::load(out).expect("fixture dataset loads")
    }

    fn fixture() -> (ReferenceIndex, OwnedGroups) {
        let ds = build_dataset(
            &[
                ("cart", "Test Cart"),
                ("disc", "Test Disc"),
                ("empty", "Test Empty"),
                ("arc", "Test Arcade"),
                ("noref", "Test NoRef"),
            ],
            &[
                FRow { sys: 0, name: "Alpha Game (USA)", crc: 0xAAAA_0001, size: 100 },
                FRow { sys: 0, name: "Alpha Game (Europe)", crc: 0xAAAA_0002, size: 100 },
                FRow { sys: 0, name: "Beta Quest (USA)", crc: 0xBBBB_0001, size: 200 },
                FRow { sys: 0, name: "Beta Quest (USA) (Rev 1)", crc: 0xBBBB_0002, size: 200 },
                FRow { sys: 0, name: "Gamma (Japan)", crc: 0xCCCC_0001, size: 300 },
                FRow { sys: 0, name: "Delta (World) (Demo)", crc: 0xDDDD_0001, size: 400 },
                FRow { sys: 0, name: "Sims 2, The - Pets (Europe)", crc: 0xEEEE_0001, size: 500 },
                FRow { sys: 1, name: "Solo (USA)", crc: 0x1111_0001, size: 500 },
                FRow { sys: 2, name: "One (USA)", crc: 0x0000_0001, size: 10 },
                FRow { sys: 2, name: "Two (USA)", crc: 0x0000_0002, size: 20 },
                FRow { sys: 2, name: "Two (Europe)", crc: 0x0000_0003, size: 20 },
            ],
            &[
                FSet { sys: 3, short: "alph", display: "Alpha Fighters (World)", parent: None, roms: vec![(0xA1, 1000), (0xA2, 2000)] },
                FSet { sys: 3, short: "alphj", display: "Alpha Fighters (Japan)", parent: Some("alph"), roms: vec![(0xA3, 3000)] },
                FSet { sys: 3, short: "zeta1", display: "Zeta (set 1)", parent: None, roms: vec![(0xB1, 4000)] },
                FSet { sys: 3, short: "zeta2", display: "Zeta (set 2)", parent: Some("zeta1"), roms: vec![(0xB2, 5000)] },
            ],
        );
        let index = ReferenceIndex::build(&ds);
        let owned = owned_group_keys(&[
            ("cart".into(), "Alpha Game (Europe)".into()),
            ("cart".into(), "Mystery (USA)".into()),
            ("disc".into(), "Solo (USA)".into()),
            ("arc".into(), "Alpha Fighters (Japan)".into()),
        ]);
        (index, owned)
    }

    #[test]
    fn summaries_are_hand_checkable() {
        let (index, owned) = fixture();
        let sums = summarize(&index, &owned);
        let by_key: HashMap<&str, &SystemSummary> =
            sums.iter().map(|s| (s.key.as_str(), s)).collect();
        let cart = by_key["cart"];
        assert_eq!(cart.total_groups, 5);
        assert_eq!(cart.owned_groups, 1);
        assert_eq!(cart.missing_groups, 4);
        assert_eq!(cart.total_releases, 7);
        assert_eq!(cart.owned_unmatched, 1);
        assert!(cart.has_reference && !cart.uses_sets);
        let disc = by_key["disc"];
        assert_eq!((disc.total_groups, disc.owned_groups, disc.missing_groups), (1, 1, 0));
        let empty = by_key["empty"];
        assert_eq!((empty.total_groups, empty.owned_groups, empty.missing_groups), (2, 0, 2));
        let arc = by_key["arc"];
        assert!(arc.uses_sets);
        assert_eq!((arc.total_groups, arc.owned_groups, arc.missing_groups), (2, 1, 1));
        assert_eq!(arc.total_releases, 4);
        let noref = by_key["noref"];
        assert!(!noref.has_reference);
        assert_eq!(noref.total_groups, 0);
    }

    #[test]
    fn missing_list_folds_variants_and_discovers_empty() {
        let (index, owned) = fixture();
        let cart = missing_groups(&index, &owned, "cart");
        let titles: Vec<&str> = cart.iter().map(|g| g.base_title.as_str()).collect();
        assert_eq!(titles, ["Beta Quest", "Delta", "Gamma", "Sims 2, The - Pets"]);
        let beta = &cart[0];
        assert_eq!(beta.releases.len(), 2);
        assert_eq!(beta.releases[0].name, "Beta Quest (USA)");
        assert_eq!(beta.releases[1].notes, "Rev 1");
        // The empty system reports its whole universe as missing.
        let empty = missing_groups(&index, &owned, "empty");
        assert_eq!(empty.len(), 2);
        // Unknown / reference-less systems yield no invented list.
        assert!(missing_groups(&index, &owned, "noref").is_empty());
        assert!(missing_groups(&index, &owned, "switch").is_empty());
        // Arcade: the Zeta group, both sets, short-name references.
        let arc = missing_groups(&index, &owned, "arc");
        assert_eq!(arc.len(), 1);
        assert_eq!(arc[0].base_title, "Zeta");
        assert_eq!(arc[0].releases.len(), 2);
        assert_eq!(arc[0].releases[0].set_short.as_deref(), Some("zeta1"));
    }

    #[test]
    fn export_rows_and_csv_are_exact() {
        let (index, owned) = fixture();
        let formats = HashMap::from([("cart".to_string(), "ZIP".to_string())]);
        let rows = export_rows(&index, &owned, Some("cart"), &formats);
        assert_eq!(
            rows,
            vec![
                ExportRow { system: "Test Cart".into(), title: "Beta Quest".into(), region: "USA".into(), format: "ZIP".into(), reference: "Beta Quest (USA) · CRC BBBB0001".into(), notes: "200 B".into() },
                ExportRow { system: "Test Cart".into(), title: "Beta Quest".into(), region: "USA".into(), format: "ZIP".into(), reference: "Beta Quest (USA) (Rev 1) · CRC BBBB0002".into(), notes: "Rev 1 · 200 B".into() },
                ExportRow { system: "Test Cart".into(), title: "Delta".into(), region: "World".into(), format: "ZIP".into(), reference: "Delta (World) (Demo) · CRC DDDD0001".into(), notes: "Demo · 400 B".into() },
                ExportRow { system: "Test Cart".into(), title: "Gamma".into(), region: "Japan".into(), format: "ZIP".into(), reference: "Gamma (Japan) · CRC CCCC0001".into(), notes: "300 B".into() },
                ExportRow { system: "Test Cart".into(), title: "Sims 2, The - Pets".into(), region: "Europe".into(), format: "ZIP".into(), reference: "Sims 2, The - Pets (Europe) · CRC EEEE0001".into(), notes: "500 B".into() },
            ]
        );
        let mut buf: Vec<u8> = Vec::new();
        let n = write_csv(&rows, &mut buf).unwrap();
        assert_eq!(n, 5);
        let text = String::from_utf8(buf).unwrap();
        let expected = concat!(
            "System,Title,Region,Format wanted,Reference,Notes\r\n",
            "Test Cart,Beta Quest,USA,ZIP,Beta Quest (USA) · CRC BBBB0001,200 B\r\n",
            "Test Cart,Beta Quest,USA,ZIP,Beta Quest (USA) (Rev 1) · CRC BBBB0002,Rev 1 · 200 B\r\n",
            "Test Cart,Delta,World,ZIP,Delta (World) (Demo) · CRC DDDD0001,Demo · 400 B\r\n",
            "Test Cart,Gamma,Japan,ZIP,Gamma (Japan) · CRC CCCC0001,300 B\r\n",
            "Test Cart,\"Sims 2, The - Pets\",Europe,ZIP,\"Sims 2, The - Pets (Europe) · CRC EEEE0001\",500 B\r\n",
        );
        assert_eq!(text, expected);
    }

    /// Minimal stored-zip reader for the round-trip: returns
    /// (name, crc, bytes) per local entry.
    fn unzip_stored(bytes: &[u8]) -> Vec<(String, u32, Vec<u8>)> {
        let mut out = Vec::new();
        let mut p = 0usize;
        while p + 30 <= bytes.len() && bytes[p..p + 4] == [0x50, 0x4b, 0x03, 0x04] {
            let crc = u32::from_le_bytes(bytes[p + 14..p + 18].try_into().unwrap());
            let size = u32::from_le_bytes(bytes[p + 18..p + 22].try_into().unwrap()) as usize;
            let nlen = u16::from_le_bytes(bytes[p + 26..p + 28].try_into().unwrap()) as usize;
            let elen = u16::from_le_bytes(bytes[p + 28..p + 30].try_into().unwrap()) as usize;
            let name = String::from_utf8_lossy(&bytes[p + 30..p + 30 + nlen]).into_owned();
            let start = p + 30 + nlen + elen;
            out.push((name, crc, bytes[start..start + size].to_vec()));
            p = start + size;
        }
        out
    }

    #[test]
    fn xlsx_round_trips_every_row() {
        let (index, owned) = fixture();
        let formats = HashMap::from([("cart".to_string(), "ZIP".to_string())]);
        let rows = export_rows(&index, &owned, Some("cart"), &formats);
        let mut buf: Vec<u8> = Vec::new();
        write_xlsx(&rows, &mut buf).unwrap();
        let entries = unzip_stored(&buf);
        assert_eq!(entries.len(), 6);
        for (name, crc, data) in &entries {
            assert_eq!(crc32fast::hash(data), *crc, "crc mismatch on {name}");
        }
        let sheet = entries
            .iter()
            .find(|(n, _, _)| n == "xl/worksheets/sheet1.xml")
            .map(|(_, _, d)| String::from_utf8_lossy(d).into_owned())
            .expect("sheet present");
        // Header + one <row> per export row.
        assert_eq!(sheet.matches("<row ").count(), rows.len() + 1);
        for r in &rows {
            assert!(sheet.contains(&xml_escape(&r.title)), "title {}", r.title);
            assert!(sheet.contains(&xml_escape(&r.reference)), "ref {}", r.reference);
        }
        assert!(sheet.contains("Format wanted"));
        // All-systems export: reference-less systems contribute
        // nothing; every other system's missing releases appear.
        let all = export_rows(&index, &owned, None, &formats);
        // cart 5 + empty 3 + arc 2 (Zeta sets) + disc 0 = 10.
        assert_eq!(all.len(), 10);
        assert!(all.iter().all(|r| r.system != "Test NoRef"));
        let zeta = all.iter().find(|r| r.title == "Zeta").unwrap();
        assert_eq!(zeta.reference, "MAME set zeta1");
    }
}
