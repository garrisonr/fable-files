// Music Player (Music Glow-Up Phase 2, crew Diva): the Panel
// arrangement of the one shared transport core (src-tauri
// player.rs). The panel owns no playback truth of its own — the
// queue, order, position clock, shuffle/repeat, and the single
// owner live in Rust; this surface renders snapshots and drives
// its OwnerEngine's element. Detach hands the sound to the Mini
// or the Stage by claim, never by copying state, and starting
// playback anywhere (here, Quick Look, a window) revokes the
// previous owner — previews can no longer talk over each other.
//
// Kept from the Round 6/7 panel, because they were right: tag-led
// headlines (audiometa), folder/embedded art resolution, saved
// per-folder row order, drag reorder, Album/Playlist modes.

import {
  api,
  onPlayerClosed,
  type Item,
  type PlayerSnapshot,
  type QueueEntry,
} from "./api";
import {
  audioBadge,
  audioProps,
  audioTags,
  folderArtUrl,
  type AudioProps,
} from "./audiometa";
import {
  OwnerEngine,
  PlayerCoreClient,
  SpectrumCanvas,
  SpectrumFeed,
  TransportBar,
  fmtClock,
  fmtTime,
  onPlayerSpectrum,
} from "./playercore";
import { LyricsView } from "./lyrics";
import type { CastDeviceLike } from "./castsession";
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

function isPlaylistName(name: string): boolean {
  return name.toLowerCase().endsWith(".m3u");
}

/** Parent directory of an absolute path; mirrors dirname() in
 * shell.ts (kept local: shell.ts imports this module). */
function dirname(path: string): string {
  const idx = path.lastIndexOf("/");
  return idx <= 0 ? "/" : path.slice(0, idx);
}

function basename(path: string): string {
  const idx = path.lastIndexOf("/");
  return idx < 0 ? path : path.slice(idx + 1);
}

function cleanTitle(name: string): string {
  return name.replace(/\.[^.]+$/, "").replace(/_+/g, " ").trim();
}

export class MusicPlayer {
  private client = new PlayerCoreClient();
  private engine: OwnerEngine;
  private overlay: HTMLDivElement | null = null;
  /** Items behind the panel's queue, by provider_key. */
  private itemsByKey = new Map<string, Item>();
  /** Tag readouts per provider_key (header + MediaSession). */
  private tagsByKey = new Map<string, { title?: string; artist?: string; album?: string }>();
  /** Durations learned locally (catalog + props + plays). */
  private durations = new Map<string, number>();
  private propsByKey = new Map<string, AudioProps>();
  private bar: TransportBar | null = null;
  private lyrics = new LyricsView();
  private lyricsExpanded = false;
  private lyricsHas = false;
  private spectrum = new SpectrumCanvas();
  private feed: SpectrumFeed;
  private spectrumOn = false;
  private rows: HTMLDivElement[] = [];
  private tracksEl: HTMLDivElement | null = null;
  private railEl: HTMLDivElement | null = null;
  private titleEl: HTMLDivElement | null = null;
  private subEl: HTMLDivElement | null = null;
  private badgeEl: HTMLSpanElement | null = null;
  private metaEl: HTMLDivElement | null = null;
  private bannerEl: HTMLDivElement | null = null;
  private stripEl: HTMLDivElement | null = null;
  private setupEl: HTMLDivElement | null = null;
  private dragFrom: number | null = null;
  /** Structural render key: rebuild rows only when this moves. */
  private renderKey = "";
  private lyricsKey = "";
  private recentKey = "";
  private castDlg: { openPicker: (n: string, v: boolean) => Promise<CastDeviceLike | null>; element: HTMLDivElement } | null = null;
  private detachWatch: number | null = null;
  private setupChecked = false;
  private setupBusy = false;
  private onKey = (e: KeyboardEvent): void => this.handleKey(e);

  constructor() {
    void this.client.start();
    this.engine = new OwnerEngine("panel", this.client, {
      castPicker: (name, isVideo) => this.castPicker(name, isVideo),
      onSnapshot: (s) => this.render(s),
      onError: (m) => Toast.error(m),
    });
    this.feed = new SpectrumFeed(this.engine, () => this.client.isOwner("panel"));
    try {
      this.spectrumOn = localStorage.getItem("ff-music-spectrum") === "1";
    } catch { /* default off */ }
    // Media keys + OS now-playing drive the core from here.
    this.client.installMediaSession(() => {
      const e = this.client.currentEntry();
      const tags = e ? this.tagsByKey.get(e.providerKey) : undefined;
      return {
        title: tags?.title ?? (e ? cleanTitle(e.name) : "Fable Files"),
        artist: tags?.artist,
        album: tags?.album,
        artwork: this.currentArt ?? undefined,
      };
    });
    // The detached window dying without a return (tombstone
    // backstop, fix batch item 4): the core lapses ownership and
    // this panel — when open — claims the sound back.
    void onPlayerClosed((path) => {
      const s = this.client.snap;
      if (!s || (s.owner !== "mini" && s.owner !== "stage")) return;
      const cur = this.client.currentEntry();
      if (!cur || cur.providerKey !== path) return;
      void api.playerDetachGone().then(() => {
        if (this.overlay) void this.client.claim("panel");
      }).catch(() => undefined);
    }).catch(() => undefined);
    void onPlayerSpectrum((bands) => {
      if (this.overlay && this.spectrumOn) this.spectrum.draw(bands);
    }).catch(() => undefined);
    this.lyrics.onSeek = (secs) => void this.engine.seekTo(secs);
  }

  private currentArt: string | null = null;

  private async castPicker(
    name: string,
    isVideo: boolean,
  ): Promise<CastDeviceLike | null> {
    if (!this.castDlg) {
      try {
        const { CastDialog } = await import("./cast");
        this.castDlg = new CastDialog();
        document.body.appendChild(this.castDlg.element);
      } catch {
        return null;
      }
    }
    return this.castDlg.openPicker(name, isVideo);
  }

  // ---------------------------------------------------------------
  // Opening
  // ---------------------------------------------------------------

  open(items: Item[]): void {
    const playlist = items.find((i) => isPlaylistName(i.name));
    if (playlist?.provider_key) {
      void this.openPlaylistFile(playlist);
      return;
    }
    const audioItems = items
      .filter((i) => i.kind !== "folder" && isAudioName(i.name))
      .sort((a, b) => a.name.localeCompare(b.name, undefined, { numeric: true }));
    if (audioItems.length === 0) {
      Toast.error("No audio files in that selection.");
      return;
    }
    this.close();
    this.itemsByKey = new Map(
      audioItems.map((i) => [i.provider_key ?? i.name, i]),
    );
    const ordered = this.applySavedOrder([...audioItems]);
    void (async () => {
      const entries = await this.buildEntries(ordered);
      this.buildPanel();
      await this.client.openQueue({
        owner: "panel",
        entries,
        startIndex: 0,
        autoplay: true,
      });
      void this.checkPlaybackSetup();
    })();
  }

  /** Add to the live queue from a file context action (the shell
   * routes here): Play next, or append. */
  enqueue(items: Item[], next: boolean): void {
    const audioItems = items.filter(
      (i) => i.kind !== "folder" && isAudioName(i.name),
    );
    if (audioItems.length === 0) return;
    for (const i of audioItems) {
      this.itemsByKey.set(i.provider_key ?? i.name, i);
    }
    void (async () => {
      const entries = await this.buildEntries(audioItems);
      await this.client.enqueue(entries, next);
      Toast.success(
        next
          ? `Queued next: ${cleanTitle(audioItems[0].name)}`
          : `Added ${entries.length} to the queue`,
      );
    })();
  }

  private async buildEntries(items: Item[]): Promise<QueueEntry[]> {
    const paths = items.map((i) => i.provider_key ?? i.name);
    let catalogDurs: (number | null)[] = [];
    try {
      catalogDurs = await api.audioDurations(paths);
    } catch {
      catalogDurs = [];
    }
    return items.map((item, i) => {
      const key = item.provider_key ?? item.name;
      const d =
        catalogDurs[i] ?? this.durations.get(item.id) ?? null;
      if (d) this.durations.set(item.id, d);
      return {
        providerKey: key,
        name: item.name,
        durationSecs: d,
      };
    });
  }

  /** A saved custom order (drag-reorder) wins over name order —
   * persisted by item id per folder, exactly as the Round 7
   * panel had it. */
  private applySavedOrder(items: Item[]): Item[] {
    const folder = this.folderOf(items);
    if (!folder) return items;
    try {
      const rawIds = localStorage.getItem(`ff-music-order-id:${folder}`);
      if (rawIds) {
        const saved = JSON.parse(rawIds) as string[];
        const ids = new Set(items.map((i) => i.id));
        if (saved.length === items.length && saved.every((id) => ids.has(id))) {
          const rank = new Map(saved.map((id, i) => [id, i]));
          return [...items].sort(
            (a, b) => (rank.get(a.id) ?? 0) - (rank.get(b.id) ?? 0),
          );
        }
        return items;
      }
      const raw = localStorage.getItem(`ff-music-order:${folder}`);
      if (!raw) return items;
      const saved = JSON.parse(raw) as string[];
      const paths = new Set(items.map((i) => i.provider_key));
      if (saved.length !== items.length || !saved.every((p) => paths.has(p))) {
        return items;
      }
      const rank = new Map(saved.map((p, i) => [p, i]));
      return [...items].sort(
        (a, b) =>
          (rank.get(a.provider_key ?? "") ?? 0) -
          (rank.get(b.provider_key ?? "") ?? 0),
      );
    } catch {
      return items;
    }
  }

  private folderOf(items: Item[]): string | null {
    const first = items[0]?.provider_key;
    if (!first) return null;
    const folder = dirname(first);
    return items.every((i) => dirname(i.provider_key ?? "") === folder)
      ? folder
      : null;
  }

  private persistOrder(queue: QueueEntry[]): void {
    const items = queue
      .map((e) => this.itemsByKey.get(e.providerKey))
      .filter((i): i is Item => !!i);
    const folder = this.folderOf(items);
    if (!folder || items.length !== queue.length) return;
    try {
      localStorage.setItem(
        `ff-music-order-id:${folder}`,
        JSON.stringify(items.map((i) => i.id)),
      );
    } catch { /* ignore */ }
  }

  close(): void {
    // Closing the panel stops the sound only when the panel owns
    // it — a detached Mini/Stage keeps playing (its window IS
    // the surface now), exactly as the old detach contract had it.
    const s = this.client.snap;
    if (s && s.owner === "panel") {
      void this.client.stop().catch(() => undefined);
    }
    this.stopDetachWatch();
    if (this.overlay) {
      this.overlay.remove();
      this.overlay = null;
    }
    this.tracksEl = null;
    this.railEl = null;
    this.rows = [];
    this.renderKey = "";
    this.lyricsKey = "";
    document.removeEventListener("keydown", this.onKey, true);
  }

  // ---------------------------------------------------------------
  // Panel construction
  // ---------------------------------------------------------------

  private buildPanel(): void {
    const overlay = document.createElement("div");
    overlay.className = "picker-overlay music-overlay";
    const compact = (() => {
      try { return localStorage.getItem("ff-music-mode") === "playlist"; } catch { return false; }
    })();
    overlay.innerHTML = `
      <div class="music-card${compact ? " compact" : ""}" role="dialog" aria-label="Music player">
        <div class="music-head">
          <div class="music-art"><span class="music-art-glyph">♪</span><img class="music-art-img" alt="" hidden /></div>
          <div class="music-meta">
            <div class="music-title"></div>
            <div class="music-sub"></div>
            <div class="music-meta-line"></div>
          </div>
          <span class="music-badge"></span>
          <button class="music-mode" title="Toggle Album / Playlist view">${compact ? "Playlist" : "Album"}</button>
          <button class="music-stage" title="Open the Stage window">⤢</button>
          <button class="music-detach" title="Detach into a mini window">⧉</button>
          <button class="music-close" title="Close (Esc)">✕</button>
        </div>
        <div class="music-setup" hidden></div>
        <div class="music-banner" hidden></div>
        <div class="music-main">
          <div class="music-tracks"></div>
          <div class="music-rail"></div>
        </div>
        <div class="music-lyrics-strip" title="Show lyrics"></div>
        <div class="music-spectrum" hidden></div>
        <div class="music-bar-host"></div>
      </div>`;
    document.body.appendChild(overlay);
    this.overlay = overlay;
    const q = <T extends HTMLElement>(sel: string) =>
      overlay.querySelector(sel) as T;
    this.titleEl = q<HTMLDivElement>(".music-title");
    this.subEl = q<HTMLDivElement>(".music-sub");
    this.metaEl = q<HTMLDivElement>(".music-meta-line");
    this.badgeEl = q<HTMLSpanElement>(".music-badge");
    this.bannerEl = q<HTMLDivElement>(".music-banner");
    this.setupEl = q<HTMLDivElement>(".music-setup");
    this.tracksEl = q<HTMLDivElement>(".music-tracks");
    this.railEl = q<HTMLDivElement>(".music-rail");
    this.stripEl = q<HTMLDivElement>(".music-lyrics-strip");
    const spectrumHost = q<HTMLDivElement>(".music-spectrum");
    spectrumHost.appendChild(this.spectrum.el);
    spectrumHost.hidden = !this.spectrumOn;
    // Lyrics full view lives under the strip, collapsed by default.
    this.lyrics.el.classList.add("music-lyrics-full");
    this.lyrics.el.hidden = true;
    overlay
      .querySelector(".music-card")
      ?.insertBefore(this.lyrics.el, q<HTMLDivElement>(".music-bar-host"));
    this.stripEl.addEventListener("click", () => {
      this.lyricsExpanded = !this.lyricsExpanded;
      this.lyrics.el.hidden = !this.lyricsExpanded;
    });
    // The one transport.
    this.bar = new TransportBar(
      {
        onToggle: () => void this.engine.toggle(),
        onStep: (d) => void this.client.step(d),
        onSeek: (s) => void this.engine.seekTo(s),
        onRepeat: () => void this.cycleRepeat(),
        onShuffle: () => {
          const s = this.client.snap;
          if (s) void this.client.setShuffle(!s.shuffle);
        },
        onVolume: (v) => void this.client.setVolume(v),
        onMute: () => {
          const s = this.client.snap;
          if (s) void this.client.setMuted(!s.muted);
        },
        onCast: () => void this.engine.startCastFlow(),
      },
      {},
    );
    const barHost = q<HTMLDivElement>(".music-bar-host");
    barHost.appendChild(this.bar.el);
    const specBtn = document.createElement("button");
    specBtn.className = "tbar-btn music-spec-btn" + (this.spectrumOn ? " on" : "");
    specBtn.textContent = "▦";
    specBtn.title = "Dot-matrix spectrum";
    specBtn.addEventListener("click", () => this.toggleSpectrum(specBtn, spectrumHost));
    this.bar.el.appendChild(specBtn);
    // Header buttons.
    q<HTMLButtonElement>(".music-close").addEventListener("click", () => this.close());
    overlay.addEventListener("pointerdown", (e) => {
      if (e.target === overlay) this.close();
    });
    q<HTMLButtonElement>(".music-mode").addEventListener("click", (e) => {
      const card = q<HTMLDivElement>(".music-card");
      const nowCompact = card.classList.toggle("compact");
      (e.target as HTMLButtonElement).textContent = nowCompact ? "Playlist" : "Album";
      try { localStorage.setItem("ff-music-mode", nowCompact ? "playlist" : "album"); } catch { /* ignore */ }
    });
    q<HTMLButtonElement>(".music-detach").addEventListener("click", () => this.detach());
    q<HTMLButtonElement>(".music-stage").addEventListener("click", () => this.openStage());
    document.addEventListener("keydown", this.onKey, true);
    // Album art for the folder (shared resolution with Quick Look).
    void this.resolveFolderArt();
    const s = this.client.snap;
    if (s) this.render(s);
  }

  private async resolveFolderArt(): Promise<void> {
    const first = [...this.itemsByKey.values()][0];
    if (!first) return;
    const img = this.overlay?.querySelector<HTMLImageElement>(".music-art-img");
    const glyph = this.overlay?.querySelector<HTMLSpanElement>(".music-art-glyph");
    if (!img || !glyph) return;
    img.addEventListener("error", () => {
      img.hidden = true;
      glyph.hidden = false;
      this.currentArt = null;
    });
    const show = (url: string) => {
      img.src = url;
      img.hidden = false;
      glyph.hidden = true;
      this.currentArt = url;
    };
    try {
      const folder = this.folderOf([...this.itemsByKey.values()]);
      if (folder) {
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
  }

  // ---------------------------------------------------------------
  // Rendering from snapshots
  // ---------------------------------------------------------------

  private render(s: PlayerSnapshot): void {
    if (!this.overlay) return;
    this.bar?.update(s);
    // Lyrics follow the position clock; the strip shows the line
    // being sung right now (tap it for the full pane).
    if (this.lyrics.isSynced) {
      this.lyrics.setPosition(s.positionSecs * 1000);
    }
    if (this.stripEl) {
      const has = this.lyricsHas;
      this.stripEl.hidden = !has;
      this.stripEl.textContent = has
        ? (this.lyrics.activeText() ?? "Lyrics")
        : "";
    }
    // Header meta for the current entry.
    const entry = s.index !== null ? s.queue[s.index] : null;
    if (entry && this.titleEl && this.subEl && this.badgeEl && this.metaEl) {
      const item = this.itemsByKey.get(entry.providerKey);
      this.titleEl.textContent = cleanTitle(entry.name);
      this.badgeEl.textContent = audioBadge(
        entry.name,
        this.propsByKey.get(entry.providerKey),
      );
      if (item) {
        void audioTags(item).then((tags) => {
          this.tagsByKey.set(entry.providerKey, tags);
          if (this.client.currentEntry()?.providerKey !== entry.providerKey) return;
          if (!this.titleEl || !this.subEl || !this.metaEl) return;
          if (tags.title) this.titleEl.textContent = tags.title;
          const sub = [tags.artist, tags.album]
            .filter((x): x is string => !!x)
            .join(" · ");
          if (sub) this.subEl.textContent = sub;
          const meta = [tags.year, tags.genre]
            .filter((x): x is string => !!x)
            .join(" · ");
          this.metaEl.textContent = meta;
        });
        void audioProps(item).then((props) => {
          this.propsByKey.set(entry.providerKey, props);
          if (props.durationSecs && !this.durations.has(item.id)) {
            this.durations.set(item.id, props.durationSecs);
          }
          if (this.badgeEl && this.client.currentEntry()?.providerKey === entry.providerKey) {
            this.badgeEl.textContent = audioBadge(entry.name, props);
          }
          this.renderKey = ""; // let rows pick up props/durations
          const fresh = this.client.snap;
          if (fresh) this.renderRows(fresh);
        });
      }
    }
    // Banner: the sound lives in another window.
    if (this.bannerEl) {
      if (s.owner === "mini" || s.owner === "stage") {
        this.bannerEl.hidden = false;
        this.bannerEl.innerHTML = "";
        const span = document.createElement("span");
        span.textContent =
          s.owner === "mini"
            ? "Playing in the detached mini window — the transport here drives it too."
            : "Playing on the Stage — the transport here drives it too.";
        const btn = document.createElement("button");
        btn.className = "pill-btn";
        btn.textContent = "Bring it back here";
        btn.addEventListener("click", () => {
          void this.client.claim("panel");
          if (entry) void api.closePlayerWindow(entry.providerKey).catch(() => undefined);
        });
        this.bannerEl.append(span, btn);
      } else {
        this.bannerEl.hidden = true;
      }
    }
    // Lyrics load per entry.
    const lkey = entry ? `${entry.providerKey}:${s.playToken}` : "";
    if (entry && lkey !== this.lyricsKey) {
      this.lyricsKey = lkey;
      const wasExpanded = this.lyricsExpanded;
      void this.lyrics.load(entry.providerKey).then((has) => {
        this.lyricsHas = has;
        if (this.stripEl) this.stripEl.hidden = !has;
        this.lyrics.el.hidden = !(has && wasExpanded);
      });
    }
    // Detached tombstone watch runs while a window owns the sound.
    if (s.owner === "mini" || s.owner === "stage") this.startDetachWatch();
    else this.stopDetachWatch();
    this.renderRows(s);
    this.renderRail(s);
  }

  private renderRows(s: PlayerSnapshot): void {
    if (!this.tracksEl) return;
    const key = [
      s.queue.map((e) => e.providerKey).join("|"),
      s.order.join(","),
      s.index ?? -1,
      s.owner ?? "",
    ].join("::");
    if (key === this.renderKey) return;
    this.renderKey = key;
    this.tracksEl.innerHTML = "";
    this.rows = s.queue.map((entry, i) => {
      const row = document.createElement("div");
      row.className = "music-row" + (i === s.index ? " playing" : "");
      row.draggable = true;
      const item = this.itemsByKey.get(entry.providerKey);
      const props = this.propsByKey.get(entry.providerKey);
      const dur =
        entry.durationSecs ??
        (item ? this.durations.get(item.id) : undefined) ??
        props?.durationSecs;
      row.innerHTML = `
        <span class="music-n">${i + 1}</span>
        <span class="music-t"></span>
        <span class="music-fmt"></span>
        <span class="music-bit"></span>
        <span class="music-sr"></span>
        <span class="music-d">${dur ? fmtTime(dur) : "–:––"}</span>
        <button class="music-row-x" title="Remove from queue">✕</button>`;
      const t = row.querySelector(".music-t") as HTMLSpanElement;
      t.textContent = cleanTitle(entry.name);
      (row.querySelector(".music-fmt") as HTMLSpanElement).textContent =
        audioBadge(entry.name, props);
      (row.querySelector(".music-bit") as HTMLSpanElement).textContent =
        props?.bitsPerSample ? `${props.bitsPerSample}-bit` : "";
      (row.querySelector(".music-sr") as HTMLSpanElement).textContent =
        props?.sampleRate ? `${(props.sampleRate / 1000).toFixed(props.sampleRate % 1000 === 0 ? 0 : 1)} kHz` : "";
      if (item) {
        void audioTags(item).then((tags) => {
          if (tags.title) t.textContent = tags.title;
        });
        if (!props) {
          void audioProps(item).then((p) => {
            this.propsByKey.set(entry.providerKey, p);
            this.renderKey = "";
            const cur = this.client.snap;
            if (cur) this.renderRows(cur);
          });
        }
      }
      row.addEventListener("click", (e) => {
        if ((e.target as HTMLElement).closest(".music-row-x")) return;
        void this.playRow(i, entry);
      });
      (row.querySelector(".music-row-x") as HTMLButtonElement).addEventListener(
        "click",
        () => void this.client.remove(i),
      );
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
        to = Math.max(0, Math.min(s.queue.length - 1, to));
        const from = this.dragFrom;
        this.dragFrom = null;
        void this.client.reorder(from, to).then((snap) => {
          this.persistOrder(snap.queue);
        });
      });
      this.tracksEl!.appendChild(row);
      return row;
    });
  }

  /** Row click: a saved long-form position resumes (the pinned
   * rule lives in the core glue; an explicit start position
   * here simply honors what the row offered). */
  private async playRow(i: number, entry: QueueEntry): Promise<void> {
    try {
      const st = await api.audioPlayState(entry.providerKey);
      const dur = entry.durationSecs ?? undefined;
      const longForm =
        (dur !== undefined && dur >= 600) ||
        entry.name.toLowerCase().endsWith(".m4b");
      if (
        longForm &&
        st.lastPositionSecs > 5 &&
        (dur === undefined || st.lastPositionSecs < dur - 10)
      ) {
        const s = this.client.snap;
        if (s) {
          await this.client.openQueue({
            owner: "panel",
            entries: s.queue,
            startIndex: i,
            startPosition: st.lastPositionSecs,
            autoplay: true,
          });
          return;
        }
      }
    } catch { /* fall through to the plain play */ }
    await this.client.playIndex(i);
  }

  private renderRail(s: PlayerSnapshot): void {
    if (!this.railEl) return;
    const currentPos = s.index !== null ? s.order.indexOf(s.index) : -1;
    const upNext = s.order
      .slice(currentPos + 1)
      .map((qi) => ({ qi, entry: s.queue[qi] }))
      .filter((x) => !!x.entry);
    const key =
      "rail:" +
      upNext.map((x) => x.qi).join(",") +
      ":" +
      (s.index ?? -1);
    if (key === this.recentKey) return;
    this.recentKey = key;
    this.railEl.innerHTML = "";
    const head = document.createElement("div");
    head.className = "music-rail-head";
    const h = document.createElement("span");
    h.textContent = "Up Next";
    head.appendChild(h);
    const save = document.createElement("button");
    save.className = "music-rail-btn";
    save.textContent = "Save .m3u";
    save.title = "Save this queue as an .m3u beside the album";
    save.addEventListener("click", () => void this.saveM3u(s));
    const load = document.createElement("button");
    load.className = "music-rail-btn";
    load.textContent = "Load .m3u";
    load.title = "Load an .m3u from this folder as the queue";
    load.addEventListener("click", () => void this.loadM3u());
    const clear = document.createElement("button");
    clear.className = "music-rail-btn";
    clear.textContent = "Clear";
    clear.title = "Clear everything after the current track";
    clear.addEventListener("click", () =>
      void this.client.clearQueue(true),
    );
    head.append(save, load, clear);
    this.railEl.appendChild(head);
    if (upNext.length === 0) {
      const empty = document.createElement("div");
      empty.className = "music-rail-empty";
      empty.textContent = "Nothing queued after this.";
      this.railEl.appendChild(empty);
    }
    for (const { qi, entry } of upNext.slice(0, 30)) {
      const row = document.createElement("div");
      row.className = "music-rail-row";
      const t = document.createElement("span");
      t.className = "music-rail-t";
      t.textContent = cleanTitle(entry.name);
      t.addEventListener("click", () => void this.client.playIndex(qi));
      const x = document.createElement("button");
      x.className = "music-row-x";
      x.textContent = "✕";
      x.title = "Remove from queue";
      x.addEventListener("click", () => void this.client.remove(qi));
      row.append(t, x);
      this.railEl.appendChild(row);
    }
    // Recently Played (the history hook's face).
    void api
      .audioRecentlyPlayed(10)
      .then((recent) => {
        if (!this.railEl || !recent.length) return;
        const h2 = document.createElement("div");
        h2.className = "music-rail-head";
        h2.innerHTML = "<span>Recently Played</span>";
        this.railEl.appendChild(h2);
        for (const r of recent) {
          if (s.queue.some((e) => e.providerKey === r.providerKey)) continue;
          const row = document.createElement("div");
          row.className = "music-rail-row recent";
          const t = document.createElement("span");
          t.className = "music-rail-t";
          const resume =
            r.state.lastPositionSecs > 5
              ? ` · resume at ${fmtClock(r.state.lastPositionSecs)}`
              : "";
          t.textContent = `${cleanTitle(basename(r.providerKey))} — played ${r.state.playCount}×${resume}`;
          t.title = "Queue this next";
          t.addEventListener("click", () => {
            void this.client.enqueue(
              [
                {
                  providerKey: r.providerKey,
                  name: basename(r.providerKey),
                  durationSecs: null,
                },
              ],
              true,
            );
            Toast.success("Queued next.");
          });
          row.appendChild(t);
          this.railEl.appendChild(row);
        }
      })
      .catch(() => undefined);
  }

  // ---------------------------------------------------------------
  // Detach / Stage / tombstone
  // ---------------------------------------------------------------

  /** Detach the current track into the Mini window: the window
   * boots from the core's snapshot and claims ownership — the
   * sound moves, nothing restarts. */
  private detach(): void {
    const s = this.client.snap;
    const entry = this.client.currentEntry();
    if (!entry) return;
    if (s?.owner === "mini") {
      // Already there: the button brings it home instead.
      void this.client.claim("panel");
      void api.closePlayerWindow(entry.providerKey).catch(() => undefined);
      return;
    }
    const artImg =
      this.overlay?.querySelector<HTMLImageElement>(".music-art-img");
    const art =
      artImg && !artImg.hidden && artImg.src.startsWith("data:")
        ? artImg.src
        : null;
    void api
      .openPlayerWindow(entry.providerKey, entry.name, {
        kind: "audio",
        start: s?.positionSecs ?? 0,
        autoplay: s?.state === "playing",
        art,
      })
      .catch(() => undefined);
  }

  private openStage(): void {
    const entry = this.client.currentEntry();
    if (!entry) return;
    void api
      .openPlayerWindow(entry.providerKey, entry.name, {
        kind: "audio-stage",
        start: this.client.snap?.positionSecs ?? 0,
        autoplay: this.client.snap?.state === "playing",
        art: null,
      })
      .catch(() => undefined);
  }

  private startDetachWatch(): void {
    if (this.detachWatch !== null) return;
    this.detachWatch = window.setInterval(() => {
      const s = this.client.snap;
      const cur = this.client.currentEntry();
      if (!s || (s.owner !== "mini" && s.owner !== "stage") || !cur) {
        this.stopDetachWatch();
        return;
      }
      if (s.owner === "stage") return; // the stage has its own close path
      void api
        .playerWindowOpen(cur.providerKey)
        .then((open) => {
          if (!open) {
            void api.playerDetachGone().then(() => {
              if (this.overlay) void this.client.claim("panel");
            });
          }
        })
        .catch(() => undefined);
    }, 1200);
  }

  private stopDetachWatch(): void {
    if (this.detachWatch !== null) {
      window.clearInterval(this.detachWatch);
      this.detachWatch = null;
    }
  }

  // ---------------------------------------------------------------
  // Spectrum toggle
  // ---------------------------------------------------------------

  private toggleSpectrum(btn: HTMLButtonElement, host: HTMLDivElement): void {
    this.spectrumOn = !this.spectrumOn;
    try {
      localStorage.setItem("ff-music-spectrum", this.spectrumOn ? "1" : "0");
    } catch { /* ignore */ }
    btn.classList.toggle("on", this.spectrumOn);
    host.hidden = !this.spectrumOn;
    if (this.spectrumOn) {
      if (!this.feed.enable()) {
        // The spike's honest gate: if the WebAudio tap misbehaves
        // on this stack, the toggle says so instead of faking it.
        Toast.error(
          this.feed.failed ?? "The spectrum is unavailable on this system.",
        );
        this.spectrumOn = false;
        btn.classList.remove("on");
        host.hidden = true;
      }
    } else {
      this.feed.disable();
    }
  }

  // ---------------------------------------------------------------
  // Repeat cycling
  // ---------------------------------------------------------------

  private cycleRepeat(): void {
    const s = this.client.snap;
    if (!s) return;
    const next =
      s.repeat === "off" ? "all" : s.repeat === "all" ? "one" : "off";
    void this.client.setRepeat(next);
  }

  // ---------------------------------------------------------------
  // Playback setup (the audio twin of the video player's strip)
  // ---------------------------------------------------------------

  private async checkPlaybackSetup(): Promise<void> {
    if (this.setupChecked) return;
    this.setupChecked = true;
    try {
      const st = await api.playbackSetupStatus();
      if (!this.overlay || st.pluginsPresent) return;
      this.renderSetupStrip(
        "This system is missing the pieces that let Fable play music.",
        "Set up playback",
        false,
      );
    } catch {
      // A failed probe is not a playback error.
    }
  }

  private renderSetupStrip(text: string, btnLabel: string, withRepo: boolean): void {
    const strip = this.setupEl;
    if (!strip) return;
    strip.replaceChildren();
    const span = document.createElement("span");
    span.className = "vp-setup-text";
    span.textContent = text;
    const btn = document.createElement("button");
    btn.className = "vp-setup-btn";
    btn.textContent = btnLabel;
    btn.addEventListener("click", () => void this.runSetupInstall(withRepo, btn));
    strip.append(span, btn);
    strip.hidden = false;
  }

  private async runSetupInstall(withRepo: boolean, btn: HTMLButtonElement): Promise<void> {
    if (this.setupBusy) return;
    this.setupBusy = true;
    btn.disabled = true;
    btn.textContent = "Setting up…";
    try {
      const st = await api.playbackSetupInstall(withRepo);
      this.setupBusy = false;
      if (!this.overlay) return;
      if (st.pluginsPresent) {
        this.renderSetupStrip(
          "Playback is set up — if a track already gave up, play it again and it will sound.",
          "Done",
          false,
        );
        const strip = this.setupEl;
        window.setTimeout(() => {
          if (strip) strip.hidden = true;
        }, 6000);
      } else if (st.repoName && !st.repoEnabled) {
        this.renderSetupStrip(
          `The full playback set on this system lives in the ${st.repoName} source, which isn't enabled yet. Fable can add it — the system will ask for your password.`,
          `Add ${st.repoName} and finish setup`,
          true,
        );
      } else {
        this.renderSetupStrip(
          "Playback setup finished, but the pieces still aren't landing on this system.",
          "Try again",
          withRepo,
        );
      }
    } catch (e) {
      this.setupBusy = false;
      if (!this.overlay) return;
      this.renderSetupStrip(
        e instanceof Error ? e.message : String(e),
        "Try again",
        withRepo,
      );
    }
  }

  // ---------------------------------------------------------------
  // .m3u save / load
  // ---------------------------------------------------------------

  private async saveM3u(s: PlayerSnapshot): Promise<void> {
    if (s.queue.length === 0) return;
    const folder = dirname(s.queue[0].providerKey);
    const firstItem = this.itemsByKey.get(s.queue[0].providerKey);
    const albumName =
      (firstItem ? cleanTitle(firstItem.name) : null) || "playlist";
    const folderName = basename(folder) || albumName;
    const fileName = `${folderName.replace(/[\\/:*?"<>|]/g, "_")}.m3u`;
    const lines = ["#EXTM3U"];
    for (const e of s.queue) {
      const dur = e.durationSecs ? Math.round(e.durationSecs) : -1;
      lines.push(`#EXTINF:${dur},${cleanTitle(e.name)}`);
      lines.push(e.providerKey);
    }
    try {
      await api.writeTextFile(`${folder}/${fileName}`, lines.join("\n") + "\n");
      Toast.success(`Saved ${fileName} beside the album.`);
    } catch (e) {
      Toast.error(e instanceof Error ? e.message : String(e));
    }
  }

  /** Load: the .m3u files in the current folder. One → load it;
   * several → the first (the toast names it); none → say so. */
  private async loadM3u(): Promise<void> {
    const first = [...this.itemsByKey.values()][0];
    if (!first?.provider_key) return;
    const folder = dirname(first.provider_key);
    try {
      const { listFolderByPath } = await import("./audiometa");
      const entries = await listFolderByPath(first, folder);
      const playlists = (entries ?? []).filter((e) => isPlaylistName(e.name));
      if (playlists.length === 0) {
        Toast.error("No .m3u playlist in this folder.");
        return;
      }
      await this.openPlaylistFile(playlists[0]);
    } catch {
      Toast.error("That playlist could not be read.");
    }
  }

  /** Open an .m3u as the queue: relative paths resolve against
   * the playlist's folder; missing files are skipped and counted
   * in the toast (never silently dropped). */
  private async openPlaylistFile(item: Item): Promise<void> {
    if (!item.provider_key) return;
    const folder = dirname(item.provider_key);
    let text: { lines: string[]; truncated: boolean };
    try {
      text = await api.previewText(item.id, 10000);
    } catch {
      Toast.error("That playlist could not be read.");
      return;
    }
    const wanted: QueueEntry[] = [];
    for (const raw of text.lines) {
      const line = raw.trim();
      if (!line || line.startsWith("#")) continue;
      const path = line.startsWith("/") ? line : `${folder}/${line}`;
      wanted.push({
        providerKey: path,
        name: basename(path),
        durationSecs: null,
      });
    }
    // Existence is probed through the path-registration door
    // (it refuses files that aren't there): missing files are
    // skipped and COUNTED, never silently dropped.
    const entries: QueueEntry[] = [];
    let missing = 0;
    for (const e of wanted) {
      try {
        await api.castRegisterPath(e.providerKey);
        entries.push(e);
      } catch {
        missing++;
      }
    }
    if (entries.length === 0) {
      Toast.error(
        missing > 0
          ? `That playlist's ${missing} track${missing === 1 ? " is" : "s are"} gone.`
          : "That playlist names no tracks.",
      );
      return;
    }
    if (missing > 0) {
      Toast.error(
        `${missing} track${missing === 1 ? " was" : "s were"} missing and skipped.`,
      );
    }
    this.close();
    this.itemsByKey = new Map();
    this.buildPanel();
    if (this.titleEl) this.titleEl.textContent = cleanTitle(item.name);
    await this.client.openQueue({
      owner: "panel",
      entries,
      startIndex: 0,
      autoplay: true,
    });
    if (text.truncated) {
      Toast.error("That playlist is very long — only its first part was loaded.");
    }
  }

  // ---------------------------------------------------------------
  // Keyboard
  // ---------------------------------------------------------------

  private handleKey(e: KeyboardEvent): void {
    if (!this.overlay) return;
    const target = e.target as HTMLElement;
    if (
      target &&
      (target.tagName === "INPUT" || target.tagName === "TEXTAREA") &&
      !target.classList.contains("tbar-vol")
    ) {
      return;
    }
    const s = this.client.snap;
    if (!s) return;
    if (e.key === "Escape") {
      e.preventDefault();
      this.close();
    } else if (e.key === " ") {
      e.preventDefault();
      void this.engine.toggle();
    } else if (e.key === "ArrowRight") {
      void this.engine.seekTo(Math.min(s.durationSecs || 0, s.positionSecs + 5));
    } else if (e.key === "ArrowLeft") {
      void this.engine.seekTo(Math.max(0, s.positionSecs - 5));
    } else if (e.key === "ArrowUp" || e.key === "ArrowDown") {
      e.preventDefault();
      const v = Math.min(
        1,
        Math.max(0, s.volume + (e.key === "ArrowUp" ? 0.05 : -0.05)),
      );
      void this.client.setVolume(v);
    } else if (e.key.toLowerCase() === "m") {
      void this.client.setMuted(!s.muted);
    }
  }
}

