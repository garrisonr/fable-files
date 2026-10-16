//! ES-DE launch-template expansion (amendment §2).
//!
//! Templates are data Fable owns; expansion mirrors ES-DE's
//! variables. `%ROM%` is always the game's REAL catalog path — the
//! unified view never appears here (prime directive). Expansion is
//! string-first (multi-word emulator commands like
//! `flatpak run org.libretro.RetroArch` substitute cleanly), then a
//! quote-aware tokenizer produces argv — there is no shell anywhere
//! in the launch path, so a ROM path with spaces or quotes in it is
//! just data.

use std::collections::HashMap;

/// Everything a template can reference.
#[derive(Debug, Clone, Default)]
pub struct LaunchVars {
    /// The real path of the game file being launched.
    pub rom: String,
    /// RetroArch's cores directory for the detected flavor.
    pub core_dir: String,
    /// The unified-view root (%ROMPATH%) — only templates for
    /// external consumers use it; Fable's own never do.
    pub rom_path_root: String,
    /// `%EMULATOR_<NAME>%` resolutions, keyed by the full token name
    /// ("EMULATOR_RETROARCH", "EMULATOR_MESEN", …). Values are
    /// command lines (may be multi-word, e.g. a flatpak invocation).
    pub emulators: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expanded {
    pub argv: Vec<String>,
    /// `%STARTDIR%=<dir>` prefix, when the template carries one.
    pub cwd: Option<String>,
}

fn basename_of(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

fn stem_of(path: &str) -> String {
    let base = basename_of(path);
    match base.rfind('.') {
        Some(i) if i > 0 => base[..i].to_string(),
        _ => base,
    }
}

fn dir_of(path: &str) -> String {
    match path.rfind('/') {
        Some(0) => "/".to_string(),
        Some(i) => path[..i].to_string(),
        None => ".".to_string(),
    }
}

/// Wrap a value for the tokenizer: quoted when it contains anything
/// a space-split would break. Escapes embedded double quotes.
fn quoted(value: &str) -> String {
    if value.is_empty()
        || value
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '\'')
    {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        value.to_string()
    }
}

/// Quote-aware tokenizer: whitespace splits, double/single quotes
/// group (and are stripped), backslash escapes the next character
/// inside double quotes.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_token = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_token = true;
                while let Some(c2) = chars.next() {
                    match c2 {
                        '"' => break,
                        '\\' => {
                            if let Some(c3) = chars.next() {
                                cur.push(c3);
                            }
                        }
                        _ => cur.push(c2),
                    }
                }
            }
            '\'' => {
                in_token = true;
                for c2 in chars.by_ref() {
                    if c2 == '\'' {
                        break;
                    }
                    cur.push(c2);
                }
            }
            c if c.is_whitespace() => {
                if in_token {
                    out.push(std::mem::take(&mut cur));
                    in_token = false;
                }
            }
            _ => {
                in_token = true;
                cur.push(c);
            }
        }
    }
    if in_token {
        out.push(cur);
    }
    out
}

/// Expand a template into argv (+ optional working directory).
pub fn expand_template(template: &str, vars: &LaunchVars) -> Expanded {
    let mut text = template.trim().to_string();
    let mut cwd = None;
    // %STARTDIR%=<dir> is a prefix directive, not an argument.
    if let Some(rest) = text.strip_prefix("%STARTDIR%=") {
        let rest = rest.trim_start();
        // The directory runs to the first unquoted space.
        let toks = tokenize(rest);
        if let Some((first, _)) = toks.first().map(|t| (t.clone(), ())) {
            cwd = Some(first);
            // Remove the consumed token (plus its original quoting)
            // from the raw text: find the end of the first token by
            // re-scanning the raw string.
            let raw = rest;
            let mut idx = 0;
            let bytes = raw.as_bytes();
            if idx < bytes.len() && (bytes[idx] == b'"' || bytes[idx] == b'\'') {
                let q = bytes[idx];
                idx += 1;
                while idx < bytes.len() && bytes[idx] != q {
                    idx += 1;
                }
                idx += 1; // closing quote
            } else {
                while idx < bytes.len() && !bytes[idx].is_ascii_whitespace() {
                    idx += 1;
                }
            }
            text = raw[idx..].trim_start().to_string();
        }
    }

    let rom_q = quoted(&vars.rom);
    let mut out = text;
    // Order matters: the RAW forms before the quoted forms would
    // corrupt them (%ROM% is a prefix of nothing here, but %ROMRAW%
    // must not be eaten by a %ROM% pass — replace longest first).
    out = out.replace("%ROMRAW%", &vars.rom);
    out = out.replace("%ROM%", &rom_q);
    out = out.replace("%BASENAME%", &quoted(&stem_of(&vars.rom)));
    out = out.replace("%FILENAME%", &quoted(&basename_of(&vars.rom)));
    out = out.replace("%GAMEDIRRAW%", &dir_of(&vars.rom));
    out = out.replace("%GAMEDIR%", &quoted(&dir_of(&vars.rom)));
    out = out.replace("%ROMPATH%", &quoted(&vars.rom_path_root));
    out = out.replace("%CORE_RETROARCH%", &quoted(&vars.core_dir));
    // %EMULATOR_<NAME>% tokens: substitute each known resolution.
    // Unknown emulators stay literal — the launch will fail plainly
    // rather than silently launching the wrong thing.
    for (name, cmd) in &vars.emulators {
        out = out.replace(&format!("%{name}%"), cmd);
    }
    Expanded {
        argv: tokenize(&out),
        cwd,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> LaunchVars {
        let mut emulators = HashMap::new();
        emulators.insert(
            "EMULATOR_RETROARCH".to_string(),
            "flatpak run org.libretro.RetroArch".to_string(),
        );
        emulators.insert("EMULATOR_MESEN".to_string(), "mesen".to_string());
        LaunchVars {
            rom: "/media/richard/Desk SSD/Games/1942 (Japan, USA).nes".to_string(),
            core_dir: "/home/richard/.var/app/org.libretro.RetroArch/config/retroarch/cores"
                .to_string(),
            rom_path_root: "/home/richard/ROMs".to_string(),
            emulators,
        }
    }

    #[test]
    fn core_template_expands_to_flatpak_argv() {
        let e = expand_template(
            "%EMULATOR_RETROARCH% -L %CORE_RETROARCH%/mesen_libretro.so %ROM%",
            &vars(),
        );
        assert_eq!(
            e.argv,
            vec![
                "flatpak",
                "run",
                "org.libretro.RetroArch",
                "-L",
                "/home/richard/.var/app/org.libretro.RetroArch/config/retroarch/cores/mesen_libretro.so",
                "/media/richard/Desk SSD/Games/1942 (Japan, USA).nes",
            ]
        );
        assert_eq!(e.cwd, None);
    }

    #[test]
    fn standalone_template_and_gamedir() {
        let e = expand_template("%EMULATOR_MESEN% --fullscreen %ROM%", &vars());
        assert_eq!(
            e.argv,
            vec![
                "mesen",
                "--fullscreen",
                "/media/richard/Desk SSD/Games/1942 (Japan, USA).nes"
            ]
        );
        let e2 = expand_template("mame -rompath %GAMEDIR% nes -cart %BASENAME%", &vars());
        assert_eq!(e2.argv[2], "/media/richard/Desk SSD/Games");
        assert_eq!(e2.argv[5], "1942 (Japan, USA)");
    }

    #[test]
    fn quoted_labels_survive_tokenizing() {
        let toks = tokenize("ares --fullscreen --system \"Super Famicom\" game.sfc");
        assert_eq!(
            toks,
            vec!["ares", "--fullscreen", "--system", "Super Famicom", "game.sfc"]
        );
    }
}
