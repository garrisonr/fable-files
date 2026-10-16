//! Mining ES-DE's bundled `es_systems.xml`.
//!
//! The vendored snapshot (`data/es_systems.xml`, from ES-DE's
//! `resources/systems/unix/`) is parsed lazily and cached. ES-DE lists
//! each system's launch commands in preference order — that order is
//! curation, and [`crate::table`] builds on it (the romm-tender
//! ADR-0020 pattern), with Fable's override layer on top.
//!
//! The parser is deliberately narrow: it reads exactly the elements
//! Fable consumes (`name`, `fullname`, `path`, `extension`,
//! `command` + its `label`, `platform`, `theme`) and ignores the
//! rest, so ES-DE schema additions never break the mine.

use serde::{Deserialize, Serialize};
use std::io::BufReader;

/// One `<command>` from a system definition: ES-DE's label and its
/// launch template, in document (preference) order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EsCommand {
    pub label: String,
    pub template: String,
}

/// One `<system>` from the mined file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EsSystem {
    pub name: String,
    pub fullname: String,
    pub path: String,
    /// Extensions as listed, split on whitespace (".nes", ".NES", …).
    pub extensions: Vec<String>,
    pub commands: Vec<EsCommand>,
    pub platform: String,
    pub theme: String,
}

/// Parse a whole `es_systems.xml` document. Tolerant by design: a
/// malformed system is skipped, never fatal — a mine that yields 194
/// of 195 systems beats one that yields none.
pub fn parse_es_systems(xml: &str) -> Vec<EsSystem> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_reader(BufReader::new(xml.as_bytes()));
    let mut out: Vec<EsSystem> = Vec::new();
    let mut cur: Option<EsSystem> = None;
    let mut field: Option<&'static str> = None;
    let mut cmd_label = String::new();
    let mut buf: Vec<u8> = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match e.name().as_ref() {
                "system" => {
                    cur = Some(EsSystem {
                        name: String::new(),
                        fullname: String::new(),
                        path: String::new(),
                        extensions: Vec::new(),
                        commands: Vec::new(),
                        platform: String::new(),
                        theme: String::new(),
                    });
                    field = None;
                }
                "name" => field = Some("name"),
                "fullname" => field = Some("fullname"),
                "path" => field = Some("path"),
                "extension" => field = Some("extension"),
                "platform" => field = Some("platform"),
                "theme" => field = Some("theme"),
                "command" => {
                    field = Some("command");
                    cmd_label.clear();
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == "label" {
                            if let Ok(v) = attr.unescape_value() {
                                cmd_label = v.into_owned();
                            }
                        }
                    }
                }
                _ => {}
            },
            Ok(Event::Text(e)) => {
                if let (Some(sys), Some(f)) = (cur.as_mut(), field) {
                    if let Ok(text) = quick_xml::escape::unescape(e.as_ref()) {
                        let text = text.as_ref();
                        match f {
                            "name" => sys.name.push_str(text),
                            "fullname" => sys.fullname.push_str(text),
                            "path" => sys.path.push_str(text),
                            "extension" => {
                                sys.extensions
                                    .extend(text.split_whitespace().map(|s| s.to_string()));
                            }
                            "platform" => sys.platform.push_str(text),
                            "theme" => sys.theme.push_str(text),
                            "command" => {
                                let t = text.trim().to_string();
                                if !t.is_empty() {
                                    sys.commands.push(EsCommand {
                                        label: cmd_label.clone(),
                                        template: t,
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            Ok(Event::End(e)) => match e.name().as_ref() {
                "system" => {
                    if let Some(sys) = cur.take() {
                        if !sys.name.is_empty() {
                            out.push(sys);
                        }
                    }
                    field = None;
                }
                "name" | "fullname" | "path" | "extension" | "platform" | "theme"
                | "command" => {
                    field = None;
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    // Element text can arrive with surrounding whitespace from the
    // pretty-printed source; normalize the scalar fields.
    for sys in &mut out {
        sys.name = sys.name.trim().to_string();
        sys.fullname = sys.fullname.trim().to_string();
        sys.path = sys.path.trim().to_string();
        sys.platform = sys.platform.trim().to_string();
        sys.theme = sys.theme.trim().to_string();
    }
    out
}

/// The vendored snapshot, parsed once.
pub fn vendored_systems() -> &'static [EsSystem] {
    static CACHE: std::sync::OnceLock<Vec<EsSystem>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| parse_es_systems(VENDORED_XML))
}

/// ES-DE's bundled systems file (unix variant), vendored at build
/// time. ES-DE is MIT-licensed; the About screen carries the
/// attribution. Refreshing this snapshot is a release-checklist chore
/// (SPEC-AMENDMENT §2).
pub static VENDORED_XML: &str = include_str!("../data/es_systems.xml");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendored_snapshot_mines_the_library_systems() {
        let systems = vendored_systems();
        assert!(
            systems.len() > 150,
            "expected the full ES-DE table, got {}",
            systems.len()
        );
        let nes = systems.iter().find(|s| s.name == "nes").expect("nes mined");
        assert_eq!(nes.fullname, "Nintendo Entertainment System");
        assert!(nes.extensions.iter().any(|e| e == ".nes"));
        // ES-DE's preference order is the curation: Mesen first.
        assert_eq!(nes.commands.first().map(|c| c.label.as_str()), Some("Mesen"));
        assert!(nes
            .commands
            .first()
            .unwrap()
            .template
            .contains("mesen_libretro.so"));
        let snes = systems.iter().find(|s| s.name == "snes").expect("snes mined");
        assert_eq!(
            snes.commands.first().map(|c| c.label.as_str()),
            Some("Snes9x - Current")
        );
        assert!(snes
            .commands
            .first()
            .unwrap()
            .template
            .contains("snes9x_libretro.so"));
    }

    #[test]
    fn parser_tolerates_a_small_document() {
        let xml = r#"<?xml version="1.0"?>
<systemList>
  <system>
    <name>testsys</name>
    <fullname>Test System</fullname>
    <path>%ROMPATH%/testsys</path>
    <extension>.bin .BIN</extension>
    <command label="Only">run %ROM%</command>
    <platform>testsys</platform>
    <theme>testsys</theme>
  </system>
</systemList>"#;
        let v = parse_es_systems(xml);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "testsys");
        assert_eq!(v[0].extensions, vec![".bin".to_string(), ".BIN".to_string()]);
        assert_eq!(v[0].commands.len(), 1);
        assert_eq!(v[0].commands[0].label, "Only");
    }
}
