# Second-AI adversarial review queue

**Status: QUEUED — blocked until the Gemini/DeepSeek sign-ins are restored**
(the QA pipeline stall; needs the human's 2SV tap / login).

Standing rule for this build: AI writes the code, the compiler does mechanical
QC, **a second AI does adversarial review**, the human does black-box QA.
This file is the reviewer's work order. Review happens per milestone, not once
at the end.

## v0.1 scaffold review brief (for the reviewing AI)

You are reviewing a v0.1 Rust workspace scaffold for a Tauri v2 file manager.
Be adversarial: assume the author is clever but fallible. Check:

1. **Soundness.** Any `unwrap`/`expect` on untrusted input? Any `todo!()` on a
   path reachable from a Tauri command without a guard? (`start_copy` and
   `preview` correctly return not-implemented errors — verify no other command
   can reach a `todo!()`.)
2. **MCP choke point.** Is `McpServer::authorize` truly the single path to
   every tool? Could a future tool bypass it? Is expiry enforced under clock
   skew?
3. **Theme token completeness.** Does `TokenSet` cover every role the CSS
   consumes? Any role the shell needs but the schema lacks?
4. **Job DAG.** Is `runnable_order` correct (Kahn's)? Is the cycle error
   reachable before any step runs? Is crash-recovery (`Running → Pending`)
   sound given step idempotency is only a *contract*?
5. **Catalog.** Migration discipline (downgrades refused? version table
   authoritative?), FTS table wiring (`items_fts` needs triggers to stay in
   sync — flag it), foreign-key cascades on delete.
6. **Serde boundaries.** Any struct that crosses the Tauri bridge with a field
   the UI shouldn't see? Any `Deserialize` impl that accepts more than the
   format documents?
7. **License.** Workspace is GPL-3.0-or-later for MAME compatibility — verify
   every new dependency's license is GPL-compatible before it's added.

## How to run it

Paste this file plus the repo path into the reviewing AI when sign-ins are
back. Findings go to the fixer AI with failing-test-first discipline; the
human re-runs `docs/QA_CHECKLIST.md` after fixes land.
