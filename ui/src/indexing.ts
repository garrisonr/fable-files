/**
 * Indexing: keep the catalog fresh. On boot, index EVERY location (not
 * just the first), and the shell can re-index on demand ("Re-index
 * Library" / the smart-folder header button) so files downloaded while
 * the app runs appear without a restart.
 *
 * Backend semantics (verified in commands.rs): start_indexing claims a
 * single GLOBAL slot — a call while a walk is running returns the
 * current state and does NOT queue. Locations are therefore chained
 * here: start the head of the queue, wait for the completion edge
 * (index-progress event / 2s poll of index_status), then start the next.
 * When the queue fully drains, onComplete fires once — the shell uses
 * it to refresh the visible smart folder / search results.
 */
import { api, onIndexProgress, type IndexState } from "./api";

const POLL_MS = 2000;

export class IndexTracker {
  private timer = 0;
  private watching = false;
  /** Location ids waiting to be indexed; the head is in flight. */
  private queue: string[] = [];
  private starting = false;
  /** Observed the backend actually running for the current head. */
  private sawRunning = false;
  /** When the current head was started (fast empty walks may never
   * show a running state between polls; the timestamp is the fallback
   * signal that the head finished). */
  private headStartedAt = 0;

  constructor(
    private onStatus: (text: string | null) => void,
    private onComplete?: () => void,
  ) {}

  async boot(locationIds: string[]): Promise<void> {
    if (locationIds.length === 0) return;
    await this.reindexAll(locationIds);
  }

  /** Queue every location for a fresh indexing pass. */
  async reindexAll(locationIds: string[]): Promise<void> {
    for (const id of locationIds) {
      if (!this.queue.includes(id)) this.queue.push(id);
    }
    if (this.queue.length === 0) return;
    await this.ensureWatching();
    await this.startNext();
  }

  /** True while any indexing pass is running or queued. */
  get busy(): boolean {
    return this.watching && (this.queue.length > 0 || this.sawRunning);
  }

  private async ensureWatching(): Promise<void> {
    if (this.watching) return;
    this.watching = true;
    try {
      await onIndexProgress((s) => this.handle(s));
    } catch {
      // events unavailable — polling still covers it
    }
    void this.poll();
    this.timer = window.setInterval(() => void this.poll(), POLL_MS);
  }

  private stopWatching(): void {
    this.watching = false;
    window.clearInterval(this.timer);
  }

  private async startNext(): Promise<void> {
    if (this.starting) return;
    const next = this.queue[0];
    if (!next) return;
    this.starting = true;
    this.sawRunning = false;
    this.headStartedAt = Date.now();
    try {
      await api.startIndexing(next);
    } catch {
      // Backend refused this location (unknown id): drop it and move on.
      this.queue.shift();
      this.headStartedAt = 0;
    } finally {
      this.starting = false;
    }
  }

  private async poll(): Promise<void> {
    try {
      const s = await api.indexStatus();
      this.handle(s);
    } catch {
      if (this.queue.length === 0) this.stopWatching();
    }
  }

  private handle(s: IndexState): void {
    if (s.running) {
      this.sawRunning = true;
      const files = s.files_done.toLocaleString();
      const more =
        this.queue.length > 1 ? ` (${this.queue.length} locations)` : "";
      this.onStatus(`Indexing… ${files} files${more}`);
      return;
    }
    // Not running: either the head finished or nothing has started yet.
    if (this.queue.length === 0) {
      this.onStatus(null);
      this.stopWatching();
      return;
    }
    if (this.starting) return; // start call in flight — wait for it
    const headDone =
      this.sawRunning ||
      (this.headStartedAt > 0 && Date.now() - this.headStartedAt >= 1500);
    if (!headDone) return; // walk may not have spun up yet
    this.queue.shift();
    this.sawRunning = false;
    this.headStartedAt = 0;
    if (this.queue.length > 0) {
      void this.startNext();
      return;
    }
    // Queue drained: the whole pass is complete.
    this.onStatus(null);
    this.stopWatching();
    this.onComplete?.();
  }
}
