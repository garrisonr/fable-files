/**
 * JobsPanel — floating card (bottom-right, material blur) tracking
 * background work: transcodes and burns (percent-shaped events), and
 * Phase 1 transfer jobs (the `job-progress` contract: journaled
 * copy/move/delete with pause/resume/cancel). Spring-in per the
 * design law; each row: name, live progress bar, percent, controls.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { springTo, reducedMotion } from "./motion";
import { api, type JobSummary } from "./api";

interface JobRow {
  root: HTMLDivElement;
  bar: HTMLDivElement;
  pct: HTMLSpanElement;
  cancel: HTMLButtonElement;
  pause?: HTMLButtonElement;
  paused: boolean;
}

/** The backend `job-progress` payload (commands.rs emit_job_progress). */
interface JobProgress {
  job_id: string;
  title: string;
  state: string;
  steps_done: number;
  steps_total: number;
  bytes_done: number;
  bytes_total: number;
  current: string | null;
  done: boolean;
  error: string | null;
}

const TERMINAL_STATES = new Set(["completed", "cancelled", "failed"]);

export class JobsPanel {
  readonly el: HTMLDivElement;
  private rows = new Map<string, JobRow>();
  private list: HTMLDivElement;
  /** Fired when a job completes successfully (shell refreshes). */
  onJobDone: ((output: string) => void) | null = null;
  /** Fired when a job fails. */
  onJobError: ((error: string) => void) | null = null;
  /** Fired on a transfer job's terminal emit (inc-2 §3.2): the shell
   * drains its per-call continuation (refresh + undo push) from here. */
  onTransferDone: ((jobId: string, error: string | null) => void) | null =
    null;

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
    void listen<JobProgress>("job-progress", (e) =>
      this.onJobProgress(e.payload),
    ).catch(() => undefined);
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

  /** Register a transfer job (inc-2): a row with pause/resume (⏸/▶)
   * and cancel (✕) wired to the job commands. Rows are keyed
   * `job:<id>` so they can never collide with transcode/burn ids. */
  trackJob(jobId: string, label: string): void {
    const key = `job:${jobId}`;
    if (this.rows.has(key)) return;
    this.show();
    const row = this.makeRow(key, label, true);
    row.pause?.addEventListener("click", () => {
      if (row.paused) void api.resumeJob(jobId).catch(() => undefined);
      else void api.pauseJob(jobId).catch(() => undefined);
    });
    row.cancel.addEventListener("click", () => {
      void api.cancelJob(jobId).catch(() => undefined);
    });
  }

  /** Startup hydration (inc-2 §3.2): surface every non-terminal job
   * the backend still holds — above all a job crash-recovery left
   * Paused, which appears here with a ▶ so the user can resume it
   * from exactly where the backend left it. */
  async hydrate(): Promise<void> {
    let jobs: JobSummary[];
    try {
      jobs = await api.listJobs();
    } catch {
      return; // panel simply starts empty
    }
    for (const j of jobs) {
      if (TERMINAL_STATES.has(j.state)) continue;
      this.trackJob(j.job_id, j.title);
      const row = this.rows.get(`job:${j.job_id}`);
      if (row) {
        const pct =
          j.bytes_total > 0
            ? (j.bytes_done / j.bytes_total) * 100
            : j.steps_total > 0
              ? (j.steps_done / j.steps_total) * 100
              : 0;
        this.paintRow(row, pct, j.state);
      }
    }
  }

  private makeRow(id: string, label: string, transfer = false): JobRow {
    const root = document.createElement("div");
    root.className = "job-row" + (transfer ? " job-row-xfer" : "");
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
    let pause: HTMLButtonElement | undefined;
    if (transfer) {
      pause = document.createElement("button");
      pause.className = "icon-btn job-pause";
      pause.textContent = "⏸";
      pause.title = "Pause";
      root.append(name, track, pct, pause, cancel);
    } else {
      root.append(name, track, pct, cancel);
    }
    this.list.appendChild(root);
    const row: JobRow = { root, bar, pct, cancel, pause, paused: false };
    this.rows.set(id, row);
    return row;
  }

  /** Paint a transfer row from a state + percent: the bar freezes
   * while paused and the pause affordance flips to ▶. */
  private paintRow(row: JobRow, percent: number, state: string): void {
    const clamped = Math.max(0, Math.min(100, percent));
    row.bar.style.width = `${clamped}%`;
    row.paused = state === "paused";
    if (row.pause) {
      row.pause.textContent = row.paused ? "▶" : "⏸";
      row.pause.title = row.paused ? "Resume" : "Pause";
    }
    row.pct.textContent = row.paused
      ? "⏸ Paused"
      : `${Math.floor(clamped)}%`;
  }

  private onJobProgress(p: JobProgress): void {
    const key = `job:${p.job_id}`;
    let row = this.rows.get(key);
    if (!row) {
      if (p.done) {
        // Terminal emit for a job we never tracked (e.g. it finished
        // before hydration): still deliver the completion.
        if (this.onTransferDone) this.onTransferDone(p.job_id, p.error);
        return;
      }
      this.trackJob(p.job_id, p.title);
      row = this.rows.get(key);
      if (!row) return;
    }
    if (p.current) row.root.title = p.current;
    if (!p.done) {
      const pct =
        p.bytes_total > 0
          ? (p.bytes_done / p.bytes_total) * 100
          : p.steps_total > 0
            ? (p.steps_done / p.steps_total) * 100
            : 0;
      this.paintRow(row, pct, p.state);
      return;
    }
    this.rows.delete(key);
    row.root.remove();
    if (this.rows.size === 0) this.el.hidden = true;
    if (this.onTransferDone) this.onTransferDone(p.job_id, p.error);
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
