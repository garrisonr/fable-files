# Theme package format

A theme is a folder with two required files and optional assets:

```
my-theme/
  theme.toml    # manifest (flat key/value, see below)
  tokens.toml   # semantic tokens (flat colors + [typography] + [shape] + density)
  wallpapers/   # optional
  icons/        # optional
  preview.png   # optional gallery preview
```

No executable code. Strict size/type validation on import; images decoded
safely. Import from file, drag-and-drop install, export, one-click rollback.

## theme.toml (manifest)

```toml
name = "Grizzette"
author = "Richard Garrison"
version = "0.1.0"
license = "MIT"
app_range = ">=0.1.0"   # semver range of compatible app versions
dark = false
description = "…"
```

## tokens.toml (semantic tokens)

Colors are `#RRGGBB` strings. Roles (spec §24):

| Token | Role |
|---|---|
| `surface` | navigation chrome background |
| `surface_elevated` | toolbars, status bars, dialogs |
| `content_canvas` | the file grid/list background (separate from chrome) |
| `text` / `text_muted` | primary / secondary text |
| `border` | hairlines, dividers |
| `selection` | selected-row background |
| `focus_ring` | keyboard focus indicator |
| `accent` / `accent_text` / `accent_dim` | actions; text on accent; large-art accent |
| `success` / `warning` / `error` / `info` | status roles |
| `[typography]` | `family_ui`, `family_mono`, `base_size_pt`, `line_height` |
| `[shape]` | `radius_sm/md/lg`, `shadow_strength` |
| `density` | `compact` \| `comfortable` \| `spacious` |

## Contrast rule

Every text pair (`text`/`surface`, `text_muted`/`surface`, `accent_text`/`accent`,
`text`/`content_canvas`) must reach **4.5:1** (WCAG AA). `theme_contrast_report`
names failing pairs and *suggests* a safe color — it never rewrites the
designer's choice silently. The theme editor shows the report live with
undo/compare/duplicate/reset.

## How the UI consumes themes

`ufm-themes` renders the token set to CSS custom properties (`--ufm-*`); the
shell installs them verbatim. If a surface ever needs a value with no token,
that's a spec bug — add a token, don't hard-code.
