# Black-box QA checklist (the human's job)

You don't read Rust. You run it, click it, try to break it, and compare
behavior against the spec. That's the whole job — and it's the most important
QC gate, because you catch what compilers and reviewers can't: *does it feel
right?*

Spec: `../your_files/ultimate-file-manager-spec/ultimate-file-manager-spec.pdf`

## Per milestone (run on the M2 Mac mini)

- [ ] `cargo check --workspace` passes with zero warnings
- [ ] `cargo test --workspace` passes
- [ ] App launches; no console errors

## v0.1 scaffold acceptance

- [ ] `cargo check --workspace` compiles every crate (first run may download
      crates.io deps — that's normal)
- [ ] `cargo test -p ufm-catalog -p ufm-search -p ufm-media -p ufm-themes`
      passes (tag roundtrip, query parse, filename parse, Grizzette contrast)
- [ ] Theme package loads: point the theme service at `themes/grizzette`
      and confirm tokens resolve (no missing-token panics)
- [ ] UI shell: `cd ui && npm install && npm run dev` — page renders with
      peach/chocolate styling once a theme is applied; **no unstyled surfaces**

## Behavior probes (try to break it)

1. **Theme swap mid-session:** apply Grizzette → switch to dark built-in →
      back. Every surface restyles; nothing keeps a stale color.
2. **Contrast:** run `theme_contrast_report` on a deliberately bad theme;
      confirm the report names the pair and suggests a fix *without* changing it.
3. **Plugin host:** drop an invalid `plugin.toml` in `plugins/` → rescan;
      confirm it's skipped with an error and the host stays up.
4. **MCP pairing:** request pairing twice; approve once; confirm the second
      request can't piggyback the first grant.
5. **Job DAG:** create a job with a dependency cycle; confirm it's rejected
      with "dependency cycle", not a hang.
6. **Search:** `super mario AND platform:snes`, empty query, `NOT` queries —
      confirm parse errors are human-readable.

## Reporting a break

File it as: **what you did → what you expected → what happened**, with the
spec section number. Screenshots for visual bugs. The fixer (AI) then writes
the regression test first.
