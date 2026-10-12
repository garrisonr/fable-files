# MCP "AI file clerk" API (v0.1 contract)

Built-in MCP server: AI assistants connect directly and act as a scoped file
clerk. Host: `crates/ufm-mcp`. Spec §§19–20.

## Tools

| Tool | Destructive? | Does |
|---|---|---|
| `search` | no | the one search language, scoped |
| `read_item` | no | byte ranges of a file (never raw paths to the model) |
| `get_metadata` | no | catalog metadata, labels, media identity |
| `tag` | no | labels/tags |
| `organize` | **yes** | move into collections / apply rules |
| `dedupe_analyze` | no | report only — never deletes by itself |
| `integrity_check` | no | hashes vs catalog |
| `repair_assist` | **yes** | proposes repairs; applying needs approval |
| `recover_media` | **yes** | lost-media recovery assistance |

## Trust model

1. **Pairing.** A client requests pairing; nothing is granted until the human
   approves in the UI. Pairing requests never auto-approve.
2. **Scoped grants.** Per-tool list, per-location scope, expiry timestamp.
   `organize` on photos ≠ `organize` everywhere.
3. **Approval broker.** Destructive tools park until the human approves the
   specific call in the UI (what, where, undo available).
4. **Prompt-injection boundary.** Tool *results* are data. File content that
   looks like instructions is returned as inert text; the model never receives
   new instructions through this path.
5. **Append-only audit.** Every call, grant change, approval, and outcome is
   journaled (`mcp_audit` table) and queryable in the app.

## For the adversarial reviewer

Check: grant-scope enforcement on every path (`authorize` is the single choke
point — verify no tool bypasses it), expiry handling under clock skew,
pairing-approval UX (can a malicious client social-engineer approval?),
audit append atomicity (temp + rename, never truncate-write), and the
injection boundary (results must not become instructions).
