/**
 * UFM adaptive shell — v0.1 skeleton.
 *
 * Layout policy (spec: adaptive shell): one shell, three layout classes.
 * - desktop: full chrome (nav pane, inspector, job center)
 * - handheld (tablet/phone): full browse/search/preview; guided ops; quick theme customize
 * - tv: media-first browse, reduced capability policy
 *
 * The shell renders ONLY from theme tokens (--ufm-*). No hard-coded colors.
 */
import { invoke } from "@tauri-apps/api/core";

type LayoutClass = "desktop" | "handheld" | "tv";

function detectLayout(): LayoutClass {
  // v0.1 heuristic; the real build asks the platform layer.
  const w = window.innerWidth;
  const coarse = window.matchMedia("(pointer: coarse)").matches;
  const tv = window.matchMedia("(display-mode: fullscreen)").matches && w > 1600;
  if (tv) return "tv";
  if (coarse || w < 820) return "handheld";
  return "desktop";
}

interface Item {
  id: string;
  name: string;
  kind: string;
  size_bytes: number | null;
}

/** Install a theme: the Rust core returns CSS custom properties; the shell
 *  applies them verbatim. The shell never invents a color. */
async function applyTheme(themeId: string): Promise<void> {
  const css: string = await invoke("apply_theme", { theme: themeId });
  const el = document.getElementById("ufm-theme-vars");
  if (el) el.textContent = css;
}

async function boot(): Promise<void> {
  const layout = detectLayout();
  document.body.dataset.layout = layout;
  document.getElementById("status")!.textContent =
    `layout: ${layout} · theme: grizzette (built-in)`;

  // TODO: list_themes → apply saved theme; list_locations → nav pane.
  // v0.1: verify the command bridge is alive.
  try {
    const themes: string[] = await invoke("list_themes");
    if (themes.length > 0) await applyTheme(themes[0]);
  } catch (e) {
    document.getElementById("status")!.textContent = `bridge error: ${e}`;
  }
}

document.addEventListener("DOMContentLoaded", () => void boot());
