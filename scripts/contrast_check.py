#!/usr/bin/env python3
"""SUPERSEDED — this script no longer checks anything. Do not rely on it.

It verified four contrast pairs for two themes, once, by hand. The gate
is now the Rust suite in crates/ufm-themes: check_contrast runs a
20-pair matrix (text/muted on every surface, status roles, accent,
focus_ring at the 1.4.11 non-text bar) against EVERY themes/*/ package
in the standard cargo gate — see the test
`every_shipped_theme_is_contrast_clean`. Design-checks P1, 2026-10-16.

Kept as a pointer so stale references (old theme headers, old reports)
land somewhere honest instead of on a checker that silently under-checks.
"""
import sys

print(__doc__.strip(), file=sys.stderr)
sys.exit(1)
