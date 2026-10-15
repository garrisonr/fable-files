# Fable Files — Acceptance Pass (Phase 6, 2026-10-15)

Base build: `f19050f5d7a6a57d94194a95cb8a7af4` (Media Resolution) + Phase 6.
Method: (1) a mechanical cross-check, `scripts/acceptance.py` —
commands registered vs commands the UI invokes, plus grep evidence
probes for the headline promises; (2) a hand pass over the promise
sources: `DESIGN.md` (the spec), `docs/PLUGIN_API.md`,
`docs/MCP_API.md`, `docs/ARCHITECTURE.md`, `docs/THEME_FORMAT.md`,
`README.md`, `QA_CHECKLIST.md`, and the campaign phase reports
(NIGHT-OPS). GUI behavior is Dickles' retest territory — nothing here
claims a GUI behavior was seen working on a screen.

Scoring: **Implemented** (built + evidence) / **Partial** (real but
narrower than promised — the gap is named) / **Missing** (not built —
named, with whose call it is).

## Mechanical cross-check (scripts/acceptance.py, this build)

- Registered commands (`generate_handler!`): **182**
- UI-invoked commands: **179**
- Invoked-but-unregistered (dead buttons): **0**
- Registered-but-uninvoked: **3** — `mcp_audit_recent` (the audit
  trail's working surface is the `--mcp-audit` CLI; the registered
  command has no UI caller — a real, minor gap), `term_close`
  (terminal lifecycle handled elsewhere; command appears vestigial),
  `theme_contrast_report` (QA tooling; pairs with
  `scripts/contrast_check.py`).
- Evidence probes: **18/18** present in the tree.
- Gates: see the Phase 6 ship report (full workspace suite + UI
  build, run on the box against this exact tree).

## Promise table

### Core file management
| Promise | Status | Evidence |
|---|---|---|
| Browse/list/grid/icon views, tabs, sidebar, breadcrumbs | Implemented | ui/src (fileview/tabs/sidebar), 182-command surface |
| Open with OS default (double-click law), Quick Look on Space | Implemented | `open_item`, quicklook.ts; double-click law unchanged by Phase 6 |
| New Folder / New File / rename / duplicate / trash / restore | Implemented | commands.rs ops family; trash commands |
| Copy/move engine: journaled jobs, resume, progress, duplicate-job guard | Implemented | ufm-jobs suite; start_job signature guard |
| One-click job undo | Partial | The journal records every step; the engine stores no compensation payloads and no undo command exists. Nothing in the UI claims one. |
| Archives (extract/create), checksums, split/join, ISO mount, burn list, torrents, casting | Implemented | commands registered + invoked (fix-batch/earlier rounds) |
| Terminal panel | Implemented | term commands; xterm in ui |

### Library, search, organization
| Promise | Status | Evidence |
|---|---|---|
| Indexer with live progress + resume | Implemented | ufm-search; incremental re-index 462× measured (20k fixture) |
| Library Memory: offline volumes searchable, mount history, retention + per-volume override + Forget, per-volume Rebuild | Implemented | Indexer v2 ship (2026-10-15 03:15); catalog suite incl. volume tests; Preferences ▸ Library Memory |
| Full-text search with facets (incl. `starred:`) | Implemented | ufm-search 15 tests; search.rs facet post-filter |
| Stars, tags/labels, collections (CRUD + members), per-location view profiles | Implemented | Phase 2 ship (2026-10-15 04:13); path-keyed persistence; rename re-keys |
| Job-moved files keep collection membership | Partial | Path-keyed by design in v1: a file a job moves shows as missing in its collection until re-added. Named limit, not a silent break. |

### MCP (spec §§19–20)
| Promise | Status | Evidence |
|---|---|---|
| `fable-files mcp` stdio server, real tools on the GUI's machinery | Implemented | Phase 3 ship (2026-10-15 05:07); 14 tools; ufm-mcp 6 tests |
| Trust model: pairing grants narrow tools; destructive trio needs approval and has no fs-mutating path | Implemented | `ufm_mcp::authorize` choke point; parked tools refuse + audit |
| Every call audited (10k retention) + `--mcp-audit` CLI | Implemented | `mcp_audit` table; CLI readback proven live |
| Audit trail visible in Preferences | Partial | The `mcp_audit_recent` command is registered but has no UI caller (found by this pass). CLI is the working surface. |

### Previews & Quick Look (spec §25 area)
| Promise | Status | Evidence |
|---|---|---|
| Image/video/audio previews, detach pop-outs | Implemented | Fix batch (2026-10-15 00:17) — all seven dogfood laws |
| PDF preview: first-page render + metadata + text | Implemented | Phase 4; poppler-backed with plain-language fallback notes |
| EPUB preview: cover + metadata + excerpt + DRM honesty | Implemented | Phase 4; pure-Rust decoder, ufm-preview suite |

### ROM library & media
| Promise | Status | Evidence |
|---|---|---|
| ROM identification (DAT-governed where DATs exist) | Implemented | 60 systems / 139,309 rows; ufm-media romdat |
| Honest coverage: systems with no DAT anywhere ship extension-governed and say so | Implemented | ROM org ship (2026-10-15 00:51) — Wii U, Astrocade, APF-MP1000, FM Towns, Playdia |
| Owned-hardware canon (his 94 consoles) in-app | Implemented | 20 owned-hardware detail entries; collection source `his-consoles.md` |
| ROM media enrichment (boxart/snaps/titles), ES-DE + RetroArch trees | Implemented | ROM media ship (2026-10-15 02:15); Libretro keyless path live-proven |
| ScreenScraper source | Partial | Works with user-pasted dev credentials (Settings → Advanced, keyring). First-class use needs Fable's own ScreenScraper dev registration — a web signup on the vendor's site, Dickles'/parent lane. |
| EmuMovies source | Partial | Client built (bounded-walk FTP). The vendor's FTP layout is unverifiable without an account; misses speak plainly. |
| Guidance-law Settings copy (teach, free path first, DIY) | Implemented | Preferences copy + Grizzly nudge marker-verified in the shipped binary |

### Media Resolution (film/TV)
| Promise | Status | Evidence |
|---|---|---|
| Parse + TMDB match with honest confidence, Plex/Emby/Jellyfin-compliant naming, journaled Organize | Implemented | Media Resolution ship (2026-10-15 06:40); ufm-media 16 tests |
| Live TMDB keyed path | Partial | Shapes proven against real API captures; activates when the user pastes their own (no-cost) TMDB v3 key in Preferences. Not live-verified — no key on the box. |

### Plugins (spec §§17–18) — Phase 6
| Promise | Status | Evidence |
|---|---|---|
| WASM plugin runtime, sandboxed | Implemented | ufm-plugins runtime (wasmi): fuel budget from the manifest, 256 MB memory cap, no WASI, fresh instance per call; 9 tests incl. fuel/memory/scope attacks |
| Capability/permission model in the MCP trust spirit | Partial | Enforced shape: a plugin sees ONLY the invoked item (`read_item` scope), no network, no fs. Grants beyond the locked-down policy are declared in manifests but not user-configurable yet. |
| Minimal host API: log, item metadata read, propose | Implemented | Host ABI `log` / `read_item` / return-value proposals; plugin log persisted (`plugin-log.jsonl`) |
| Nothing mutates except via the journaled job engine with user confirmation | Implemented | Proposals re-validated host-side (`check_proposal`), confirmed in a dialog, executed as `StepOp::WriteFile` jobs; WriteFile never overwrites |
| One real example plugin, source + built wasm in-repo, end-to-end | Implemented | `plugins/metadata-note/` (source, `plugin.toml`, `main.wasm` 43,877 B); runtime e2e test against the real wasm; seeded into the plugins folder on first run |
| Plugins folder + Preferences list with enable/disable | Implemented | Preferences ▸ Plugins; `~/.local/share/fable-files/plugins/`; rescan on every list — a dropped-in folder appears with no restart |
| All six capability kinds wired (`action.context`, `search.provider`, `preview.provider`, `organize.rule`, `cloud.provider`, `theme.pack`) | Partial | v1 wires `action.context` only. The other five are declared, displayed, and honestly badged "not wired" in Preferences and here. |
| Hot reload on folder change | Partial | Rescan-on-list instead of a filesystem watcher (the spec's own watcher was never committed). No restart is ever needed; a plugin added while Preferences is open appears on the next open. |

### Folder templates (spec §22) — Phase 6
| Promise | Status | Evidence |
|---|---|---|
| Named templates: folder trees + placeholder files | Implemented | templates.rs store under app data; starters seeded (Project, Photo Shoot) |
| Create from an existing folder | Implemented | Capture with rails (10k entries / depth 32); files > 1 MB skipped AND reported in the UI |
| Create blank, rename, delete | Implemented | Preferences ▸ Folder Templates |
| Apply via New Folder flow as a journaled job | Implemented | Background menu ▸ New Folder from Template; planned Mkdir/Copy steps via start_job |

### Ebook reader (spec §25) — Phase 6
| Promise | Status | Evidence |
|---|---|---|
| Reader surface on the EPUB decoder: spine navigation, chapter list | Implemented | reader.rs + reader.ts; `decode_epub_book` (spine order, chapter titles) |
| Progress persistence per book (chapter + type size) | Implemented | `reading-progress.json` sidecar; saved on navigate + close; reopen resumes |
| Typography per design law; adaptive phone→desktop→TV | Implemented | Measure-capped serif column, full-bleed when narrow, A−/A+ steppers, Esc/arrow keys |
| Opens from Quick Look "Read" + Open With ▸ Fable Reader | Implemented | quicklook.ts / openwith.ts; double-click default UNCHANGED (explicit doors only) |
| DRM books | Implemented (honesty) | Plain-language note; metadata still shown; no fake chapters |
| **PDF reader** | Implemented (2026-10-15, PDF Reader build 2c0adbf0) | Poppler-backed paginated reader in the EPUB reader shell: page navigation, zoom, text layer where the PDF carries one. Supersedes the earlier "Missing — CUT" note from the Phase 6 audit — Dickles ordered it built at 07:40 EDT and it shipped same-day. |

### Platform promises
| Promise | Status | Evidence |
|---|---|---|
| Themes: engine + per-distro themes + gallery; Themes is a first-class surface | Implemented | theme commands; Preferences gallery (Profiler-hub precedent in Fable since the UI round) |
| Network locations: AFP / FTP+FTPS / NFS / SMB / SFTP+SSH / WebDAV as first-class locations | Implemented (2026-10-15, network build) | Connect to Server… (hamburger → Connect) mounts via GVfs/GIO on a dedicated glib thread; bookmarks persist as sidebar Locations with Library Memory semantics (offline servers keep catalog rows + "not connected" badge; Forget Server… removes bookmark + saved password). Passwords live ONLY in the system keyring (never in the URL or our files). All eight GVfs backends verified live on the ship box (gvfs 1.54.4): smb/ftp/sftp/dav/davs/nfs/afp all load and attempt real connections. AFP is a first-class CLIENT (legacy Mac servers, Netatalk, Time Capsule); honest caveat: modern macOS no longer SERVES AFP — the dialog says so and points at SMB. SFTP + WebDAV proven end-to-end by a loopback rig on the box (scratch sshd user + Python WebDAV: mount → browse → read → write/read-back/delete → unmount). NFS via gio nfs:// where the backend serves it; kernel NFS mounts simply appear as local volumes. File ops run through the existing journaled job engine over the FUSE mount path; search/indexing treats a connected share like a local volume (Indexer v2, resumable) — there is no server-side search, and the UI never pretends otherwise. SSH host-key prompts follow ~/.ssh/known_hosts (gio 0.18 exposes no ask-question binding; an unanswered prompt fails with a plain-language error). |
| Distro Gauntlet (top-20 distros + Flathub verification) | Missing — separate queue item | Per-distro themes exist; the gauntlet itself is queued campaign work, not a Phase 6 promise kept or broken here. |
| Packaging: AppImage canonical | Implemented | The ship pipeline's install form (this build); Flathub storefront = release work outside this repo tree — Partial overall |
| Report a Bug + Request a Feature in-app | Implemented | shell.ts + about.ts surfaces |
| No-dependency-errors law (apps self-install what they need) | Partial | The PDF path degrades with a plain-language note when poppler is absent rather than installing it. Named gap against the standing law. |
| Rust only; one icon family; GPL-3.0 | Implemented | Workspace is Rust + the Tauri web UI (the family's established shape); license in repo |

### The baseline audits' headline verdicts, re-scored
The pre-campaign external audits (Gemini ~38%, Copilot ~35–40%
implemented) scored plugins, folder templates, and the ebook reader
as ghostware — correctly, at the time. After Phase 6: plugins
Partial (runtime real, one capability wired), folder templates
Implemented, reader Implemented for EPUB and (since 2026-10-15) PDF.
The final external re-audit (next queue item) re-scores the
whole surface from its own packet; this table is the internal
counterpart it will be checked against.
