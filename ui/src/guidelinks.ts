/**
 * Guidance links (Dickles, 2026-10-15: "Sir — this website should be
 * an actual URL"). Teaching/walkthrough copy names websites; every
 * named site must be a REAL link that opens in the system browser,
 * never plain text that looks like one.
 *
 * Two render styles, one behavior:
 *  - `guidanceNote(text)` builds a <p class="prefs-note"> from text
 *    with a [label](https://…) convention — DOM-built (text nodes +
 *    anchors), so copy can never inject markup.
 *  - Copy already built with innerHTML uses
 *    <a class="guide-link" href="…">label</a> and the host calls
 *    `wireGuideLinks(block)` once — a Tauri webview anchor would
 *    navigate the app window itself, so clicks are intercepted and
 *    routed through api.openUrl (the same open-external path About's
 *    Report a Bug uses).
 *
 * Every URL passed here was verified live against the site's own
 * pages on 2026-10-15 (see the ship notes) — never construct one.
 */
import { api } from "./api";
import { Toast } from "./toast";

export function openGuideLink(url: string): void {
  // Never swallow a failure: a click that silently does nothing is
  // how the dead Preferences links hid for a day (2026-10-16 — the
  // backend gate rejected every non-project URL and `void` ate it).
  api.openUrl(url).catch(() => {
    Toast.error(`That link could not be opened: ${url}`);
  });
}

/** Build a teaching paragraph with real links from [label](url) copy. */
export function guidanceNote(text: string, className = "prefs-note"): HTMLParagraphElement {
  const p = document.createElement("p");
  p.className = className;
  const re = /\[([^\]]+)\]\((https:\/\/[^)\s]+)\)/g;
  let last = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) {
    if (m.index > last) {
      p.appendChild(document.createTextNode(text.slice(last, m.index)));
    }
    const url = m[2];
    const a = document.createElement("a");
    a.className = "guide-link";
    a.href = url;
    a.textContent = m[1];
    a.addEventListener("click", (e) => {
      e.preventDefault();
      openGuideLink(url);
    });
    p.appendChild(a);
    last = m.index + m[0].length;
  }
  if (last < text.length) {
    p.appendChild(document.createTextNode(text.slice(last)));
  }
  return p;
}

/** Wire every a.guide-link inside `root` to open externally. */
export function wireGuideLinks(root: ParentNode): void {
  root.querySelectorAll<HTMLAnchorElement>("a.guide-link").forEach((a) => {
    if (a.dataset.wired) return;
    a.dataset.wired = "1";
    a.addEventListener("click", (e) => {
      e.preventDefault();
      openGuideLink(a.href);
    });
  });
}
