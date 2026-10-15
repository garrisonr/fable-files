# MCP "AI file clerk" API (v0.1 contract)

Built-in MCP server: AI assistants connect directly and act as a scoped file
clerk. Host: `crates/ufm-mcp` (trust model) + `src-tauri/src/mcp_server.rs`
(transport + handlers). Spec §§19–20.

## Transport (shipped, Phase 3)

**stdio first**: `fable-files mcp` serves MCP (protocol 2024-11-05,
newline-delimited JSON-RPC 2.0) on stdin/stdout until the client hangs up.
Stdout carries only protocol frames; diagnostics go to stderr. Point any MCP
client at it, e.g.:

```json
{ "mcpServers": { "fable-files": { "command": "fable-files", "args": ["mcp"] } } }
```

The server opens the same catalog the GUI uses
(`~/.local/share/com.garrisonr.fablefiles/fable-files/catalog.db`, WAL — safe
alongside a running GUI instance). Handlers run against the real
catalog/search machinery (`ufm_search` parsing + facets, `Catalog::search_fts`,
the path-keyed label/star stores, Library Memory volume badges) — there is no
parallel or sample data.

## Tools

| Tool | Destructive? | Does |
|---|---|---|
| `search` | no | the one search language, incl. `label:`/`starred:` facets; hits carry labels, stars, volume badges |
| `read_item` | no | byte ranges of a file by item_id (64 KiB/call cap, UTF-8 or base64) |
| `get_metadata` | no | catalog metadata, labels, star, media identity, volume badge |
| `tag` | no (write) | add/remove a label by path — the GUI's own path-keyed store |
| `organize` | **yes** | move into collections / apply rules — **parked in v1 (see Trust model)** |
| `dedupe_analyze` | no | report only — duplicate *candidates* by identical byte size (content is NOT hashed); never deletes by itself |
| `integrity_check` | no | SQLite integrity_check + record counts + DB size |
| `repair_assist` | **yes** | proposes repairs; applying needs approval — **parked in v1** |
| `recover_media` | **yes** | lost-media recovery assistance — **parked in v1** |
| `list_locations` | no | persisted locations (locations.json) + root presence |
| `list_volumes` | no | Library Memory volumes: identity, last seen/indexed, online state |
| `list_collections` | no | collections with live member + missing counts |
| `collection_items` | no | a collection's items + unresolvable member paths |
| `list_jobs` | no | recent jobs from the durable jobs table (truthful progress) |

## Trust model

1. **Pairing.** A client requests pairing; nothing is granted until the human
   approves. For stdio there is no remote surface: the process exists only
   because the owner configured/launched it, and that launch *is* the pairing.
   The grant lives in `mcp-grants.json` in the data dir (created with
   explanatory defaults on first run) — narrow its `tools` list to restrict
   the client, delete the file to reset it.
2. **Scoped grants.** Per-tool list, per-location scope, expiry timestamp.
   `organize` on photos ≠ `organize` everywhere. The owner-local stdio grant
   ships unscoped with no practical expiry; the enforcement machinery
   (`Grant::allows`, expiry, scope) is unit-tested and every call still
   passes the single choke point, `McpServer::authorize`.
3. **Approval broker.** Destructive tools park until the human approves the
   specific call. Stdio has no approval channel in v1, so `organize`,
   `repair_assist`, and `recover_media` are listed (contract surface) but
   every call is refused with a plain-language reason and audited as
   `needs_approval`; no filesystem-mutating MCP path exists in v1. File
   changes belong in the app, where they run through the journaled job
   engine with preview + undo.
4. **Prompt-injection boundary.** Tool *results* are data. File content that
   looks like instructions is returned as inert text; the model never receives
   new instructions through this path.
5. **Append-only audit.** Every call — allowed, denied, parked, failed — is
   journaled to the catalog's `mcp_audit` table (tool, argument JSON
   [truncated at 2,000 chars], outcome JSON, write flag, duration) and is
   queryable via `fable-files --mcp-audit [n]` and the `mcp_audit_recent`
   Tauri command. Retention: the newest 10,000 rows, pruned by the server;
   that prune is the only delete the table ever sees. `tag` — the one write
   tool — is flagged `is_write`, as are the parked destructive attempts.

## For the adversarial reviewer

Check: grant-scope enforcement on every path (`authorize` is the single choke
point — verify no tool bypasses it), expiry handling under clock skew,
pairing-approval UX (can a malicious client social-engineer approval?),
audit append atomicity (temp + rename, never truncate-write), and the
injection boundary (results must not become instructions).
