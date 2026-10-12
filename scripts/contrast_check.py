#!/usr/bin/env python3
"""WCAG 2.x contrast check for Fable Files theme packages.

Checks the four mandated text pairs from docs/THEME_FORMAT.md:
  text/surface, text_muted/surface, accent_text/accent, text/content_canvas
against 4.5:1 (WCAG AA). Usage:
  python3 scripts/contrast_check.py themes/fable-light themes/fable-dark
"""
import re
import sys


def rel_luminance(hex_color: str) -> float:
    hex_color = hex_color.strip().lstrip("#")
    r, g, b = (int(hex_color[i : i + 2], 16) / 255.0 for i in (0, 2, 4))

    def lin(c: float) -> float:
        return c / 12.92 if c <= 0.03928 else ((c + 0.055) / 1.055) ** 2.4

    return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)


def ratio(fg: str, bg: str) -> float:
    l1, l2 = rel_luminance(fg), rel_luminance(bg)
    lighter, darker = max(l1, l2), min(l1, l2)
    return (lighter + 0.05) / (darker + 0.05)


def load_tokens(path: str) -> dict:
    tokens: dict = {}
    with open(path) as f:
        for line in f:
            m = re.match(r'^\s*([a-z_]+)\s*=\s*"#([0-9A-Fa-f]{6})"', line)
            if m:
                tokens[m.group(1)] = "#" + m.group(2)
    return tokens


PAIRS = [
    ("text", "surface"),
    ("text_muted", "surface"),
    ("accent_text", "accent"),
    ("text", "content_canvas"),
]

THRESHOLD = 4.5
failed = False

for theme_dir in sys.argv[1:]:
    tokens = load_tokens(f"{theme_dir}/tokens.toml")
    print(f"== {theme_dir}")
    for fg, bg in PAIRS:
        r = ratio(tokens[fg], tokens[bg])
        ok = r >= THRESHOLD
        failed = failed or not ok
        print(
            f"  {fg:12s}/{bg:15s} {tokens[fg]} on {tokens[bg]}  "
            f"{r:5.2f}:1  {'PASS' if ok else 'FAIL'}"
        )

sys.exit(1 if failed else 0)
