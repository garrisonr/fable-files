//! ufm-themes: the theme engine.
//!
//! Spec §24, **P0 and non-negotiable**: every visual decision is expressed
//! through documented semantic tokens. No hard-coded production colors,
//! spacing, icons, or fonts may exist outside the token system — the theme
//! conformance test (`every_shipped_theme_is_contrast_clean` in this
//! crate's suite) loads every `themes/*` package and fails the build on
//! any contrast issue, so no surface ships on an unverified theme.
//!
//! Theme packages are portable data: manifest + tokens + optional assets.
//! No executable code, strict validation, one-click rollback.

use serde::{Deserialize, Serialize};
use ufm_core::{Result, ThemeId, UfmError};

// ---------------------------------------------------------------------------
// Manifest — identity and compatibility of one theme package.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeManifest {
    /// Assigned by the theme service on install — not authored in theme.toml.
    #[serde(default = "ThemeId::new")]
    pub id: ThemeId,
    pub name: String,
    pub author: String,
    pub version: String,
    pub license: String,
    /// Semver range of app versions this theme is built for.
    pub app_range: String,
    pub dark: bool,
    pub description: String,
}

// ---------------------------------------------------------------------------
// Semantic tokens — the complete visual vocabulary. Roles (not raw colors)
// so light, dark, high-contrast, and community themes stay coherent.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSet {
    pub surface: Color,
    pub surface_elevated: Color,
    pub content_canvas: Color,
    pub text: Color,
    pub text_muted: Color,
    pub border: Color,
    pub selection: Color,
    pub focus_ring: Color,
    pub accent: Color,
    pub accent_text: Color,
    pub accent_dim: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub info: Color,
    pub typography: Typography,
    pub shape: Shape,
    pub density: Density,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(transparent)]
pub struct Color(pub [u8; 3]);

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let hex = s.strip_prefix('#').ok_or_else(|| {
            serde::de::Error::custom(format!("color {s:?} must be #RRGGBB"))
        })?;
        if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(serde::de::Error::custom(format!(
                "color {s:?} must be #RRGGBB"
            )));
        }
        let n = u32::from_str_radix(hex, 16)
            .map_err(|_| serde::de::Error::custom(format!("bad hex {s:?}")))?;
        Ok(Color::hex(n))
    }
}

impl Color {
    pub const fn hex(hex: u32) -> Self {
        Self([
            ((hex >> 16) & 0xff) as u8,
            ((hex >> 8) & 0xff) as u8,
            (hex & 0xff) as u8,
        ])
    }

    /// WCAG relative luminance.
    pub fn luminance(&self) -> f64 {
        let f = |c: u8| {
            let s = c as f64 / 255.0;
            if s <= 0.03928 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * f(self.0[0]) + 0.7152 * f(self.0[1]) + 0.0722 * f(self.0[2])
    }

    /// WCAG contrast ratio between two colors.
    pub fn contrast_against(&self, other: &Color) -> f64 {
        let (a, b) = (self.luminance(), other.luminance());
        let (hi, lo) = if a > b { (a, b) } else { (b, a) };
        (hi + 0.05) / (lo + 0.05)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Typography {
    pub family_ui: String,
    pub family_mono: String,
    pub base_size_pt: f32,
    pub line_height: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Shape {
    pub radius_sm: f32,
    pub radius_md: f32,
    pub radius_lg: f32,
    pub shadow_strength: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Density {
    Compact,
    Comfortable,
    Spacious,
}

// ---------------------------------------------------------------------------
// Validation — schema + accessibility. A theme that fails contrast does not
// install silently: the report names the failing pair and suggests a safe
// text color *without overwriting the designer's choice* (spec §24).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContrastIssue {
    pub pair: (String, String),
    pub ratio: f64,
    pub required: f64,
    pub suggested_fix: String,
}

/// One gated pair: foreground token, background token, WCAG floor.
struct ContrastPair {
    fg: &'static str,
    bg: &'static str,
    required: f64,
}

const fn pair(fg: &'static str, bg: &'static str, required: f64) -> ContrastPair {
    ContrastPair { fg, bg, required }
}

/// The full contrast gate (design-checks P1, 2026-10-16 — widened from
/// the original four pairs after the audit in
/// `~/workspace/design-checks/INVESTIGATION.md` found the default theme
/// failing seven pairs no test computed). Text-grade pairs sit at the
/// WCAG AA 4.5:1 bar; `focus_ring` is a non-text indicator and sits at
/// the WCAG 1.4.11 3:1 bar. Status roles are text (labels, badges), so
/// they are held to 4.5 like everything else that is read.
const CONTRAST_PAIRS: &[ContrastPair] = &[
    // The original mandated four.
    pair("text", "surface", 4.5),
    pair("text_muted", "surface", 4.5),
    pair("accent_text", "accent", 4.5),
    pair("text", "content_canvas", 4.5),
    // Chrome text actually lives on the elevated surface.
    pair("text", "surface_elevated", 4.5),
    pair("text_muted", "surface_elevated", 4.5),
    // Selected rows render text on the selection wash.
    pair("text", "selection", 4.5),
    // Status roles on both reading surfaces.
    pair("success", "surface", 4.5),
    pair("warning", "surface", 4.5),
    pair("error", "surface", 4.5),
    pair("info", "surface", 4.5),
    pair("success", "content_canvas", 4.5),
    pair("warning", "content_canvas", 4.5),
    pair("error", "content_canvas", 4.5),
    pair("info", "content_canvas", 4.5),
    // The accent is used as link text and icon marks on surfaces.
    pair("accent", "surface", 4.5),
    pair("accent", "content_canvas", 4.5),
    // Focus indicator: non-text bar, on every surface it can ring.
    pair("focus_ring", "surface", 3.0),
    pair("focus_ring", "content_canvas", 3.0),
    pair("focus_ring", "surface_elevated", 3.0),
];

/// Tokens deliberately outside the gate. The exempt list is CODE, with
/// the reason attached — never folklore (design-checks P1). A token that
/// needs out of the matrix gets a named entry here, or it gets checked.
pub const CONTRAST_EXEMPTIONS: &[(&str, &str)] = &[(
    "accent_dim",
    "large art / highlights only — never text, icons, or any mark that carries meaning (spec §24)",
)];

fn token_color<'a>(tokens: &'a TokenSet, name: &str) -> &'a Color {
    match name {
        "surface" => &tokens.surface,
        "surface_elevated" => &tokens.surface_elevated,
        "content_canvas" => &tokens.content_canvas,
        "text" => &tokens.text,
        "text_muted" => &tokens.text_muted,
        "border" => &tokens.border,
        "selection" => &tokens.selection,
        "focus_ring" => &tokens.focus_ring,
        "accent" => &tokens.accent,
        "accent_text" => &tokens.accent_text,
        "accent_dim" => &tokens.accent_dim,
        "success" => &tokens.success,
        "warning" => &tokens.warning,
        "error" => &tokens.error,
        "info" => &tokens.info,
        other => unreachable!("unknown token {other}"),
    }
}

pub fn check_contrast(tokens: &TokenSet) -> Vec<ContrastIssue> {
    let mut issues = Vec::new();
    for p in CONTRAST_PAIRS {
        let fg = token_color(tokens, p.fg);
        let bg = token_color(tokens, p.bg);
        let ratio = fg.contrast_against(bg);
        if ratio < p.required {
            // Point the suggestion at whichever side has room to move:
            // deepen the darker side or raise the lighter one.
            let suggested_fix = if fg.luminance() > bg.luminance() {
                format!(
                    "raise {} luminance or deepen {} until ratio ≥ {:.1}:1",
                    p.fg, p.bg, p.required
                )
            } else {
                format!(
                    "deepen {} or raise {} luminance until ratio ≥ {:.1}:1",
                    p.fg, p.bg, p.required
                )
            };
            issues.push(ContrastIssue {
                pair: (p.fg.into(), p.bg.into()),
                ratio,
                required: p.required,
                suggested_fix,
            });
        }
    }
    issues
}

// ---------------------------------------------------------------------------
// Theme package — manifest + tokens (+ optional assets on disk).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemePackage {
    pub manifest: ThemeManifest,
    pub tokens: TokenSet,
}

/// Sanitize a font-family list for CSS interpolation. Only characters that
/// cannot break out of the declaration are kept (letters, digits, space,
/// `-_,'"` and commas); `;{}()/` and friends are stripped. A value that
/// sanitizes to nothing falls back to `fallback`. Community themes are a
/// product pillar, so theme-authored strings are untrusted input at this
/// boundary.
fn sanitize_font_family(raw: &str, fallback: &str) -> String {
    let clean: String = raw
        .chars()
        .filter(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, ' ' | '_' | '-' | ',' | '\'' | '"')
        })
        .collect();
    let clean = clean.trim();
    if clean.is_empty() {
        fallback.to_string()
    } else {
        clean.to_string()
    }
}

impl ThemePackage {
    /// Load from a theme directory (`theme.toml` + `tokens.toml`).
    pub fn load_from_dir(dir: &std::path::Path) -> Result<Self> {
        let read = |name: &str| {
            std::fs::read_to_string(dir.join(name))
                .map_err(|e| UfmError::Theme(format!("cannot read {name}: {e}")))
        };
        let manifest: ThemeManifest = toml::from_str(&read("theme.toml")?)
            .map_err(|e| UfmError::Theme(format!("bad theme.toml: {e}")))?;
        let tokens: TokenSet = toml::from_str(&read("tokens.toml")?)
            .map_err(|e| UfmError::Theme(format!("bad tokens.toml: {e}")))?;
        Ok(Self { manifest, tokens })
    }
    /// CSS custom properties for the web shell — the ONLY styling input
    /// the UI is allowed to consume.
    pub fn to_css_variables(&self) -> String {
        let t = &self.tokens;
        let c = |col: &Color| format!("#{:02X}{:02X}{:02X}", col.0[0], col.0[1], col.0[2]);
        format!(
            ":root {{\n\
             --ufm-surface: {surface};\n\
             --ufm-surface-elevated: {surface_elevated};\n\
             --ufm-content-canvas: {content_canvas};\n\
             --ufm-text: {text};\n\
             --ufm-text-muted: {text_muted};\n\
             --ufm-border: {border};\n\
             --ufm-selection: {selection};\n\
             --ufm-focus-ring: {focus_ring};\n\
             --ufm-accent: {accent};\n\
             --ufm-accent-text: {accent_text};\n\
             --ufm-accent-dim: {accent_dim};\n\
             --ufm-success: {success};\n\
             --ufm-warning: {warning};\n\
             --ufm-error: {error};\n\
             --ufm-info: {info};\n\
             --ufm-font-ui: {font_ui};\n\
             --ufm-font-mono: {font_mono};\n\
             --ufm-radius-md: {radius}px;\n\
             }}\n",
            surface = c(&t.surface),
            surface_elevated = c(&t.surface_elevated),
            content_canvas = c(&t.content_canvas),
            text = c(&t.text),
            text_muted = c(&t.text_muted),
            border = c(&t.border),
            selection = c(&t.selection),
            focus_ring = c(&t.focus_ring),
            accent = c(&t.accent),
            accent_text = c(&t.accent_text),
            accent_dim = c(&t.accent_dim),
            success = c(&t.success),
            warning = c(&t.warning),
            error = c(&t.error),
            info = c(&t.info),
            font_ui = sanitize_font_family(&t.typography.family_ui, "system-ui, sans-serif"),
            font_mono = sanitize_font_family(&t.typography.family_mono, "ui-monospace, monospace"),
            radius = t.shape.radius_md,
        )
    }
}

/// The built-in Grizzette direction from the spec: peach paper, chocolate ink,
/// orange-red action, icy-blue support — with the contrast-fixed accessible
/// accent (#A83500, verified ≥ 4.5:1 on paper).
pub fn grizzette_tokens() -> TokenSet {
    TokenSet {
        surface: Color::hex(0xFFDDB5),          // peachy cream paper
        surface_elevated: Color::hex(0xFFE9D2),
        content_canvas: Color::hex(0xFFF6EA),
        text: Color::hex(0x6B3816),             // chocolate ink
        text_muted: Color::hex(0x8A5A2E),
        border: Color::hex(0xD9B98C),
        selection: Color::hex(0xCAE7FC),        // icy blue
        focus_ring: Color::hex(0x1C6980),       // deep bondi (contrast-fixed)
        accent: Color::hex(0xA83500),           // accessible orange-red
        accent_text: Color::hex(0xFFFFFF),
        accent_dim: Color::hex(0xFB4E00),       // vivid accent for large art
        success: Color::hex(0x296E2C),          // deepened: ≥4.5 on peach
        warning: Color::hex(0x8C5300),          // deepened: ≥4.5 on peach
        error: Color::hex(0xB3261E),
        info: Color::hex(0x1C6980),             // deep bondi, = focus_ring
        typography: Typography {
            family_ui: "system-ui, sans-serif".into(),
            family_mono: "\"Victor Mono\", ui-monospace, monospace".into(),
            base_size_pt: 11.0,
            line_height: 1.45,
        },
        shape: Shape {
            radius_sm: 4.0,
            radius_md: 8.0,
            radius_lg: 14.0,
            shadow_strength: 0.18,
        },
        density: Density::Comfortable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grizzette_passes_contrast() {
        let issues = check_contrast(&grizzette_tokens());
        assert!(
            issues.is_empty(),
            "grizzette must ship contrast-clean: {issues:?}"
        );
    }

    #[test]
    fn font_sanitizer_keeps_legitimate_stacks() {
        assert_eq!(
            sanitize_font_family("system-ui, sans-serif", "fallback"),
            "system-ui, sans-serif"
        );
        assert_eq!(
            sanitize_font_family("\"Victor Mono\", ui-monospace, monospace", "fallback"),
            "\"Victor Mono\", ui-monospace, monospace"
        );
        assert_eq!(
            sanitize_font_family("-apple-system, 'Segoe UI', sans-serif", "fallback"),
            "-apple-system, 'Segoe UI', sans-serif"
        );
    }

    #[test]
    fn font_sanitizer_neutralizes_css_breakout() {
        // Every breakout character is stripped; the remainder cannot close
        // the declaration or open a new rule.
        let evil = "sans-serif; } body { display:none; } /*";
        let clean = sanitize_font_family(evil, "fallback");
        assert!(
            !clean.chars().any(|c| matches!(c, ';' | '{' | '}' | '(' | ')' | '/')),
            "breakout survived: {clean:?}"
        );
        // And a value that is *only* hostile falls back safely.
        assert_eq!(sanitize_font_family(";{}()/", "fallback"), "fallback");
        assert_eq!(sanitize_font_family("   ", "fallback"), "fallback");
    }

    #[test]
    fn css_variables_cannot_carry_font_injection() {
        let mut tokens = grizzette_tokens();
        tokens.typography.family_ui =
            "x\"; } .pwned { color:red; } /*".to_string();
        let pkg = ThemePackage {
            manifest: ThemeManifest {
                id: ThemeId::new(),
                name: "evil".into(),
                author: "evil".into(),
                version: "0".into(),
                license: "evil".into(),
                app_range: "*".into(),
                dark: false,
                description: "evil".into(),
            },
            tokens,
        };
        let css = pkg.to_css_variables();
        // The hostile payload must not survive into the stylesheet: strip
        // the legitimate trailing semicolon, then no rule syntax may remain.
        for line in css.lines().filter(|l| l.contains("--ufm-font-")) {
            let body = line.trim_end().strip_suffix(';').unwrap_or(line);
            assert!(
                !body.chars().any(|c| matches!(c, ';' | '{' | '}')),
                "breakout in {line:?}"
            );
        }
        assert!(!css.contains(".pwned"));
    }

    #[test]
    fn grizzette_package_loads_and_passes_contrast() {
        // The real theme package on disk must parse against the schema and
        // stay contrast-clean — guards against TOML drift.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../themes/grizzette");
        let pkg = ThemePackage::load_from_dir(&dir).expect("grizzette theme loads");
        assert_eq!(pkg.manifest.name, "Grizzette");
        let issues = check_contrast(&pkg.tokens);
        assert!(issues.is_empty(), "packaged grizzette must ship contrast-clean: {issues:?}");
        // The CSS export is the shell's only styling input — it must render.
        let css = pkg.to_css_variables();
        assert!(css.contains("--ufm-surface: #FFDDB5"));
        assert!(css.contains("--ufm-accent: #A83500"));
    }

    #[test]
    fn every_shipped_theme_is_contrast_clean() {
        // The conformance test the crate header mandates (design-checks
        // P1): load EVERY package in themes/ and run the full gate.
        // Completeness is enforced at parse time — TokenSet has no serde
        // defaults, so a package missing a token fails to load — and a
        // theme dropped into the folder is covered with no test edit.
        // Before this test, fable-dark and fable-light were loaded by no
        // test at all and grizzette failed seven unchecked pairs.
        let themes_dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../themes");
        let mut names = Vec::new();
        for entry in std::fs::read_dir(&themes_dir).expect("themes/ directory readable") {
            let dir = entry.expect("theme entry").path();
            if !dir.join("tokens.toml").exists() {
                continue;
            }
            let pkg = ThemePackage::load_from_dir(&dir).unwrap_or_else(|e| {
                panic!("theme at {} must load complete: {e}", dir.display())
            });
            let issues = check_contrast(&pkg.tokens);
            assert!(
                issues.is_empty(),
                "theme {} fails the contrast gate: {issues:?}",
                pkg.manifest.name
            );
            names.push(pkg.manifest.name);
        }
        // Guard against a vacuous pass: the shipped set must be found.
        assert!(
            names.len() >= 3,
            "expected the shipped themes, found {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "Grizzette"),
            "grizzette missing from themes/: {names:?}"
        );
    }
}
