/**
 * JobsPanel — floating card (bottom-right, material blur) tracking
 * background work: transcodes today, anything with progress events
 * tomorrow. Spring-in per the design law; each row: name, live
 * progress bar, percent, cancel.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { springTo, reducedMotion } from "./motion";

interface JobRow {
  root: HTMLDivElement;
  bar: HTMLDivElement;
  pct: HTMLSpanElement;
  cancel: HTMLButtonElement;
}

export class JobsPanel {
  readonly el: HTMLDivElement;
  private rows = new Map<string, JobRow>();
  private list: HTMLDivElement;
  /** Fired when a job completes successfully (shell refreshes). */
  onJobDone: ((output: string) => void) | null = null;
  /** Fired when a job fails. */
  onJobError: ((error: string) => void) | null = null;

  constructor() {
    this.el = document.createElement("div");
    this.el.className = "jobs-panel";
    this.el.hidden = true;
    const head = document.createElement("div");
    head.className = "jobs-head";
    head.textContent = "⚙ Working…";
    this.list = document.createElement("div");
    this.list.className = "jobs-list";
    this.el.append(head, this.list);
    void listen<{
      id: string;
      percent: number;
      done: boolean;
      output?: string | null;
      error?: string | null;
    }>("transcode-progress", (e) => this.onProgress(e.payload)).catch(
      () => undefined,
    );
    void listen<{
      id: string;
      percent: number;
      done: boolean;
      output?: string | null;
      error?: string | null;
    }>("burn-progress", (e) => this.onProgress(e.payload)).catch(
      () => undefined,
    );
  }

  /** Register a freshly started transcode so its row exists at 0%. */
  trackTranscode(id: string, label: string): void {
    this.show();
    const row = this.makeRow(id, `🎬 ${label}`);
    row.cancel.addEventListener("click", () => {
      void invoke("cancel_transcode", { id }).catch(() => undefined);
    });
  }

  /** Track a USB write (Burn Studio). No backend cancel exists for
   * dd; the row's ✕ just dismisses it. */
  trackBurn(id: string, label: string): void {
    this.show();
    const row = this.makeRow(id, `🔥 ${label}`);
    row.cancel.addEventListener("click", () => {
      row.root.remove();
      this.rows.delete(id);
      if (this.rows.size === 0) this.el.hidden = true;
    });
  }

  private makeRow(id: string, label: string): JobRow {
    const root = document.createElement("div");
    root.className = "job-row";
    const name = document.createElement("div");
    name.className = "job-name";
    name.textContent = label;
    const track = document.createElement("div");
    track.className = "job-track";
    const bar = document.createElement("div");
    bar.className = "job-bar";
    track.appendChild(bar);
    const pct = document.createElement("span");
    pct.className = "job-pct";
    pct.textContent = "0%";
    const cancel = document.createElement("button");
    cancel.className = "icon-btn job-cancel";
    cancel.textContent = "✕";
    cancel.title = "Cancel";
    root.append(name, track, pct, cancel);
    this.list.appendChild(root);
    const row: JobRow = { root, bar, pct, cancel };
    this.rows.set(id, row);
    return row;
  }

  private onProgress(p: {
    id: string;
    percent: number;
    done: boolean;
    output?: string | null;
    error?: string | null;
  }): void {
    const row = this.rows.get(p.id);
    if (!row) return;
    if (!p.done) {
      row.bar.style.width = `${Math.max(0, Math.min(100, p.percent))}%`;
      row.pct.textContent = `${Math.floor(p.percent)}%`;
      return;
    }
    this.rows.delete(p.id);
    row.root.remove();
    if (this.rows.size === 0) this.el.hidden = true;
    if (p.error) {
      if (this.onJobError) this.onJobError(p.error);
    } else if (p.output && this.onJobDone) {
      this.onJobDone(p.output);
    }
  }

  private show(): void {
    if (!this.el.hidden) return;
    this.el.hidden = false;
    if (!reducedMotion()) {
      this.el.style.opacity = "0";
      this.el.style.transform = "translateY(12px) scale(0.97)";
      springTo(
        "jobs-panel-in",
        1,
        (v) => {
          this.el.style.opacity = String(v);
          this.el.style.transform = `translateY(${12 * (1 - v)}px) scale(${0.97 + 0.03 * v})`;
        },
        { from: 0 },
      );
    }
  }
}
