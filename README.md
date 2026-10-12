# Fable Files

**Working title · v0.1 scaffold · 12 October 2026**

A fast, beautiful, local-first file environment for every screen — from phone to TV —
without the usual Linux assembly kit.

> Product thesis (the whole point): *with just this file manager you could do tons of
> things with no other software needed.*

The full product/feature specification lives at
`../your_files/fable-files-spec/fable-files-spec.pdf`
(34 pages, v0.1, 12 Oct 2026). This repo is the **v0.1 code scaffold**: the Rust
workspace, the Tauri v2 shell, the domain crates, the theme package format with the
Grizzette theme, and the docs the second-pass reviewer and the black-box QA pass need.

## Architecture (one paragraph)

A **Rust core owns truth; the shell owns presentation.** Domain crates under
`crates/` implement the virtual filesystem, the resumable job DAG, the SQLite
metadata catalog + search index, the preview provider registry, the theme engine
(P0 — no hard-coded colors anywhere), the plugin host, the MCP "AI file clerk"
server, media identity (TMDB / No-Intro-style hash datasets), and the sandboxed
MAME Easter-egg runtime. `src-tauri/` exposes only **typed commands and events** —
no broad filesystem access reaches web content. `ui/` is a TypeScript adaptive shell
(desktop / tablet-phone / TV layouts) that renders exclusively from theme tokens.

## Division of labor (standing rule for this build)

- **AI writes the code** (this scaffold, then feature crates).
- **The Rust compiler + tests perform mechanical QC** — first `cargo check` runs on
  the M2 Mac mini (no Rust toolchain in the build sandbox).
- **A second AI performs adversarial code review** — queued (see
  `docs/REVIEW_QUEUE.md`); blocked until the Gemini/DeepSeek sign-ins are restored.
- **The human performs black-box UX/behavior QA** — run it, click it, try to break
  it, compare against the spec (`docs/QA_CHECKLIST.md`).

## Layout

```
crates/            Rust domain crates (the truth)
  ufm-core         shared ids, errors, events
  ufm-vfs          virtual filesystem: items, locations, provider contract
  ufm-jobs         resumable job DAG + journal (TeraCopy-style ops live here)
  ufm-catalog      SQLite WAL metadata catalog, FTS, labels, collections
  ufm-search       query language (keywords, facets, booleans) + index feeds
  ufm-preview      pluggable preview provider registry (spacebar Quick Look)
  ufm-themes       theme packages: schema, token resolution, contrast checks
  ufm-plugins      hot-loaded plugin host + capability broker + sandbox policy
  ufm-mcp          MCP server: pairing, scoped grants, approval broker, audit log
  ufm-media        media identity: filename parsing, TMDB matching, ROM hashing
  ufm-emu          sandboxed MAME runtime stub (spacebar-on-ROM Easter egg)
src-tauri/         Tauri v2 app: typed commands/events only
ui/                TypeScript adaptive shell (token-driven, no hard-coded colors)
themes/grizzette   the built-in example theme (peach paper / chocolate ink)
plugins/           drop-in plugin folder (hot-loaded; see docs/PLUGIN_API.md)
docs/              architecture, format, API, QA, and review-queue notes
scripts/           crate scaffolding helper
```

## Build (on a machine with the toolchain)

```sh
# Rust core + desktop app
cargo check --workspace          # mechanical QC pass 1
cargo test --workspace           # mechanical QC pass 2
cargo tauri dev                  # run the adaptive shell (needs Tauri CLI)

# TypeScript shell
cd ui && npm install && npm run dev
```

macOS public distribution/notarization needs the $99/yr Apple Developer Program;
open-source Windows builds may qualify for free SignPath Foundation signing.
Nothing is signed or published from this scaffold.

## Non-negotiables carried over from the spec

1. **Theme engine is P0.** No production colors, spacing, icons, or fonts outside
   the token system — enforced by `ufm-themes` + a conformance test (to be added).
2. **Local-first.** Browsing, metadata, labels, search indexes, rules, and
   operation history work offline. Cloud accounts extend the library; they don't
   own it.
3. **One mental model.** Local folders, cloud dirs, shares, archives, search
   results, and virtual collections implement the same location contract.
4. **Safe power.** Job DAG previews effects, reveals conflicts, journals everything,
   supports undo/compensating actions. Confirmations are reserved for
   high-consequence actions only.
5. **Zero-config media.** No "point me at your media folders" setup screen — media
   identity is a classifier over the already-global index; datasets ship/update
   silently.
6. **GPL compatibility** for the emulation path (no bundled ROMs or BIOS).

## Status

v0.1 scaffold: crate boundaries, core types, provider/plugin/theme/MCP contracts,
Tauri command surface, UI shell skeleton, Grizzette theme package, docs. Feature
implementation follows the roadmap in the spec (§32) crate by crate.
