import { errText } from "./toast";
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
import { VideoPlayer } from "./video";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import "./styles.css";
import "./distro-themes.css";

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
    if (status) status.textContent = `Boot failed: ${errText(e)}`;
  }
}

/** Pop-out player windows boot here instead of the full shell. */
function bootPlayer(params: URLSearchParams): void {
  document.body.classList.add("player-body");
  const path = params.get("path") ?? "";
  const name = params.get("name") ?? "Video";
  document.title = `${name} — Fable Files`;
  const root = document.getElementById("app-root")!;
  const player = new VideoPlayer({
    path,
    standalone: true,
    onReturn: () => {
      void getCurrentWebviewWindow().close();
    },
  });
  root.appendChild(player.el);
  // The window closing IS the surface closing: tear playback down
  // with it (fix block item 3).
  window.addEventListener("beforeunload", () => player.destroy());
}

const bootParams = new URLSearchParams(location.search);
if (bootParams.get("mode") === "player") {
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", () => bootPlayer(bootParams));
  } else {
    bootPlayer(bootParams);
  }
} else if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", () => void boot());
} else {
  void boot();
}
