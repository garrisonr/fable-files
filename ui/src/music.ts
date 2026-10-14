// Music Player (Dickles' Music Experience, Round 6 WIP): album/playlist
// panel over the loopback media server. Plays hi-res FLAC natively through
// WebKit/GStreamer; DSF/DFF attempt the same path (GStreamer dsdparse —
// the app self-installs gst-plugins-bad; an ffmpeg decode fallback lands
// with the backend pass). Queue = the audio items handed in (a selection
// is an instant playlist). Drag-reorder + .m3u persistence and the
// dot-matrix spectrum are the next increments.

import { api, mediaSrc, type Item } from "./api";
import { audioTags, folderArtUrl } from "./audiometa";
import { Toast } from "./toast";

const AUDIO_EXTS = new Set([
  "mp3", "flac", "m4a", "m4b", "aac", "ogg", "oga", "opus", "wav", "aif", "aiff",
  "dsf", "dff", "ape", "wv", "wma",
]);

export function isAudioName(name: string): boolean {
  const dot = name.lastIndexOf(".");
  if (dot < 0) return false;
  return AUDIO_EXTS.has(name.slice(dot + 1).toLowerCase());
}

function extOf(name: string): string {
  const dot = name.lastIndexOf(".");
  return dot < 0 ? "" : name.slice(dot + 1).toLowerCase();
}

/** Parent directory of an absolute path; mirrors dirname() in shell.ts
 * (kept local: shell.ts imports this module, so importing back would
 * cycle). Never returns "" — dirname("/foo.mp3") === "/". */
function dirname(path: string): string {
  const idx = path.lastIndexOf("/");
  return idx <= 0 ? "/" : path.slice(0, idx);
}

function fmtTime(secs: number): string {
  if (!Number.isFinite(secs) || secs < 0) return "–:––";
  const m = Math.floor(secs / 60);
  const s = Math.floor(secs % 60);
  return `${m}:${String(s).padStart(2, "0")}`;
}

function cleanTitle(name: string): string {
  return name.replace(/\.[^.]+$/, "").replace(/_+/g, " ").trim();
}

function formatBadge(name: string): string {
  const e = extOf(name);
  if (e === "dsf" || e === "dff") return "DSD";
  return e.toUpperCase();
}

export class MusicPlayer {
  private overlay: HTMLDivElement | null = null;
  private audio = new Audio();
  private queue: Item[] = [];
  private index = 0;
  private durations = new Map<string, number>();
  private rows: HTMLDivElement[] = [];
  private tracksEl: HTMLDivElement | null = null;
  private dragFrom: number | null = null;
  /** Id of the item whose src was loaded most recently; a
   * loadedmetadata event for any other item is stale (Round 7). */
  private loadingItemId: string | null = null;
  private playBtn!: HTMLButtonElement;
  private seek!: HTMLInputElement;
  private timeLabel!: HTMLSpanElement;
  private badge!: HTMLSpanElement;
  private titleEl!: HTMLDivElement;
  private subEl!: HTMLDivElement;
  private onKey = (e: KeyboardEvent): void => this.handleKey(e);

  constructor() {
    this.audio.preload = "metadata";
    this.audio.addEventListener("timeupdate", () => this.syncProgress());
    this.audio.addEventListener("loadedmetadata", () => {
      // Round 7 (Copilot audit): metadata can arrive after a fast skip —
      // record it only when it belongs to the item loaded most recently,
      // or track A's duration lands on track B's row.
      const cur = this.queue[this.index];
      if (cur && cur.id === this.loadingItemId) {
        this.durations.set(cur.id, this.audio.duration);
        this.fillDuration(this.index);
      }
      this.syncProgress();
    });
    this.audio.addEventListener("ended", () => this.step(1, true));
    this.audio.addEventListener("error", () => {
      const cur = this.queue[this.index];
      if (cur && (extOf(cur.name) === "dsf" || extOf(cur.name) === "dff")) {
        Toast.error("This DSD file could not be played directly — the decode fallback arrives with the next backend pass.");
      } else {
        Toast.error("That track could not be played.");
      }
    });
    try {
      const v = parseFloat(localStorage.getItem("ff-music-vol") ?? "");
      if (Number.isFinite(v)) this.audio.volume = Math.min(1, Math.max(0, v));
    } catch { /* default volume */ }
  }

  open(items: Item[]): void {
    const audioItems = items
      .filter((i) => i.kind !== "folder" && isAudioName(i.name))
      .sort((a, b) => a.name.localeCompare(b.name, undefined, { numeric: true }));
    if (audioItems.length === 0) {
      Toast.error("No audio files in that selection.");
      return;
    }
    this.close();
    this.queue = audioItems;
    this.applySavedOrder();
    this.index = 0;
    this.durations.clear();
    this.buildPanel();
    void this.playIndex(0);
  }

  /** Folder these items live in, when they all share one (else null). */
  private queueFolder(): string | null {
    const first = this.queue[0]?.provider_key;
    if (!first) return null;
    const folder = dirname(first);
    return this.queue.every((i) => dirname(i.provider_key ?? "") === folder)
      ? folder
      : null;
  }

  // The path→id folder walk that used to live here moved to
  // audiometa.ts (inc-2 item 9) as listFolderByPath — shared with
  // Quick Look so folder art resolves identically everywhere.

  /** A saved custom order (drag-reorder) wins over name order.
   * Round 7: orders persist by item id (`ff-music-order-id:<folder>`);
   * the legacy provider_key order (`ff-music-order:<folder>`) is read
   * as a fallback so existing users keep their custom order. */
  private applySavedOrder(): void {
    const folder = this.queueFolder();
    if (!folder) return;
    try {
      const rawIds = localStorage.getItem(`ff-music-order-id:${folder}`);
      if (rawIds) {
        const saved = JSON.parse(rawIds) as string[];
        const ids = new Set(this.queue.map((i) => i.id));
        if (saved.length === this.queue.length && saved.every((id) => ids.has(id))) {
          const rank = new Map(saved.map((id, i) => [id, i]));
          this.queue.sort((a, b) => (rank.get(a.id) ?? 0) - (rank.get(b.id) ?? 0));
        }
        return;
      }
      const raw = localStorage.getItem(`ff-music-order:${folder}`);
      if (!raw) return;
      const saved = JSON.parse(raw) as string[];
      const paths = new Set(this.queue.map((i) => i.provider_key));
      if (saved.length !== this.queue.length || !saved.every((p) => paths.has(p))) return;
      const rank = new Map(saved.map((p, i) => [p, i]));
      this.queue.sort((a, b) => (rank.get(a.provider_key ?? "") ?? 0) - (rank.get(b.provider_key ?? "") ?? 0));
    } catch { /* name order stands */ }
  }

  private persistOrder(): void {
    const folder = this.queueFolder();
    if (!folder) return;
    try {
      localStorage.setItem(
        `ff-music-order-id:${folder}`,
        JSON.stringify(this.queue.map((i) => i.id)),
      );
    } catch { /* ignore */ }
  }

  private reorder(from: number, to: number): void {
    if (from === to || from < 0 || to < 0 || from >= this.queue.length || to >= this.queue.length) return;
    const playing = this.queue[this.index];
    const [moved] = this.queue.splice(from, 1);
    this.queue.splice(to, 0, moved);
    this.index = this.queue.indexOf(playing);
    this.renderRows();
    this.persistOrder();
  }

  close(): void {
    this.audio.pause();
    this.loadingItemId = null;
    this.audio.removeAttribute("src");
    this.audio.load();
    if (this.overlay) {
      this.overlay.remove();
      this.overlay = null;
    }
    this.tracksEl = null;
    this.rows = [];
    document.removeEventListener("keydown", this.onKey, true);
  }

  private buildPanel(): void {
    const overlay = document.createElement("div");
    overlay.className = "picker-overlay music-overlay";
    const compact = (() => {
      try { return localStorage.getItem("ff-music-mode") === "playlist"; } catch { return false; }
    })();
    const first = this.queue[0];
    const folder = dirname(first?.provider_key ?? "").split("/").pop() ?? "";
    overlay.innerHTML = `
      <div class="music-card${compact ? " compact" : ""}" role="dialog" aria-label="Music player">
        <div class="music-head">
          <div class="music-art"><span class="music-art-glyph">♪</span><img class="music-art-img" alt="" hidden /></div>
          <div class="music-meta">
            <div class="music-title"></div>
            <div class="music-sub"></div>
          </div>
          <button class="music-mode" title="Toggle Album / Playlist view">${compact ? "Playlist" : "Album"}</button>
          <button class="music-close" title="Close (Esc)">✕</button>
        </div>
        <div class="music-tracks"></div>
        <div class="music-bar">
          <button class="music-btn music-prev" title="Previous">⏮</button>
          <button class="music-btn music-play" title="Play / pause (Space)">▶</button>
          <button class="music-btn music-next" title="Next">⏭</button>
          <input class="music-seek" type="range" min="0" max="1000" value="0" />
          <span class="music-time">0:00 / –:––</span>
          <span class="music-badge"></span>
          <span class="music-vol-lab">Vol</span>
          <input class="music-vol" type="range" min="0" max="100" />
        </div>
      </div>`;
    document.body.appendChild(overlay);
    this.overlay = overlay;
    const q = <T extends HTMLElement>(sel: string) => overlay.querySelector(sel) as T;
    this.titleEl = q<HTMLDivElement>(".music-title");
    this.titleEl.textContent = folder || "Music";
    this.subEl = q<HTMLDivElement>(".music-sub");
    this.subEl.textContent =
      `${this.queue.length} track${this.queue.length === 1 ? "" : "s"} · drag rows to reorder`;
    this.playBtn = q<HTMLButtonElement>(".music-play");
    this.seek = q<HTMLInputElement>(".music-seek");
    this.timeLabel = q<HTMLSpanElement>(".music-time");
    this.badge = q<HTMLSpanElement>(".music-badge");
    q<HTMLInputElement>(".music-vol").value = String(Math.round(this.audio.volume * 100));

    // Album art: folder art (folder.jpg / cover.jpg / front.jpg …) wins;
    // otherwise the thumbnailer extracts embedded cover art for audio.
    const img = q<HTMLImageElement>(".music-art-img");
    // A failed art load must fall back to the glyph, never a broken
    // image box (fix block item 5).
    img.addEventListener("error", () => {
      img.hidden = true;
      q<HTMLSpanElement>(".music-art-glyph").hidden = false;
    });
    if (first) {
      void (async () => {
        const show = (url: string) => {
          img.src = url;
          img.hidden = false;
          q<HTMLSpanElement>(".music-art-glyph").hidden = true;
        };
        try {
          const folder = this.queueFolder();
          if (folder) {
            // Shared with Quick Look (inc-2 item 9): audiometa's
            // folderArtUrl owns the art-name set + path→id walk.
            const url = await folderArtUrl(first, folder);
            if (url) {
              show(url);
              return;
            }
          }
        } catch { /* fall through to embedded art */ }
        try {
          const url = await api.getThumbnail(first.id, first.provider_key, true);
          if (url) show(url);
        } catch { /* glyph stays */ }
      })();
    }

    this.tracksEl = q<HTMLDivElement>(".music-tracks");
    this.renderRows();

    q<HTMLButtonElement>(".music-close").addEventListener("click", () => this.close());
    overlay.addEventListener("pointerdown", (e) => {
      if (e.target === overlay) this.close();
    });
    this.playBtn.addEventListener("click", () => this.toggle());
    q<HTMLButtonElement>(".music-prev").addEventListener("click", () => this.step(-1, true));
    q<HTMLButtonElement>(".music-next").addEventListener("click", () => this.step(1, true));
    this.seek.addEventListener("input", () => {
      const d = this.audio.duration;
      if (Number.isFinite(d) && d > 0) {
        this.audio.currentTime = (Number(this.seek.value) / 1000) * d;
      }
    });
    q<HTMLInputElement>(".music-vol").addEventListener("input", (e) => {
      const v = Number((e.target as HTMLInputElement).value) / 100;
      this.audio.volume = v;
      try { localStorage.setItem("ff-music-vol", String(v)); } catch { /* ignore */ }
    });
    q<HTMLButtonElement>(".music-mode").addEventListener("click", (e) => {
      const card = q<HTMLDivElement>(".music-card");
      const nowCompact = card.classList.toggle("compact");
      (e.target as HTMLButtonElement).textContent = nowCompact ? "Playlist" : "Album";
      try { localStorage.setItem("ff-music-mode", nowCompact ? "playlist" : "album"); } catch { /* ignore */ }
    });
    document.addEventListener("keydown", this.onKey, true);
  }

  private async playIndex(i: number): Promise<void> {
    if (i < 0 || i >= this.queue.length) return;
    this.index = i;
    const item = this.queue[i];
    this.loadingItemId = item.id;
    const path = item.provider_key ?? item.name;
    this.badge.textContent = formatBadge(item.name);
    // Headline + subline from embedded tags (inc-2 item 9): the
    // Title tag is the headline, Artist · Album the subline; the
    // cleaned filename is only the fallback.
    this.titleEl.textContent = cleanTitle(item.name);
    void audioTags(item).then((tags) => {
      if (this.index !== i) return;
      if (tags.title) this.titleEl.textContent = tags.title;
      const sub = [tags.artist, tags.album]
        .filter((s): s is string => !!s)
        .join(" · ");
      if (sub && this.subEl) this.subEl.textContent = sub;
    });
    this.rows.forEach((r, j) => r.classList.toggle("playing", j === i));
    try {
      this.audio.src = await mediaSrc(path);
      await this.audio.play();
    } catch {
      // error event / toast path handles messaging
    }
    this.syncPlayGlyph();
  }

  private step(dir: 1 | -1, autoplay: boolean): void {
    const next = this.index + dir;
    if (next < 0 || next >= this.queue.length) {
      if (dir === 1) this.audio.pause();
      return;
    }
    void this.playIndex(next).then(() => {
      if (!autoplay) this.audio.pause();
    });
  }

  private toggle(): void {
    if (this.audio.paused) void this.audio.play().then(() => this.syncPlayGlyph());
    else {
      this.audio.pause();
      this.syncPlayGlyph();
    }
  }

  private syncPlayGlyph(): void {
    if (this.playBtn) this.playBtn.textContent = this.audio.paused ? "▶" : "❚❚";
  }

  private syncProgress(): void {
    const d = this.audio.duration;
    const c = this.audio.currentTime;
    if (this.seek && Number.isFinite(d) && d > 0) {
      this.seek.value = String(Math.round((c / d) * 1000));
    }
    if (this.timeLabel) {
      this.timeLabel.textContent = `${fmtTime(c)} / ${fmtTime(d)}`;
    }
    this.syncPlayGlyph();
  }

  private fillDuration(i: number): void {
    const row = this.rows[i];
    const item = this.queue[i];
    const d = item ? this.durations.get(item.id) : undefined;
    if (row && d) {
      (row.querySelector(".music-d") as HTMLSpanElement).textContent = fmtTime(d);
    }
  }

  /** (Re)build the track rows — used on open and after a drag-reorder. */
  private renderRows(): void {
    if (!this.tracksEl) return;
    this.tracksEl.innerHTML = "";
    this.rows = this.queue.map((item, i) => {
      const row = document.createElement("div");
      row.className = "music-row" + (i === this.index ? " playing" : "");
      row.draggable = true;
      row.innerHTML = `
        <span class="music-n">${i + 1}</span>
        <span class="music-t"></span>
        <span class="music-fmt"></span>
        <span class="music-d">–:––</span>`;
      (row.querySelector(".music-t") as HTMLSpanElement).textContent = cleanTitle(item.name);
      // Embedded Title tag replaces the filename when present
      // (inc-2 item 9); reads are cached per item in audiometa.
      void audioTags(item).then((tags) => {
        if (!tags.title) return;
        const t = row.querySelector(".music-t") as HTMLSpanElement | null;
        if (t) t.textContent = tags.title;
      });
      (row.querySelector(".music-fmt") as HTMLSpanElement).textContent = formatBadge(item.name);
      const known = this.durations.get(item.id);
      if (known) {
        (row.querySelector(".music-d") as HTMLSpanElement).textContent = fmtTime(known);
      }
      row.addEventListener("click", () => void this.playIndex(i));
      row.addEventListener("dragstart", (e) => {
        this.dragFrom = i;
        row.classList.add("dragging");
        if (e.dataTransfer) e.dataTransfer.effectAllowed = "move";
      });
      row.addEventListener("dragend", () => {
        this.dragFrom = null;
        this.rows.forEach((r) => r.classList.remove("dragging", "drop-before", "drop-after"));
      });
      row.addEventListener("dragover", (e) => {
        if (this.dragFrom === null || this.dragFrom === i) return;
        e.preventDefault();
        const rect = row.getBoundingClientRect();
        const after = e.clientY > rect.top + rect.height / 2;
        row.classList.toggle("drop-after", after);
        row.classList.toggle("drop-before", !after);
      });
      row.addEventListener("dragleave", () => {
        row.classList.remove("drop-before", "drop-after");
      });
      row.addEventListener("drop", (e) => {
        e.preventDefault();
        if (this.dragFrom === null) return;
        const rect = row.getBoundingClientRect();
        const after = e.clientY > rect.top + rect.height / 2;
        let to = i + (after ? 1 : 0);
        if (to > this.dragFrom) to -= 1;
        // Clamp into range: dropping after the last row moves the item
        // to the end instead of silently doing nothing (Round 7).
        to = Math.max(0, Math.min(this.queue.length - 1, to));
        const from = this.dragFrom;
        this.dragFrom = null;
        this.reorder(from, to);
      });
      this.tracksEl!.appendChild(row);
      return row;
    });
  }

  private handleKey(e: KeyboardEvent): void {
    if (!this.overlay) return;
    const target = e.target as HTMLElement;
    if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA") && !target.classList.contains("music-seek") && !target.classList.contains("music-vol")) return;
    if (e.key === "Escape") {
      e.preventDefault();
      this.close();
    } else if (e.key === " ") {
      e.preventDefault();
      this.toggle();
    } else if (e.key === "ArrowRight") {
      this.audio.currentTime = Math.min(this.audio.duration || 0, this.audio.currentTime + 5);
    } else if (e.key === "ArrowLeft") {
      this.audio.currentTime = Math.max(0, this.audio.currentTime - 5);
    }
  }
}
