//! datc — compile No-Intro / Redump / MAME DAT files (as redistributed by
//! libretro-database, CC BY-SA 4.0) into Fable Files' compact ROM dataset
//! (rom-dats.bin, format FABLEDAT1).
//!
//! The DATs in libretro-database's metadat/ + dat/ are clrmamepro TEXT
//! format (not XML):
//!   game (
//!       name "..."
//!       rom ( name "..." size N crc XXXXXXXX md5 .. sha1 .. )
//!   )
//! The one XML source is the MAME 2016 listxml (Arcade Only), mined for
//! per-machine ROM sets (arcade + the Neo Geo family via romof="neogeo").
//!
//! Output sections: header, systems blob, file-rows (sorted by crc,size),
//! names blob, md5/sha1 side tables (only for rows whose (crc,size) key is
//! ambiguous — unique keys don't need the extra hashes), sets + set-roms
//! (MAME-style zip sets matched by filename stem + contained CRCs).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write as _;

#[derive(Clone, Debug)]
struct FileRow {
    sys: u8,
    crc: u32,
    size: u64,
    md5: Option<[u8; 16]>,
    sha1: Option<[u8; 20]>,
    name: String,
}

#[derive(Clone, Debug)]
struct SetRow {
    sys: u8,
    short: String,
    display: String,
    parent: Option<String>, // romof
    roms: Vec<(u32, u64)>,  // (crc, size), own roms only
}

struct SysDef {
    key: &'static str,
    display: &'static str,
    files: &'static [&'static str],
}

const NI: &str = "metadat/no-intro";
const RD: &str = "metadat/redump";
const MM: &str = "metadat/mame";
const DT: &str = "dat";
const TS: &str = "metadat/tosec";

fn systems() -> Vec<SysDef> {
    vec![
        SysDef { key: "3do", display: "3DO", files: &[RD] },
        SysDef { key: "amiga", display: "Amiga", files: &[NI] },
        SysDef { key: "arcade", display: "Arcade (MAME)", files: &[MM] },
        SysDef { key: "atari2600", display: "Atari 2600", files: &[NI] },
        SysDef { key: "atari5200", display: "Atari 5200", files: &[NI] },
        SysDef { key: "atari7800", display: "Atari 7800", files: &[NI] },
        SysDef { key: "atari8bit", display: "Atari 8-Bit", files: &[NI] },
        SysDef { key: "lynx", display: "Atari Lynx", files: &[NI] },
        SysDef { key: "colecovision", display: "ColecoVision", files: &[NI] },
        SysDef { key: "c64", display: "Commodore 64", files: &[NI] },
        SysDef { key: "dreamcast", display: "Dreamcast", files: &[RD] },
        SysDef { key: "gb", display: "Game Boy", files: &[NI] },
        SysDef { key: "gba", display: "Game Boy Advance", files: &[NI] },
        SysDef { key: "gbc", display: "Game Boy Color", files: &[NI] },
        SysDef { key: "gamegear", display: "Game Gear", files: &[NI] },
        SysDef { key: "gamecube", display: "GameCube", files: &[RD] },
        SysDef { key: "genesis", display: "Genesis", files: &[NI] },
        SysDef { key: "intellivision", display: "Intellivision", files: &[NI] },
        SysDef { key: "jaguar", display: "Atari Jaguar", files: &[NI] },
        SysDef { key: "mastersystem", display: "Master System", files: &[NI] },
        SysDef { key: "msx", display: "MSX", files: &[NI] },
        SysDef { key: "neogeo", display: "Neo Geo", files: &[] },
        SysDef { key: "neogeopocket", display: "Neo Geo Pocket", files: &[NI] },
        SysDef { key: "nes", display: "Nintendo Entertainment System", files: &[NI] },
        SysDef { key: "n3ds", display: "Nintendo 3DS", files: &[NI] },
        SysDef { key: "n64", display: "Nintendo 64", files: &[NI] },
        SysDef { key: "nds", display: "Nintendo DS", files: &[NI] },
        SysDef { key: "psx", display: "PlayStation", files: &[RD] },
        SysDef { key: "ps2", display: "PlayStation 2", files: &[RD] },
        SysDef { key: "ps3", display: "PlayStation 3", files: &[RD] },
        SysDef { key: "psp", display: "PlayStation Portable", files: &[NI, RD] },
        SysDef { key: "saturn", display: "Saturn", files: &[RD] },
        SysDef { key: "sega32x", display: "Sega 32X", files: &[NI] },
        SysDef { key: "segacd", display: "Sega CD", files: &[RD] },
        SysDef { key: "snes", display: "Super Nintendo Entertainment System", files: &[NI] },
        SysDef { key: "tg16", display: "TurboGrafx-16", files: &[NI, RD] },
        SysDef { key: "vectrex", display: "Vectrex", files: &[NI] },
        SysDef { key: "virtualboy", display: "Virtual Boy", files: &[NI] },
        SysDef { key: "wii", display: "Wii", files: &[RD] },
        SysDef { key: "wiiu", display: "Wii U", files: &[DT] },
        SysDef { key: "wonderswan", display: "WonderSwan", files: &[NI] },
        SysDef { key: "xbox", display: "Xbox", files: &[RD] },
        SysDef { key: "xbox360", display: "Xbox 360", files: &[RD] },
        SysDef { key: "zxspectrum", display: "ZX Spectrum", files: &[DT] },
        // ROM organization pass (2026-10-15): systems Dickles owns whose
        // DATs were in libretro-database all along but never compiled in.
        // Appended so existing system indices never move.
        SysDef { key: "odyssey2", display: "Odyssey2", files: &[NI] },
        SysDef { key: "channelf", display: "Channel F", files: &[NI] },
        SysDef { key: "studio2", display: "Studio II", files: &[NI] },
        SysDef { key: "arcadia2001", display: "Arcadia 2001", files: &[NI] },
        SysDef { key: "scv", display: "Super Cassette Vision", files: &[NI] },
        SysDef { key: "pv1000", display: "PV-1000", files: &[NI] },
        SysDef { key: "superacan", display: "Super A'can", files: &[NI] },
        SysDef { key: "sg1000", display: "SG-1000", files: &[NI] },
        SysDef { key: "vic20", display: "VIC-20", files: &[NI] },
        SysDef { key: "gamecom", display: "Game.com", files: &[NI] },
        SysDef { key: "psvita", display: "PS Vita", files: &[NI] },
        SysDef { key: "cdi", display: "CD-i", files: &[RD] },
        SysDef { key: "pcfx", display: "PC-FX", files: &[RD] },
        SysDef { key: "neogeocd", display: "Neo Geo CD", files: &[RD] },
        SysDef { key: "amigacd32", display: "Amiga CD32", files: &[RD] },
        SysDef { key: "gx4000", display: "GX4000", files: &[TS] },
    ]
}

/// Concrete DAT filename per (folder, system display-ish stem). The folder
/// constants above are coarse; this table is the exact mapping.
fn dat_files(key: &str) -> Vec<(String, &'static str)> {
    let ni = |n: &str| (format!("{NI}/{n}"), "No-Intro (via libretro-database)");
    let rd = |n: &str| (format!("{RD}/{n}"), "Redump (via libretro-database)");
    let mm = |n: &str| (format!("{MM}/{n}"), "MAME (via libretro-database)");
    let dt = |n: &str| (format!("{DT}/{n}"), "No-Intro combined (libretro-database)");
    let ts = |n: &str| (format!("{TS}/{n}"), "TOSEC (via libretro-database)");
    match key {
        "3do" => vec![rd("The 3DO Company - 3DO.dat")],
        "amiga" => vec![ni("Commodore - Amiga.dat")],
        "arcade" => vec![mm("MAME.dat")],
        "atari2600" => vec![ni("Atari - 2600.dat")],
        "atari5200" => vec![ni("Atari - 5200.dat")],
        "atari7800" => vec![ni("Atari - 7800.dat")],
        "atari8bit" => vec![ni("Atari - 8-bit Family.dat")],
        "lynx" => vec![ni("Atari - Lynx.dat")],
        "colecovision" => vec![ni("Coleco - ColecoVision.dat")],
        "c64" => vec![ni("Commodore - 64.dat")],
        "dreamcast" => vec![rd("Sega - Dreamcast.dat")],
        "gb" => vec![ni("Nintendo - Game Boy.dat")],
        "gba" => vec![ni("Nintendo - Game Boy Advance.dat")],
        "gbc" => vec![ni("Nintendo - Game Boy Color.dat")],
        "gamegear" => vec![ni("Sega - Game Gear.dat")],
        "gamecube" => vec![rd("Nintendo - GameCube.dat")],
        "genesis" => vec![ni("Sega - Mega Drive - Genesis.dat")],
        "intellivision" => vec![ni("Mattel - Intellivision.dat")],
        "jaguar" => vec![ni("Atari - Jaguar.dat"), rd("Atari - Jaguar CD.dat")],
        "mastersystem" => vec![ni("Sega - Master System - Mark III.dat")],
        "msx" => vec![ni("Microsoft - MSX.dat"), ni("Microsoft - MSX2.dat")],
        "neogeo" => vec![],
        "neogeopocket" => vec![
            ni("SNK - Neo Geo Pocket.dat"),
            ni("SNK - Neo Geo Pocket Color.dat"),
        ],
        "nes" => vec![ni("Nintendo - Nintendo Entertainment System.dat")],
        "n3ds" => vec![ni("Nintendo - Nintendo 3DS.dat")],
        "n64" => vec![ni("Nintendo - Nintendo 64.dat")],
        "nds" => vec![ni("Nintendo - Nintendo DS.dat")],
        "psx" => vec![rd("Sony - PlayStation.dat")],
        "ps2" => vec![rd("Sony - PlayStation 2.dat")],
        "ps3" => vec![rd("Sony - PlayStation 3.dat")],
        "psp" => vec![
            rd("Sony - PlayStation Portable.dat"),
            ni("Sony - PlayStation Portable.dat"),
        ],
        "saturn" => vec![rd("Sega - Saturn.dat")],
        "sega32x" => vec![ni("Sega - 32X.dat")],
        "segacd" => vec![rd("Sega - Mega-CD - Sega CD.dat")],
        "snes" => vec![ni("Nintendo - Super Nintendo Entertainment System.dat")],
        "tg16" => vec![
            ni("NEC - PC Engine - TurboGrafx 16.dat"),
            rd("NEC - PC Engine CD - TurboGrafx-CD.dat"),
        ],
        "vectrex" => vec![ni("GCE - Vectrex.dat")],
        "virtualboy" => vec![ni("Nintendo - Virtual Boy.dat")],
        "wii" => vec![rd("Nintendo - Wii.dat")],
        "wiiu" => vec![dt("Nintendo - Wii U.dat")],
        "wonderswan" => vec![
            ni("Bandai - WonderSwan.dat"),
            ni("Bandai - WonderSwan Color.dat"),
        ],
        "xbox" => vec![rd("Microsoft - Xbox.dat")],
        "xbox360" => vec![rd("Microsoft - Xbox 360.dat")],
        "zxspectrum" => vec![dt("Sinclair - ZX Spectrum.dat")],
        "odyssey2" => vec![
            ni("Magnavox - Odyssey2.dat"),
            ni("Philips - Videopac+.dat"),
        ],
        "channelf" => vec![ni("Fairchild - Channel F.dat")],
        "studio2" => vec![ni("RCA - Studio II.dat")],
        "arcadia2001" => vec![ni("Emerson - Arcadia 2001.dat")],
        "scv" => vec![ni("Epoch - Super Cassette Vision.dat")],
        "pv1000" => vec![ni("Casio - PV-1000.dat")],
        "superacan" => vec![ni("Funtech - Super Acan.dat")],
        "sg1000" => vec![ni("Sega - SG-1000.dat")],
        "vic20" => vec![ni("Commodore - VIC-20.dat")],
        "gamecom" => vec![ni("Tiger - Game.com.dat")],
        // The Vita (PSN) DAT (17k loose .pkg entries) is deliberately
        // NOT compiled in — see AUDIT.md; the base DAT is the library.
        "psvita" => vec![ni("Sony - PlayStation Vita.dat")],
        "cdi" => vec![rd("Philips - CD-i.dat")],
        "pcfx" => vec![rd("NEC - PC-FX.dat")],
        "neogeocd" => vec![rd("SNK - Neo Geo CD.dat")],
        "amigacd32" => vec![rd("Commodore - CD32.dat")],
        "gx4000" => vec![ts("Amstrad - GX4000.dat")],
        _ => vec![],
    }
}

fn parse_hex_bytes(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let b = s.as_bytes();
    for i in (0..b.len()).step_by(2) {
        let hi = (b[i] as char).to_digit(16)?;
        let lo = (b[i + 1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}

/// Parse the value following `key` inside a clrmamepro `( ... )` block:
/// either a quoted string (with \" \-escapes) or a bare token. Purely
/// byte-based: never slices the haystack except at ASCII boundaries
/// (the matched key bytes), so multibyte UTF-8 in names can't panic it.
fn clr_value(block: &str, key: &str) -> Option<String> {
    let bytes = block.as_bytes();
    let kb = key.as_bytes();
    if bytes.len() < kb.len() {
        return None;
    }
    let mut i = 0usize;
    while i + kb.len() <= bytes.len() {
        let boundary_before =
            i == 0 || !(bytes[i - 1] as char).is_ascii_alphanumeric() && bytes[i - 1] != b'_';
        if boundary_before && &bytes[i..i + kb.len()] == kb {
            let mut j = i + kb.len();
            // key must be followed by whitespace or a quote (not more word)
            if j < bytes.len()
                && !(bytes[j] as char).is_whitespace()
                && bytes[j] != b'"'
            {
                i += 1;
                continue;
            }
            while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'"' {
                let mut out: Vec<u8> = Vec::new();
                let mut k = j + 1;
                while k < bytes.len() {
                    match bytes[k] {
                        b'\\' if k + 1 < bytes.len() => {
                            out.push(bytes[k + 1]);
                            k += 2;
                        }
                        b'"' => {
                            return Some(String::from_utf8_lossy(&out).into_owned())
                        }
                        c => {
                            out.push(c);
                            k += 1;
                        }
                    }
                }
                return Some(String::from_utf8_lossy(&out).into_owned());
            }
            // bare token
            let mut k = j;
            while k < bytes.len()
                && !(bytes[k] as char).is_whitespace()
                && bytes[k] != b')'
                && bytes[k] != b'('
            {
                k += 1;
            }
            if k > j {
                return Some(String::from_utf8_lossy(&bytes[j..k]).into_owned());
            }
            return None;
        }
        i += 1;
    }
    None
}

/// Parse a clrmamepro DAT into (game_name, roms) — roms as
/// (name, size, crc, md5, sha1).
fn parse_clr(
    text: &str,
) -> Vec<(String, Vec<(String, u64, u32, Option<[u8; 16]>, Option<[u8; 20]>)>)> {
    let mut games = Vec::new();
    let bytes = text.as_bytes();
    let mut pos = 0usize;
    while let Some(rel) = find_sub(&text[pos..], "game (") {
        let start = pos + rel;
        // Block runs to the matching close paren. Quote-aware: parens
        // inside quoted names (e.g. "(World)") must not move the depth.
        let mut depth = 0i32;
        let mut end = None;
        let mut i = start;
        let mut in_q = false;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' if in_q => {
                    i += 1;
                }
                b'"' => in_q = !in_q,
                b'(' if !in_q => depth += 1,
                b')' if !in_q => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i);
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let Some(end) = end else { break };
        let block = &text[start..end];
        pos = end + 1;
        // game name = first `name` before the first "rom ("
        let head_end = block.find("rom (").unwrap_or(block.len());
        let head = &block[..head_end];
        let Some(gname) = clr_value(head, "name") else {
            continue;
        };
        let mut roms = Vec::new();
        let mut rpos = 0usize;
        while let Some(rrel) = find_sub(&block[rpos..], "rom (") {
            let rstart = rpos + rrel;
            // rom block ends at its closing paren — quote-aware, since
            // rom names themselves contain parens ("(World)", "(Track 01)").
            let rb = block.as_bytes();
            let mut rend = None;
            let mut in_q = false;
            let mut j = rstart;
            while j < rb.len() {
                match rb[j] {
                    b'\\' if in_q => {
                        j += 1;
                    }
                    b'"' => in_q = !in_q,
                    b')' if !in_q => {
                        rend = Some(j);
                        break;
                    }
                    _ => {}
                }
                j += 1;
            }
            let Some(rend) = rend else { break };
            let rblock = &block[rstart..rend];
            rpos = rend + 1;
            if rblock.contains("nodump") {
                continue;
            }
            let (Some(rname), Some(size_s), Some(crc_s)) = (
                clr_value(rblock, "name"),
                clr_value(rblock, "size"),
                clr_value(rblock, "crc"),
            ) else {
                continue;
            };
            let Ok(size) = size_s.parse::<u64>() else {
                continue;
            };
            let Some(crc) = u32::from_str_radix(crc_s.trim(), 16).ok() else {
                continue;
            };
            if size == 0 {
                continue;
            }
            let md5 = clr_value(rblock, "md5")
                .and_then(|s| parse_hex_bytes(&s))
                .and_then(|v| <[u8; 16]>::try_from(v.as_slice()).ok());
            let sha1 = clr_value(rblock, "sha1")
                .and_then(|s| parse_hex_bytes(&s))
                .and_then(|v| <[u8; 20]>::try_from(v.as_slice()).ok());
            roms.push((rname, size, crc, md5, sha1));
        }
        if !roms.is_empty() {
            games.push((gname, roms));
        }
    }
    games
}

fn find_sub(hay: &str, needle: &str) -> Option<usize> {
    hay.find(needle)
}

fn xml_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let semi = rest[amp..].find(';').map(|i| amp + i);
        let Some(semi) = semi else {
            out.push_str(&rest[amp..]);
            return out;
        };
        let ent = &rest[amp + 1..semi];
        match ent {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            _ if ent.starts_with("#x") => {
                if let Ok(n) = u32::from_str_radix(&ent[2..], 16) {
                    if let Some(c) = char::from_u32(n) {
                        out.push(c);
                    }
                }
            }
            _ if ent.starts_with('#') => {
                if let Ok(n) = ent[1..].parse::<u32>() {
                    if let Some(c) = char::from_u32(n) {
                        out.push(c);
                    }
                }
            }
            _ => {
                out.push('&');
                out.push_str(ent);
                out.push(';');
            }
        }
        rest = &rest[semi + 1..];
    }
    out.push_str(rest);
    out
}

fn xml_attr(tag: &str, key: &str) -> Option<String> {
    let pat = format!("{key}=\"");
    let start = tag.find(&pat)? + pat.len();
    let end = tag[start..].find('"')? + start;
    Some(xml_unescape(&tag[start..end]))
}

/// Parse the MAME listxml for machine sets. Returns all non-device
/// machines with at least one crc'd rom.
fn parse_mame_xml(text: &str) -> Vec<SetRowRaw> {
    let mut sets = Vec::new();
    let mut pos = 0usize;
    while let Some(rel) = text[pos..].find("<machine ") {
        let start = pos + rel;
        let Some(tag_end) = text[start..].find('>') else {
            break;
        };
        let tag = &text[start..start + tag_end];
        let Some(close) = text[start..].find("</machine>") else {
            break;
        };
        let block = &text[start..start + close];
        pos = start + close + 10;
        if xml_attr(tag, "isdevice").as_deref() == Some("yes") {
            continue;
        }
        let Some(name) = xml_attr(tag, "name") else {
            continue;
        };
        let desc = block
            .find("<description>")
            .and_then(|d| {
                let s = d + "<description>".len();
                block[s..].find("</description>").map(|e| xml_unescape(&block[s..s + e]))
            })
            .unwrap_or_else(|| name.clone());
        let mut roms = Vec::new();
        let mut rpos = 0usize;
        while let Some(rrel) = block[rpos..].find("<rom ") {
            let rstart = rpos + rrel;
            let Some(rend) = block[rstart..].find("/>") else {
                break;
            };
            let rtag = &block[rstart..rstart + rend];
            rpos = rstart + rend + 2;
            if xml_attr(rtag, "status").as_deref() == Some("nodump") {
                continue;
            }
            let (Some(crc_s), Some(size_s)) = (xml_attr(rtag, "crc"), xml_attr(rtag, "size"))
            else {
                continue;
            };
            let (Ok(crc), Ok(size)) = (
                u32::from_str_radix(crc_s.trim(), 16),
                size_s.parse::<u64>(),
            ) else {
                continue;
            };
            if size == 0 {
                continue;
            }
            roms.push((crc, size));
        }
        if roms.is_empty() {
            continue;
        }
        roms.sort();
        roms.dedup();
        sets.push(SetRowRaw {
            short: name,
            display: desc,
            romof: xml_attr(tag, "romof"),
            cloneof: xml_attr(tag, "cloneof"),
            sourcefile: xml_attr(tag, "sourcefile").unwrap_or_default(),
            roms,
        });
    }
    sets
}

struct SetRowRaw {
    short: String,
    display: String,
    romof: Option<String>,
    cloneof: Option<String>,
    sourcefile: String,
    roms: Vec<(u32, u64)>,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if std::env::var("DATC_DEBUG").is_ok() {
        let text = std::fs::read_to_string(&args[1]).unwrap();
        let blocks = text.matches("game (").count();
        let games = parse_clr(&text);
        println!("blocks={blocks} games_with_roms={}", games.len());
        for (n, roms) in games.iter().take(3) {
            println!("game {n:?} roms={roms:?}");
        }
        return;
    }
    let lrd = args.get(1).map(|s| s.as_str()).unwrap_or(".");
    let out_path = args.get(2).map(|s| s.as_str()).unwrap_or("rom-dats.bin");
    let manifest_path = args.get(3).map(|s| s.as_str()).unwrap_or("DAT-MANIFEST.md");

    let sysdefs = systems();
    let mut rows: Vec<FileRow> = Vec::new();
    let mut manifest = String::new();
    manifest.push_str("# Fable Files ROM dataset — DAT manifest\n\n");
    manifest.push_str("Compiled by datc from DAT files redistributed by the libretro-database project ");
    manifest.push_str("(repository license: Creative Commons Attribution-ShareAlike 4.0). The dataset ");
    manifest.push_str("carries only factual reference data — game names, file sizes and hashes ");
    manifest.push_str("(CRC32/MD5/SHA1) — published by the No-Intro, Redump, MAME and TOSEC projects for ");
    manifest.push_str("exactly this purpose (collection scanning). No ROM content is included, ever.\n\n");
    manifest.push_str("| Fable system | key | DAT file | source | games | rom rows |\n|---|---|---|---|---|---|\n");

    for (idx, sd) in sysdefs.iter().enumerate() {
        for (rel, source) in dat_files(sd.key) {
            let path = format!("{lrd}/{rel}");
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {path}: {e}"));
            let games = parse_clr(&text);
            let mut nrows = 0usize;
            for (gname, roms) in &games {
                for (_rname, size, crc, md5, sha1) in roms {
                    rows.push(FileRow {
                        sys: idx as u8,
                        crc: *crc,
                        size: *size,
                        md5: *md5,
                        sha1: *sha1,
                        name: gname.clone(),
                    });
                    nrows += 1;
                }
            }
            manifest.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                sd.display,
                sd.key,
                rel.rsplit('/').next().unwrap_or(&rel),
                source,
                games.len(),
                nrows
            ));
            eprintln!("{} {}: {} games", sd.key, rel, games.len());
        }
    }

    // MAME XML sets (arcade = all machines; neogeo = the Neo Geo family).
    let xml_path = format!("{MM}/MAME 2016 XML (Arcade Only).xml");
    let xml_full = format!("{lrd}/{xml_path}");
    let xml_text = std::fs::read_to_string(&xml_full)
        .unwrap_or_else(|e| panic!("read {xml_full}: {e}"));
    let machines = parse_mame_xml(&xml_text);
    let arcade_idx = sysdefs.iter().position(|s| s.key == "arcade").unwrap() as u8;
    let neogeo_idx = sysdefs.iter().position(|s| s.key == "neogeo").unwrap() as u8;
    // Neo Geo family: seed with direct markers, then grow through
    // cloneof chains to a fixpoint (clones don't restate romof).
    let mut neo_names: HashSet<String> = HashSet::new();
    for m in &machines {
        if m.romof.as_deref() == Some("neogeo")
            || m.short == "neogeo"
            || m.sourcefile == "neogeo.cpp"
        {
            neo_names.insert(m.short.clone());
        }
    }
    loop {
        let mut grew = false;
        for m in &machines {
            if let Some(parent) = &m.cloneof {
                if neo_names.contains(parent) && !neo_names.contains(&m.short) {
                    neo_names.insert(m.short.clone());
                    grew = true;
                }
            }
        }
        if !grew {
            break;
        }
    }
    let mut sets: Vec<SetRow> = Vec::new();
    let mut neo_count = 0usize;
    for m in &machines {
        sets.push(SetRow {
            sys: arcade_idx,
            short: m.short.clone(),
            display: m.display.clone(),
            parent: m.romof.clone(),
            roms: m.roms.clone(),
        });
        let is_neo = neo_names.contains(&m.short);
        if is_neo {
            neo_count += 1;
            sets.push(SetRow {
                sys: neogeo_idx,
                short: m.short.clone(),
                display: m.display.clone(),
                parent: m.romof.clone(),
                roms: m.roms.clone(),
            });
        }
    }
    manifest.push_str(&format!(
        "| Arcade (MAME) sets | arcade | MAME 2016 XML (Arcade Only).xml | MAME listxml (via libretro-database) | {} | — |\n",
        machines.len()
    ));
    manifest.push_str(&format!(
        "| Neo Geo sets | neogeo | (same XML, romof=neogeo family) | MAME listxml (via libretro-database) | {} | — |\n",
        neo_count
    ));
    eprintln!("mame xml: {} machines, {} neogeo", machines.len(), neo_count);

    // Dedup file rows within (sys, crc, size, name).
    rows.sort_by(|a, b| (a.sys, a.crc, a.size, &a.name).cmp(&(b.sys, b.crc, b.size, &b.name)));
    rows.dedup_by(|a, b| a.sys == b.sys && a.crc == b.crc && a.size == b.size && a.name == b.name);
    let raw_rows = rows.len();

    // Ambiguity: (crc,size) groups with >1 DISTINCT row (different sys or
    // name) keep md5/sha1 in the side tables; unique rows drop hashes.
    let mut groups: HashMap<(u32, u64), usize> = HashMap::new();
    for r in &rows {
        *groups.entry((r.crc, r.size)).or_default() += 1;
    }
    let mut md5s: Vec<[u8; 16]> = Vec::new();
    let mut sha1s: Vec<[u8; 20]> = Vec::new();
    let mut md5_idx: Vec<u32> = Vec::with_capacity(rows.len());
    let mut sha1_idx: Vec<u32> = Vec::with_capacity(rows.len());
    let mut ambiguous = 0usize;
    for r in &rows {
        if groups.get(&(r.crc, r.size)).copied().unwrap_or(0) > 1 {
            ambiguous += 1;
            md5_idx.push(match r.md5 {
                Some(h) => {
                    md5s.push(h);
                    (md5s.len() - 1) as u32
                }
                None => u32::MAX,
            });
            sha1_idx.push(match r.sha1 {
                Some(h) => {
                    sha1s.push(h);
                    (sha1s.len() - 1) as u32
                }
                None => u32::MAX,
            });
        } else {
            md5_idx.push(u32::MAX);
            sha1_idx.push(u32::MAX);
        }
    }

    // Names blob (dedup by string → offset).
    let mut name_offs: HashMap<String, u32> = HashMap::new();
    let mut names_blob: Vec<u8> = Vec::new();
    // Names are NUL-terminated in the blob so the loader can slice them
    // by offset alone.
    let mut name_off = |name: &str, blob: &mut Vec<u8>, map: &mut HashMap<String, u32>| -> u32 {
        if let Some(o) = map.get(name) {
            return *o;
        }
        let off = blob.len() as u32;
        blob.extend_from_slice(name.as_bytes());
        blob.push(0);
        map.insert(name.to_string(), off);
        off
    };
    let mut row_name_off: Vec<u32> = Vec::with_capacity(rows.len());
    for r in &rows {
        row_name_off.push(name_off(&r.name, &mut names_blob, &mut name_offs));
    }

    // Sort rows by (crc, size, sys) for binary search.
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by(|&a, &b| {
        (rows[a].crc, rows[a].size, rows[a].sys).cmp(&(rows[b].crc, rows[b].size, rows[b].sys))
    });

    // Systems blob: per sys: u8 key_len, key, u8 disp_len, disp.
    let mut sys_blob: Vec<u8> = Vec::new();
    for sd in &sysdefs {
        sys_blob.push(sd.key.len() as u8);
        sys_blob.extend_from_slice(sd.key.as_bytes());
        sys_blob.push(sd.display.len() as u8);
        sys_blob.extend_from_slice(sd.display.as_bytes());
    }

    // Sets: sort by (sys, short); set-roms blob.
    sets.sort_by(|a, b| (a.sys, &a.short).cmp(&(b.sys, &b.short)));
    sets.dedup_by(|a, b| a.sys == b.sys && a.short == b.short);
    let mut set_roms: Vec<(u32, u64)> = Vec::new();
    let mut set_meta: Vec<(u8, u32, u32, u32, u32, u16)> = Vec::new(); // sys, short_off, disp_off, parent_off(MAX), rom_start, rom_count
    let mut shorts_blob: Vec<u8> = Vec::new();
    let mut short_offs: HashMap<String, u32> = HashMap::new();
    for s in &sets {
        let d_off = name_off(&s.display, &mut names_blob, &mut name_offs);
        let s_off = name_off(&s.short, &mut shorts_blob, &mut short_offs);
        let p_off = match &s.parent {
            Some(p) => name_off(p, &mut shorts_blob, &mut short_offs),
            None => u32::MAX,
        };
        let start = set_roms.len() as u32;
        set_roms.extend_from_slice(&s.roms);
        set_meta.push((
            s.sys,
            s_off,
            d_off,
            p_off,
            start,
            s.roms.len().min(u16::MAX as usize) as u16,
        ));
    }

    // Serialize.
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(b"FABLEDAT");
    out.extend_from_slice(&1u16.to_le_bytes()); // version
    out.extend_from_slice(&(sysdefs.len() as u16).to_le_bytes());
    out.extend_from_slice(&(rows.len() as u32).to_le_bytes());
    out.extend_from_slice(&(md5s.len() as u32).to_le_bytes());
    out.extend_from_slice(&(sha1s.len() as u32).to_le_bytes());
    out.extend_from_slice(&(names_blob.len() as u32).to_le_bytes());
    out.extend_from_slice(&(sys_blob.len() as u32).to_le_bytes());
    out.extend_from_slice(&(sets.len() as u32).to_le_bytes());
    out.extend_from_slice(&(set_roms.len() as u32).to_le_bytes());
    out.extend_from_slice(&(shorts_blob.len() as u32).to_le_bytes());
    out.extend_from_slice(&sys_blob);
    for (oi, &ri) in order.iter().enumerate() {
        let r = &rows[ri];
        out.extend_from_slice(&r.crc.to_le_bytes());
        out.extend_from_slice(&r.size.to_le_bytes());
        out.extend_from_slice(&row_name_off[ri].to_le_bytes());
        out.extend_from_slice(&md5_idx[ri].to_le_bytes());
        out.extend_from_slice(&sha1_idx[ri].to_le_bytes());
        out.push(r.sys);
        out.push(0u8);
        out.extend_from_slice(&0u16.to_le_bytes());
        let _ = oi;
    }
    // Sets metadata (20B each): sys u8, pad u8, rom_count u16, short_off u32, disp_off u32, parent_off u32, rom_start u32
    for (sys, s_off, d_off, p_off, start, count) in &set_meta {
        out.push(*sys);
        out.push(0u8);
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&s_off.to_le_bytes());
        out.extend_from_slice(&d_off.to_le_bytes());
        out.extend_from_slice(&p_off.to_le_bytes());
        out.extend_from_slice(&start.to_le_bytes());
    }
    for (crc, size) in &set_roms {
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
    }
    out.extend_from_slice(&names_blob);
    out.extend_from_slice(&shorts_blob);
    for h in &md5s {
        out.extend_from_slice(h);
    }
    for h in &sha1s {
        out.extend_from_slice(h);
    }

    std::fs::write(out_path, &out).unwrap();
    manifest.push_str(&format!(
        "\n## Totals\n\n- file rows (deduped): {raw_rows}\n- rows in ambiguous (crc,size) groups (hashes kept): {ambiguous}\n- md5 side entries: {}\n- sha1 side entries: {}\n- unique names: {}\n- name blob: {} bytes\n- sets: {} (set-rom entries: {})\n- **rom-dats.bin: {} bytes**\n",
        md5s.len(),
        sha1s.len(),
        name_offs.len(),
        names_blob.len(),
        sets.len(),
        set_roms.len(),
        out.len()
    ));
    let mut mf = std::fs::File::create(manifest_path).unwrap();
    mf.write_all(manifest.as_bytes()).unwrap();
    eprintln!(
        "rows={} ambiguous={} md5={} sha1={} names={} sets={} bin={}",
        raw_rows,
        ambiguous,
        md5s.len(),
        sha1s.len(),
        name_offs.len(),
        sets.len(),
        out.len()
    );
    // Silence unused warning for BTreeMap import path (kept for debugging).
    let _ = BTreeMap::<u8, u8>::new();
    let _ = HashSet::<u8>::new();
}
