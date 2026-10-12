/**
 * Indexing: on boot, start_indexing for the first location, then show subtle
 * progress in the status bar — polled via index_status every 2s while running
 * and pushed via the index-progress event.
 */
import { api, onIndexProgress, type IndexState } from "./api";

const POLL_MS = 2000;

export class IndexTracker {
  private timer = 0;
  private running = false;

  constructor(private onStatus: (text: string | null) => void) {}

  async boot(firstLocationId: string | null): Promise<void> {
    if (!firstLocationId) return;
    try {
      await api.startIndexing(firstLocationId);
    } catch {
      return; // backend stub: indexing simply isn't running
    }
    try {
      await onIndexProgress((s) => this.handle(s));
    } catch {
      // events unavailable — polling still covers it
    }
    this.running = true;
    void this.poll();
    this.timer = window.setInterval(() => void this.poll(), POLL_MS);
  }

  private async poll(): Promise<void> {
    try {
      const s = await api.indexStatus();
      this.handle(s);
    } catch {
      this.stop();
    }
  }

  private handle(s: IndexState): void {
    if (!s.running) {
      this.stop();
      this.onStatus(null);
      return;
    }
    const files = s.files_done.toLocaleString();
    this.onStatus(`Indexing… ${files} files`);
  }

  private stop(): void {
    this.running = false;
    window.clearInterval(this.timer);
  }
}
