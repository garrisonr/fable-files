/**
 * Fable Files — adaptive shell entry point.
 *
 * One shell, three layout classes (spec: adaptive shell):
 * - desktop:  full chrome (sidebar, tab bar, inspector)
 * - handheld: full browse/search/preview; compact chrome
 * - tv:       media-first browse, reduced capability policy
 *
 * The shell renders ONLY from --ufm-* theme tokens installed by the theme
 * manager. No hard-coded colors anywhere in this tree.
 */
import { Shell } from "./shell";
import "./styles.css";

type LayoutClass = "desktop" | "handheld" | "tv";

function detectLayout(): LayoutClass {
  const w = window.innerWidth;
  const coarse = window.matchMedia("(pointer: coarse)").matches;
  const tv =
    window.matchMedia("(display-mode: fullscreen)").matches && w > 1600;
  if (tv) return "tv";
  if (coarse || w < 820) return "handheld";
  return "desktop";
}

async function boot(): Promise<void> {
  document.body.dataset.layout = detectLayout();
  window.addEventListener("resize", () => {
    document.body.dataset.layout = detectLayout();
  });

  const shell = new Shell();
  document.getElementById("app-root")!.appendChild(shell.el);
  try {
    await shell.boot();
  } catch (e) {
    // Never leave a blank window: surface the failure in the status bar.
    const status = shell.el.querySelector(".status-left");
    if (status) status.textContent = `Boot failed: ${e}`;
  }
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", () => void boot());
} else {
  void boot();
}
