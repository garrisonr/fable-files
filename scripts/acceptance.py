#!/usr/bin/env python3
"""Fable Files acceptance cross-check (Phase 6).

Mechanical half of the acceptance pass — the hand-verified promise
table lives in docs/ACCEPTANCE.md; this script produces the facts it
must agree with:

1. Commands registered in src-tauri/src/lib.rs (generate_handler!).
2. Commands the UI actually invokes (ui/src/*.ts string literals).
3. Both directions of drift: invoked-but-unregistered (a dead
   button — a real defect) and registered-but-uninvoked
   (informational: some commands are CLI/headless-only by design).
4. Evidence probes: for each headline promise, a grep that must hit
   in the tree, so the table's "Implemented" rows are backed by the
   code, not by memory.

Usage: python3 scripts/acceptance.py [--gates-log /tmp/p6-gates.log]
Exit code: 1 if any invoked-but-unregistered command or any failed
evidence probe is found; 0 otherwise.
"""

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# (promise, path, needle) — every needle must be present in the file.
PROBES = [
    ("Jobs engine wired (journaled start_job)", "src-tauri/src/commands.rs", "pub(crate) fn start_job("),
    ("WriteFile step (plugin proposals)", "crates/ufm-jobs/src/lib.rs", "WriteFile"),
    ("WriteFile executor arm", "src-tauri/src/commands.rs", "StepOp::WriteFile"),
    ("Plugin runtime (wasmi)", "crates/ufm-plugins/src/runtime.rs", "wasmi"),
    ("Plugin host ABI (fable_event)", "crates/ufm-plugins/src/runtime.rs", "fable_event"),
    ("Plugin invocation scope (read_item)", "crates/ufm-plugins/src/runtime.rs", "read_item"),
    ("Example plugin ships in-repo", "plugins/metadata-note/plugin.toml", "Metadata Note"),
    ("Folder templates store", "src-tauri/src/templates.rs", "folder-templates"),
    ("Template apply is a job", "src-tauri/src/templates.rs", "apply_folder_template"),
    ("EPUB book decoder", "crates/ufm-preview/src/epub.rs", "decode_epub_book"),
    ("Reader backend", "src-tauri/src/reader.rs", "decode_epub_book"),
    ("Reader progress sidecar", "src-tauri/src/reader.rs", "reading-progress.json"),
    ("MCP stdio server", "src-tauri/src/mcp_server.rs", "mcp"),
    ("MCP audit persistence", "crates/ufm-catalog/src/lib.rs", "mcp_audit"),
    ("Library Memory volumes", "crates/ufm-catalog/src/lib.rs", "volumes"),
    ("ROM identification dataset", "crates/ufm-media/src/romdat.rs", "RomDataset"),
    ("Media Resolution (TMDB)", "src-tauri/src/mediares.rs", "tmdb"),
    ("ROM media cascade", "src-tauri/src/rommedia.rs", "screenscraper"),
]


def registered_commands() -> list[str]:
    lib = (ROOT / "src-tauri/src/lib.rs").read_text()
    block = re.search(r"generate_handler!\[(.*?)\]\s*\)", lib, re.S)
    if not block:
        return []
    names = []
    for entry in re.findall(r"(\w+)::(\w+)", block.group(1)):
        names.append(entry[1])
    return sorted(set(names))


def invoked_commands() -> set[str]:
    """Find invoke() targets. Generics can contain '>' (e.g.
    invoke<Record<string, unknown>>(\"get_ui_prefs\")), so a naive
    regex undercounts: instead, take the first quoted literal in the
    240 characters after each `invoke` token — this codebase always
    names the command as the call's first argument."""
    names: set[str] = set()
    for ts in (ROOT / "ui/src").glob("*.ts"):
        text = ts.read_text(errors="replace")
        for m in re.finditer(r"\binvoke\b", text):
            window = text[m.end():m.end() + 240]
            lit = re.search(r'"([a-z_0-9]+)"', window)
            if lit:
                names.add(lit.group(1))
    return names


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--gates-log", default=None)
    args = ap.parse_args()

    registered = registered_commands()
    invoked = invoked_commands()
    print(f"registered commands: {len(registered)}")
    print(f"UI-invoked commands: {len(invoked)}")

    dead = sorted(invoked - set(registered))
    print(f"invoked-but-unregistered (DEAD BUTTONS): {len(dead)}")
    for name in dead:
        print(f"  !! {name}")

    uninvoked = sorted(set(registered) - invoked)
    print(f"registered-but-uninvoked (informational): {len(uninvoked)}")
    for name in uninvoked:
        print(f"  -- {name}")

    print("evidence probes:")
    failed = 0
    for promise, rel, needle in PROBES:
        path = ROOT / rel
        hit = path.exists() and needle.lower() in path.read_text(errors="replace").lower()
        print(f"  [{'ok' if hit else 'FAIL'}] {promise}  ({rel} ~ {needle!r})")
        failed += 0 if hit else 1

    if args.gates_log:
        log = Path(args.gates_log)
        if log.exists():
            print("gate results from log:")
            for line in log.read_text(errors="replace").splitlines():
                if "test result:" in line:
                    print(f"  {line.strip()}")
        else:
            print(f"gates log not found: {log}")

    if dead or failed:
        print("ACCEPTANCE CROSS-CHECK: FAILED")
        return 1
    print("ACCEPTANCE CROSS-CHECK: OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
