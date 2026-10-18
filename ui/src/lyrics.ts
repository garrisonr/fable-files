/**
 * Synced lyrics, one view for every music surface (Music Glow-Up
 * Phase 2, crew Diva). The pane logic Quick Look grew for Music
 * Completion — embedded USLT-with-stamps or a .lrc sidecar, the
 * sung line lit — lifted into a reusable component driven by the
 * shared player core's snapshots (setPosition), so the Music
 * Player panel, the Stage, and Quick Look all sing from the same
 * sheet. Clicking a synced line seeks there.
 */
import { api } from "./api";
import { parseLrcLines, looksSyncedText, type LrcLine } from "./musiccomplete";

export class LyricsView {
  readonly el: HTMLDivElement;
  /** Set by the host: a synced line was clicked (seconds). */
  onSeek: ((secs: number) => void) | null = null;
  private lines: LrcLine[] = [];
  private lineEls: HTMLElement[] = [];
  private active = -1;
  private synced = false;
  private loadToken = 0;

  constructor() {
    this.el = document.createElement("div");
    this.el.className = "lyrics-view";
    this.el.hidden = true;
  }

  get isSynced(): boolean {
    return this.synced;
  }

  /** Load lyrics for a path. Resolves true when a pane rendered
   * (the host unhides); false when the track has no lyrics. */
  async load(path: string): Promise<boolean> {
    const token = ++this.loadToken;
    let view;
    try {
      view = await api.muLyricsGet(path, false);
    } catch {
      return false;
    }
    if (token !== this.loadToken) return false;
    const syncedText =
      view.embeddedSynced ??
      (view.sidecarText && looksSyncedText(view.sidecarText)
        ? view.sidecarText
        : null);
    const plainText = syncedText
      ? null
      : (view.embeddedPlain ?? view.sidecarText);
    this.el.innerHTML = "";
    this.lines = [];
    this.lineEls = [];
    this.active = -1;
    this.synced = false;
    const head = document.createElement("div");
    head.className = "ql-lyrics-head";
    head.textContent = syncedText ? "Lyrics — synced" : "Lyrics";
    this.el.appendChild(head);
    if (syncedText) {
      const lines = parseLrcLines(syncedText);
      if (!lines.length) return false;
      this.synced = true;
      this.lines = lines;
      for (const l of lines) {
        const d = document.createElement("div");
        d.className = "ql-lrc-line";
        d.textContent = l.text;
        d.addEventListener("click", () => {
          if (this.onSeek) this.onSeek(l.timeMs / 1000);
        });
        this.el.appendChild(d);
        this.lineEls.push(d);
      }
      this.el.hidden = false;
      return true;
    }
    if (plainText) {
      for (const raw of plainText.split("\n")) {
        const d = document.createElement("div");
        d.className = "ql-lrc-line plain";
        d.textContent = raw;
        this.el.appendChild(d);
      }
      this.el.hidden = false;
      return true;
    }
    return false;
  }

  /** The currently-sung line's text (for compact strips). */
  activeText(): string | null {
    if (this.active < 0) return null;
    return this.lines[this.active]?.text ?? null;
  }

  /** Drive the highlight from the core's position clock. Cheap:
   * a no-op unless the active line changed. */
  setPosition(ms: number): void {
    if (!this.synced || !this.lines.length) return;
    let idx = -1;
    for (let i = 0; i < this.lines.length; i++) {
      if (this.lines[i].timeMs <= ms) idx = i;
      else break;
    }
    if (idx === this.active) return;
    if (this.active >= 0) this.lineEls[this.active]?.classList.remove("active");
    this.active = idx;
    if (idx >= 0) {
      this.lineEls[idx]?.classList.add("active");
      this.lineEls[idx]?.scrollIntoView({ block: "nearest" });
    }
  }

  destroy(): void {
    this.loadToken++;
    this.el.remove();
  }
}
