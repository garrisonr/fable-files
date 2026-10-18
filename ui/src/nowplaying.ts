/**
 * Now Playing — Listen's shell strip (Ebook P3).
 *
 * Narration lives in Rust and survives the reader closing; this
 * strip is how the shell shows that. It paints only the engine's
 * snapshots (events + one read on mount — no timers, no polling:
 * webview timers lie, and nothing here is time-critical anyway).
 * It hides while the reader overlay for the SAME book is open —
 * the reader has its own Listen bar, and two control surfaces
 * for one narration would be a lie about who's driving.
 */
import { api, type TtsSnapshot } from "./api";
import { listen } from "@tauri-apps/api/event";
import { openReader } from "./reader";

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  e.className = className;
  if (text !== undefined) e.textContent = text;
  return e;
}

class NowPlaying {
  private root: HTMLElement;
  private titleEl: HTMLElement;
  private subEl: HTMLElement;
  private toggleBtn: HTMLButtonElement;
  private snap: TtsSnapshot | null = null;

  constructor() {
    this.root = el("div", "now-playing");
    this.root.hidden = true;
    this.titleEl = el("div", "np-title", "");
    this.subEl = el("div", "np-sub", "");
    const text = el("div", "np-text");
    text.append(this.titleEl, this.subEl);
    this.toggleBtn = el("button", "pill-btn np-btn", "❚❚") as HTMLButtonElement;
    this.toggleBtn.title = "Pause / resume the narration";
    this.toggleBtn.addEventListener("click", () => void this.toggle());
    const stopBtn = el("button", "pill-btn np-btn", "■") as HTMLButtonElement;
    stopBtn.title = "Stop listening";
    stopBtn.addEventListener("click", () => {
      void api
        .ttsStop()
        .then((s) => this.adopt(s))
        .catch(() => {});
    });
    const openBtn = el("button", "pill-btn np-btn", "Open book") as HTMLButtonElement;
    openBtn.title = "Open the book being read aloud";
    openBtn.addEventListener("click", () => {
      if (this.snap?.path) openReader(this.snap.path);
    });
    this.root.append(text, this.toggleBtn, stopBtn, openBtn);
    document.body.appendChild(this.root);

    void api
      .ttsSnapshot()
      .then((s) => this.adopt(s))
      .catch(() => {});
    void listen<TtsSnapshot>("tts-state", (e) => this.adopt(e.payload)).catch(
      () => {},
    );
    // The reader overlay is a direct child of body; watch it
    // come and go so the strip yields to the reader's own bar.
    new MutationObserver(() => this.render()).observe(document.body, {
      childList: true,
    });
  }

  private async toggle(): Promise<void> {
    const s = this.snap;
    if (!s) return;
    const next =
      s.state === "playing"
        ? api.ttsPause()
        : s.state === "paused"
          ? api.ttsResume()
          : null;
    if (!next) return;
    const snap = await next.catch(() => null);
    if (snap) this.adopt(snap);
  }

  private adopt(snap: TtsSnapshot): void {
    this.snap = snap;
    this.render();
  }

  private readerOpenFor(path: string): boolean {
    const overlays = document.querySelectorAll(".reader-overlay");
    for (const o of overlays) {
      if ((o as HTMLElement).dataset.bookPath === path) return true;
    }
    return false;
  }

  private render(): void {
    const s = this.snap;
    const active =
      s !== null &&
      s.path !== null &&
      (s.state === "playing" ||
        s.state === "paused" ||
        s.state === "loading" ||
        s.state === "error");
    if (!active || !s || !s.path) {
      this.root.hidden = true;
      return;
    }
    if (this.readerOpenFor(s.path)) {
      this.root.hidden = true;
      return;
    }
    this.root.hidden = false;
    this.titleEl.textContent =
      s.title ?? s.path.split("/").pop() ?? "Listening";
    const parts: string[] = [];
    if (s.state === "loading") parts.push("The narrator is clearing their throat…");
    else if (s.chapter !== null)
      parts.push(`Chapter ${s.chapter + 1} of ${s.chapterCount}`);
    if (s.state === "paused") parts.push("paused");
    if (s.message) parts.push(s.message);
    this.subEl.textContent = parts.join(" · ");
    this.toggleBtn.textContent = s.state === "playing" ? "❚❚" : "▶";
  }
}

export function mountNowPlaying(): void {
  new NowPlaying();
}
