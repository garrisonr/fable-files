# Architecture — how the scaffold maps to the spec

Source of truth: `../your_files/fable-files-spec/fable-files-spec.pdf`
(spec §25, "A Rust core owns truth; the shell owns presentation").

## Crate map

| Crate | Spec section | Owns |
|---|---|---|
| `ufm-core` | §25 | ids, errors, domain events |
| `ufm-vfs` | §25 | virtual filesystem: items, locations, provider contract |
| `ufm-jobs` | §§9, 25 | resumable job DAG + journal (the transfer center) |
| `ufm-catalog` | §§25–26 | SQLite WAL catalog: FTS, labels, collections, view profiles, media identities, job journal, MCP audit |
| `ufm-search` | §13 | query language (keywords, facets, booleans), index feeds |
| `ufm-preview` | §8 | Quick Look provider registry (sandboxed, budgeted, cancelable) |
| `ufm-themes` | §24 (**P0**) | theme packages: manifest, semantic tokens, contrast checks, CSS export |
| `ufm-plugins` | §§17–18 | hot-load host, capability broker, sandbox policy |
| `ufm-mcp` | §§19–20 | MCP server: pairing, scoped grants, approval broker, append-only audit |
| `ufm-media` | §§14–15 | media identity: filename parsing, TMDB refs, ROM hash datasets |
| `ufm-emu` | §8, §25 | sandboxed MAME backend stub (spacebar-on-ROM Easter egg) |
| `src-tauri` | §25 | typed commands/events only — no broad FS exposure to web content |
| `ui/` | adaptive shell | token-driven TypeScript shell (desktop / handheld / tv) |

## Architecture rules (enforced, not aspirational)

1. **UI state may be transient; operations, metadata, indexes, template versions,
   and theme packages are durable, versioned data owned by the Rust core.**
   The shell holds no source of truth.
2. **No hard-coded production colors, spacing, icons, or fonts outside the
   token system.** The theme conformance test (planned under `src-tauri`)
   fails the build on violation.
3. **Every multi-step operation is a resumable, journaled job DAG.**
   Steps are idempotent; crash recovery replays the journal and re-queues
   non-`Done` steps. Checkpoint from the first version — never bolted on.
4. **Providers, parsers, and plugins are sandboxed.** The shell never receives
   raw paths; preview decoders are budgeted and cancelable; plugins start
   locked down.
5. **MCP trust model:** pairing → scoped grants → approval broker for
   destructive tools → append-only audit. Tool results are data, never
   instructions (prompt-injection boundary).
6. **GPL compatibility** end to end (workspace license) for the emulation path.
   No bundled ROMs or BIOS, ever.

## Data flow (one request)

```
UI event → Tauri typed command → domain crate → catalog/journal write (fsync)
        → DomainEvent emitted → UI updates from the event, not from local state
```

## Roadmap order (spec §32)

1. VFS local provider + catalog watcher feed (browse for real)
2. Job engine execution (copy/move with verify-then-skip) + transfer center UI
3. Search over the FTS index + saved searches as smart folders
4. Preview providers (image/text first) + spacebar Quick Look
5. Theme service wiring (apply/rollback/gallery import) + conformance test
6. Plugin host file-watching + first-party `organize.rule` example
7. MCP server transport + pairing UI + approval broker UI
8. Media classifier stage 2 (TMDB matching) + ROM datasets
9. Emulation backend binding (P1)
