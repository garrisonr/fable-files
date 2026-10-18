import { errText, Toast } from "./toast";
/**
 * Quick Look: Space (or double-click on a file) opens a preview pane anchored
 * to the right of the selected file. The pane slides in from the right and
 * dismisses back to the right — enter and exit along the same path.
 *
 * Interruptibility: the pane's X offset is driven by a single spring keyed
 * "quicklook-x". Re-opening mid-close simply re-targets the live spring, so
 * the pane reverses from its on-screen position — it never finishes closing
 * first. Esc or Space again dismisses.
 *
 * MVP content: name, kind label (Native/Media/Document/Archive/Metadata/Hex),
 * size, modified, fetched via the `preview` command. A failed preview still
 * shows the metadata card (backend stub returns not_implemented today).
 */
import {
  api,
  formatBytes,
  formatDate,
  mediaSrc,
  PREVIEW_LABELS,
  type Item,
  type MetaTag,
  type PreviewKind,
} from "./api";
import { onPlayerClosed, onPlayerReturn, onPlayerState } from "./api";
import { attachStep, type AttachSnapshot } from "./qlattach";
import { VideoPlayer, isVideoName } from "./video";
import type { CastDeviceLike } from "./castsession";
import { audioBadge, audioProps, audioTags, folderArtUrl } from "./audiometa";
import {
  OwnerEngine,
  PlayerCoreClient,
  TransportBar,
} from "./playercore";
import { LyricsView } from "./lyrics";
import { fadeTo, reducedMotion, springTo } from "./motion";

const FALLBACK_PANEL_WIDTH = 340;

/** Steal 7: facts a file commonly carries that are safe to edit
 * inline — a subset of the Metadata dialog's writable set (the
 * same exiftool-backed edit_metadata door serves both). */
const FACTS_EDITABLE = new Set([
  "Title",
  "Author",
  "Creator",
  "Artist",
  "Comment",
  "Description",
  "ImageDescription",
]);

export class QuickLook {
  readonly el: HTMLElement;
  private openItem: Item | null = null;
  private requestToken = 0;
  /** Path whose video is currently playing in a pop-out window. */
  private detachedFor: string | null = null;
  /** Path whose audio is currently playing in a detach window. */
  private detachedAudioFor: string | null = null;
  /** Latest known playback state per detached path (UI round
   * item 5): seeded at detach time, refreshed by the detach
   * window's player-state stream — the inline player resumes from
   * here when the media comes home. */
  private detachedState = new Map<string, { t: number; playing: boolean }>();
  /** Attachment rules (Dickles 2026-10-15): the selection the
   * user moved to while this pane's media is detached. The pane
   * stays pinned to its placeholder; when the pop-out goes away,
   * the pane adopts this item (see qlattach.ts). */
  private followPending: Item | null = null;
  private currentKind: PreviewKind | null = null;
  /** The shared transport core, Quick Look arrangement (Music
   * Glow-Up Phase 2): this pane's client + engine + transport.
   * The engine sounds only while the core's owner is
   * "quicklook" — starting a preview anywhere else silences it,
   * and starting one here silences them. */
  private core = new PlayerCoreClient();
  private qlEngine: OwnerEngine | null = null;
  private qlBar: TransportBar | null = null;
  private qlLyrics = new LyricsView();
  private qlStatusEl: HTMLDivElement | null = null;
  private qlCastDlg: {
    openPicker: (n: string, v: boolean) => Promise<CastDeviceLike | null>;
    element: HTMLDivElement;
  } | null = null;
  private lastCoreOwner: string | null = null;
  /** Path the card's audio belongs to (lyrics + detach logic). */
  private audioPath: string | null = null;
  /** Art resolved for the card on screen, per media kind — the
   * detach placeholder and the mini window wear the same face
   * (fix batch items 3 + 5). */
  private cardArt: string | null = null;
  /** Item the current card belongs to (art resets when it changes). */
  private cardItemId: string | null = null;
  /** Tombstone watch (fix batch item 4): while any detach is
   * outstanding, poll the backend for the window's existence —
   * if the close event never lands, the placeholder brings
   * playback home itself instead of pointing at a dead window. */
  private detachWatch: number | null = null;

  private stopAudio(): void {
    // The core owns the sound now: leaving the preview stops it
    // only when the preview owns it (a detached Mini keeps
    // playing in its own window, exactly as before).
    this.audioPath = null;
    if (this.core.snap?.owner === "quicklook") {
      void api.playerStop().catch(() => undefined);
    }
  }

  /** Live video player (fix block item 3): tracked like the audio
   * element so closing the pane or re-rendering the card destroys it
   * — a detached <video> kept sounding after the pane closed and
   * only quitting the app stopped it (Dickles' Matlock repro). */
  private videoPlayer: VideoPlayer | null = null;

  private stopVideo(): void {
    if (!this.videoPlayer) return;
    this.videoPlayer.destroy();
    this.videoPlayer = null;
  }

  /** Set by the shell: cast the previewed media item. */
  onCast: ((item: Item) => void) | null = null;
  /** Grand Pass steal 1: told about every item Quick Look shows,
   * from every surface — the shell notes files into Home base's
   * Recent shelf through here. */
  onShowItem: ((item: Item) => void) | null = null;
  /** Set by the shell (Player round): the player's own Cast
   * button picks through the shell's shared CastDialog. */
  castPicker: ((name: string, video: boolean) => Promise<CastDeviceLike | null>) | null = null;
  private castBtn: HTMLButtonElement | null = null;
  /** Steal 7: the Facts section's mode + per-item tag cache. The
   * mode is a pane preference (persisted); tags are re-read only
   * after an edit through the section itself. */
  private factsOn = (() => {
    try {
      return localStorage.getItem("ff-ql-facts") === "1";
    } catch {
      return false;
    }
  })();
  private factsBtn: HTMLButtonElement | null = null;
  private factsCache = new Map<string, MetaTag[]>();

  constructor(private onDismiss: () => void) {
    this.el = document.createElement("aside");
    this.el.className = "quicklook";
    this.el.setAttribute("aria-label", "Preview");
    this.el.hidden = true;
    this.el.innerHTML = `
      <div class="ql-head">
        <span class="ql-title">Preview</span>
        <button class="ql-facts-btn pill-btn" aria-label="Toggle the Facts section" title="Facts — the details stored inside the file">Facts</button>
        <button class="ql-cast icon-btn" aria-label="Cast to device" title="Cast to…" hidden>📡</button>
        <button class="ql-close icon-btn" aria-label="Close preview">✕</button>
      </div>
      <div class="ql-body"></div>`;
    this.factsBtn = this.el.querySelector(".ql-facts-btn");
    this.factsBtn?.setAttribute("aria-pressed", String(this.factsOn));
    this.factsBtn?.classList.toggle("on", this.factsOn);
    this.factsBtn?.addEventListener("click", () => {
      this.factsOn = !this.factsOn;
      try {
        localStorage.setItem("ff-ql-facts", this.factsOn ? "1" : "0");
      } catch {
        // session-only mode is fine
      }
      this.factsBtn?.setAttribute("aria-pressed", String(this.factsOn));
      this.factsBtn?.classList.toggle("on", this.factsOn);
      if (this.openItem) {
        void this.renderCard(this.openItem, this.currentKind);
      }
    });
    this.castBtn = this.el.querySelector(".ql-cast");
    this.castBtn?.addEventListener("click", () => {
      if (this.openItem && this.onCast) this.onCast(this.openItem);
    });
    this.el
      .querySelector(".ql-close")!
      .addEventListener("click", () => this.close());
    // When a pop-out player window closes, the media comes home.
    void onPlayerClosed((path) => this.handleDetachGone(path)).catch(
      () => undefined,
    );
    // Detach-window state stream (UI round item 5): keep the
    // resume cache at the position playback ACTUALLY reached, so
    // the trip home lands mid-sentence, not at the detach point.
    const cacheState = (s: { path: string; t: number; playing: boolean }) => {
      if (s.path === this.detachedFor || s.path === this.detachedAudioFor) {
        this.detachedState.set(s.path, { t: s.t, playing: s.playing });
      }
    };
    void onPlayerState(cacheState).catch(() => undefined);
    void onPlayerReturn(cacheState).catch(() => undefined);
    // The shared transport core (Music Glow-Up Phase 2): this
    // pane's engine sounds only while the core's owner is
    // "quicklook". Snapshots drive its transport, its lyrics,
    // and the mini-detach placeholder flip.
    void this.core.start();
    this.qlEngine = new OwnerEngine("quicklook", this.core, {
      castPicker: (n, v) => this.castForPreview(n, v),
      onSnapshot: (s) => {
        this.qlBar?.update(s);
        const cur = s.index !== null ? s.queue[s.index] : null;
        if (
          this.audioPath &&
          cur?.providerKey === this.audioPath &&
          this.qlLyrics.isSynced
        ) {
          this.qlLyrics.setPosition(s.positionSecs * 1000);
        }
        if (this.qlStatusEl) {
          this.qlStatusEl.textContent =
            s.owner === "panel"
              ? "Playing in the Music Player — this transport drives it."
              : s.owner === "stage"
                ? "On the Stage — this transport drives it."
                : "";
        }
        const owner = s.owner;
        if (owner !== this.lastCoreOwner) {
          const was = this.lastCoreOwner;
          this.lastCoreOwner = owner;
          if (
            (owner === "mini" || was === "mini") &&
            this.openItem &&
            this.audioPath === this.openItem.provider_key &&
            cur?.providerKey === this.openItem.provider_key
          ) {
            void this.renderCard(this.openItem, this.currentKind);
          }
        }
      },
    });
  }

  /** Cast picker for the preview's own engine: the shell's
   * shared dialog when wired, else a lazily-built one. */
  private async castForPreview(
    name: string,
    video: boolean,
  ): Promise<CastDeviceLike | null> {
    if (this.castPicker) return this.castPicker(name, video);
    if (!this.qlCastDlg) {
      try {
        const { CastDialog } = await import("./cast");
        this.qlCastDlg = new CastDialog();
        document.body.appendChild(this.qlCastDlg.element);
      } catch {
        return null;
      }
    }
    return this.qlCastDlg.openPicker(name, video);
  }

  /** The attachment machine's view of this pane, derived fresh
   * from the authoritative fields (qlattach.ts): what's open,
   * which path a pop-out pins (the explicit detached flags),
   * and the selection remembered while pinned. */
  private attachSnapshot(): AttachSnapshot {
    return {
      openId: this.openItem?.id ?? null,
      pinnedPath: this.detachedFor ?? this.detachedAudioFor ?? null,
      pendingId: this.followPending?.id ?? null,
    };
  }

  /** Selection changed on some surface (the shell routes them
   * all here — browse views, search / Smart Folder results,
   * mouse and keyboard alike). Attachment rules: an OPEN
   * attached pane follows the selection; a pane whose media is
   * detached stays pinned and remembers the selection for the
   * return; a closed pane ignores all of it. */
  followSelection(item: Item | null): void {
    if (!this.openItem) return;
    const { pendingId, effect } = attachStep(this.attachSnapshot(), {
      type: "selection",
      id: item?.id ?? null,
    });
    // attachStep reports a pending id only for the event's own
    // item (the pinned branch), so the object in hand is it.
    this.followPending =
      pendingId !== null && item && item.id === pendingId ? item : null;
    if (effect.type === "follow" && item) void this.open(item);
  }

  /** A detach window for `path` is gone — by its close event or
   * by the tombstone watch finding it missing (fix batch item 4).
   * Clears the detached flag, then the attachment rules decide
   * the card: if the user moved the selection while the media
   * was away, the pane ADOPTS that selection (rule 3); if the
   * card on screen IS that media, it re-renders so playback
   * resumes inline from the streamed state. If a different card
   * is on screen, the resume state simply stays cached for when
   * its own card returns. */
  private handleDetachGone(path: string): void {
    const mine =
      this.detachedFor === path || this.detachedAudioFor === path;
    if (!mine) return;
    // Snapshot BEFORE the flags clear: the machine needs the pin
    // this pop-out held to judge the return.
    const snap = this.attachSnapshot();
    if (this.detachedFor === path) this.detachedFor = null;
    if (this.detachedAudioFor === path) this.detachedAudioFor = null;
    this.stopDetachWatchIfIdle();
    const { effect } = attachStep(snap, {
      type: "detach-end",
      path,
      openPath: this.openItem?.provider_key ?? null,
    });
    const adopt = effect.type === "adopt" ? this.followPending : null;
    this.followPending = null;
    if (effect.type === "adopt" && adopt) {
      void this.open(adopt);
    } else if (effect.type === "render-home" && this.openItem) {
      void this.renderCard(this.openItem, this.currentKind);
    }
  }

  /** The shell popped this item out directly (double-click law):
   *  when the pane is showing the same item, the pane becomes
   *  its detached placeholder — the window owns playback, the
   *  inline player stops (renderCard tears it down), and the
   *  trip home runs through the usual detach-gone path. The
   *  inline position seeds the resume cache; the window's own
   *  state stream takes over from there. Without this, selection
   *  follow (rule 1) starting a file inline and the double-click
   *  pop-out would play it twice. */
  notePoppedOut(item: Item): void {
    if (!this.openItem || this.openItem.id !== item.id) return;
    const path = item.provider_key;
    if (!path) return;
    if (this.videoPlayer && this.videoPlayer.sourcePath === path) {
      this.detachedState.set(path, {
        t: this.videoPlayer.position(),
        playing: this.videoPlayer.isPlaying(),
      });
    } else if (this.audioPath === path) {
      const s = this.core.snap;
      this.detachedState.set(path, {
        t: s?.positionSecs ?? 0,
        playing: s?.state === "playing",
      });
    }
    if (isVideoName(item.name)) {
      if (this.detachedFor === path) return;
      this.detachedFor = path;
    } else {
      if (this.detachedAudioFor === path) return;
      this.detachedAudioFor = path;
    }
    this.startDetachWatch();
    void this.renderCard(item, this.currentKind);
  }

  /** Tombstone watch (fix batch item 4): while any detach is
   * outstanding, verify the window still exists every second or
   * so. The close event is the fast path; this is the guarantee
   * — a placeholder may never point at a window that is gone. */
  private startDetachWatch(): void {
    if (this.detachWatch !== null) return;
    this.detachWatch = window.setInterval(() => {
      for (const p of [this.detachedFor, this.detachedAudioFor]) {
        if (!p) continue;
        void api
          .playerWindowOpen(p)
          .then((open) => {
            if (!open) this.handleDetachGone(p);
          })
          .catch(() => undefined);
      }
    }, 1200);
  }

  private stopDetachWatchIfIdle(): void {
    if (this.detachedFor || this.detachedAudioFor) return;
    if (this.detachWatch !== null) {
      window.clearInterval(this.detachWatch);
      this.detachWatch = null;
    }
  }

  /** Space while media Quick Look is open (fix batch item 1):
   * toggles the INLINE player. Returns false when there is no
   * inline media to toggle (metadata card, or playback lives in
   * the detach window) so the caller keeps its default Space
   * behavior. */
  toggleMediaPlayback(): boolean {
    if (this.videoPlayer) {
      this.videoPlayer.togglePlay();
      return true;
    }
    if (this.audioPath) {
      // The preview's track is the core's track: Space drives
      // the core (whoever owns the element right now).
      const s = this.core.snap;
      const cur =
        s && s.index !== null ? s.queue[s.index] : null;
      if (cur && cur.providerKey === this.audioPath) {
        void this.core.toggle().catch(() => undefined);
        return true;
      }
    }
    return false;
  }

  /** The card's art chain, in one place: folder art first, then
   * the thumbnailer's embedded art — the order the audio card
   * has always used (fix batch items 3 + 5 share it). */
  private async resolveArt(item: Item, path: string): Promise<string | null> {
    try {
      const folder = path.slice(0, path.lastIndexOf("/"));
      const furl = folder ? await folderArtUrl(item, folder) : null;
      if (furl) return furl;
      return await api.getThumbnail(item.id, item.provider_key, true);
    } catch {
      return null;
    }
  }

  /** The detached-playing placeholder (fix batch item 5): flat
   * family material — artwork, title, one quiet line, one
   * bordered pill button. No glow, no early-2000s chrome. */
  private buildDetachedPlaceholder(item: Item, path: string): HTMLElement {
    const holder = document.createElement("div");
    holder.className = "ql-detached";
    const art = document.createElement("img");
    art.className = "ql-detached-art";
    art.alt = "";
    art.hidden = true;
    const glyph = document.createElement("div");
    glyph.className = "ql-detached-glyph";
    glyph.textContent = isVideoName(item.name) ? "🎬" : "🎵";
    art.addEventListener("error", () => {
      art.hidden = true;
      glyph.hidden = false;
    });
    const meta = document.createElement("div");
    meta.className = "ql-detached-meta";
    const title = document.createElement("div");
    title.className = "ql-detached-title";
    title.textContent = item.name;
    const sub = document.createElement("div");
    sub.className = "ql-detached-sub";
    sub.textContent = "Playing in the pop-out window";
    meta.append(title, sub);
    const btn = document.createElement("button");
    btn.className = "ql-detach-btn";
    btn.textContent = "⤺ Bring it back here";
    btn.addEventListener("click", () => {
      // Bring it home (fix batch item 4): close the window; its
      // close event — or the tombstone watch, if the event never
      // lands — re-renders this card with playback resumed inline.
      void api.closePlayerWindow(path).catch(() => undefined);
    });
    holder.append(art, glyph, meta, btn);
    const show = (url: string | null): void => {
      if (!url) return;
      this.cardArt = url;
      art.src = url;
      art.hidden = false;
      glyph.hidden = true;
    };
    if (this.cardArt) show(this.cardArt);
    else void this.resolveArt(item, path).then(show);
    return holder;
  }

  /** Pop the card's video out (fix batch item 6): the detach
   * window continues from THIS position and play state; the
   * resume cache is seeded so even an instantly-killed window
   * comes home sanely. Shared by the transport ⧉ and the card's
   * visible Detach button — one door, both handles. */
  private detachVideo(item: Item, player: VideoPlayer): void {
    const vpath = item.provider_key;
    if (!vpath || this.detachedFor === vpath) return;
    const t = player.position();
    const playing = player.isPlaying();
    this.detachedState.set(vpath, { t, playing });
    void api
      .openPlayerWindow(vpath, item.name, {
        kind: "video",
        start: t,
        autoplay: playing,
      })
      .then(() => {
        this.detachedFor = vpath;
        this.startDetachWatch();
        if (this.openItem === item) {
          void this.renderCard(item, this.currentKind);
        }
      })
      .catch(() => undefined);
  }

  get isOpen(): boolean {
    return this.openItem !== null;
  }

  get item(): Item | null {
    return this.openItem;
  }

  async open(item: Item): Promise<void> {
    const token = ++this.requestToken;
    this.openItem = item;
    this.onShowItem?.(item);
    if (this.castBtn) {
      const ext = item.name.split(".").pop()?.toLowerCase() ?? "";
      const AUDIO = ["mp3", "flac", "ogg", "oga", "wav", "m4a", "opus", "aac", "aif", "aiff"];
      this.castBtn.hidden = !(
        this.onCast &&
        (AUDIO.includes(ext) ||
          ["mp4", "mkv", "webm", "mov", "avi", "m4v", "mpg", "mpeg"].includes(ext))
      );
    }
    this.el.hidden = false;
    this.renderCard(item, null);
    // Slide in from the right — from the LIVE offset (interruptible).
    this.slideTo(0);
    try {
      const kind = await api.preview(item.id);
      if (token === this.requestToken && this.openItem === item) {
        this.currentKind = kind;
        this.renderCard(item, kind);
      }
    } catch {
      if (token === this.requestToken && this.openItem === item) {
        this.renderCard(item, null); // metadata card survives backend gaps
      }
    }
  }

  close(): void {
    if (!this.openItem) return;
    this.stopAudio();
    this.stopVideo();
    this.requestToken++;
    this.openItem = null;
    this.followPending = null;
    this.slideTo(this.closedX(), () => {
      // A re-open mid-close re-targets the same spring; its stale onDone
      // must not hide a pane that is open again.
      if (this.openItem) return;
      this.el.hidden = true;
      this.el.style.transform = "";
    });
    this.onDismiss();
  }

  toggle(item: Item | null): void {
    if (this.isOpen && (!item || item.id === this.openItem?.id)) {
      this.close();
    } else if (item) {
      void this.open(item);
    }
  }

  private slideTo(x: number, onDone?: () => void): void {
    if (reducedMotion()) {
      // Reduced motion: 120ms opacity fade, no travel.
      if (x === 0) {
        this.el.style.opacity = "0";
        fadeTo(this.el, 1, onDone);
      } else {
        fadeTo(this.el, 0, onDone);
      }
      return;
    }
    const from = this.liveX();
    springTo(
      "quicklook-x",
      x,
      (v) => {
        this.el.style.transform = `translateX(${v}px)`;
      },
      { from, onDone },
    );
  }

  /** Off-screen X: the pane's live width, so CSS and motion never drift. */
  private closedX(): number {
    return this.el.offsetWidth > 0 ? this.el.offsetWidth : FALLBACK_PANEL_WIDTH;
  }

  private liveX(): number {
    const t = this.el.style.transform;
    const m = t.match(/translateX\((-?[\d.]+)px\)/);
    return m ? parseFloat(m[1]) : this.closedX();
  }

  /** Phase 4 rich document preview: a PDF's rendered first page or
   * an EPUB's cover as the card hero, the book's own title/author as
   * headline/subline (the audio-tags pattern), page/chapter count in
   * the metadata rows, the opening text below, and any plain-language
   * note (DRM, oversize, unreadable) last. The metadata card paints
   * instantly; a quiet "Rendering…" line holds the hero's place
   * until the decode lands. Returns false only when the decode
   * failed outright, so the caller falls back to text extraction. */
  private async renderDocumentPreview(
    item: Item,
    card: HTMLElement,
    dl: HTMLElement,
    name: HTMLElement,
  ): Promise<boolean> {
    const loading = document.createElement("p");
    loading.className = "ql-doc-loading";
    loading.textContent =
      item.name.toLowerCase().endsWith(".pdf")
        ? "Rendering first page…"
        : "Opening book…";
    card.appendChild(loading);
    let doc: Awaited<ReturnType<typeof api.previewDocument>>;
    try {
      doc = await api.previewDocument(item.id);
    } catch {
      loading.remove();
      return false;
    }
    loading.remove();
    if (doc.image) {
      const img = document.createElement("img");
      img.className = "ql-doc-hero";
      img.src = doc.image;
      img.alt = doc.title ?? item.name;
      card.insertBefore(img, card.firstChild);
      card.classList.add("has-photo");
    }
    if (doc.title && doc.title !== item.name) {
      name.textContent = doc.title;
      name.title = doc.title;
    }
    if (doc.author) {
      const subEl = document.createElement("div");
      subEl.className = "ql-sub";
      subEl.textContent = doc.author;
      name.after(subEl);
    }
    if (doc.detail) {
      const dt = document.createElement("dt");
      dt.textContent = "Length";
      const dd = document.createElement("dd");
      dd.textContent = doc.detail;
      dl.appendChild(dt);
      dl.appendChild(dd);
    }
    if (doc.lines.length > 0) {
      const pre = document.createElement("pre");
      pre.className = "ql-text-preview";
      pre.textContent =
        doc.lines.join("\n") + (doc.truncated ? "\n… (truncated)" : "");
      card.appendChild(pre);
    }
    if (doc.note) {
      const note = document.createElement("p");
      note.className = "ql-doc-note";
      note.textContent = doc.note;
      card.appendChild(note);
    }
    // Phase 6: an EPUB opens in the built-in reader from here —
    // an explicit door; double-click still opens the OS default app.
    // PDF Reader build: PDFs take the same door into the paginated
    // reader.
    const lowerName = item.name.toLowerCase();
    if (lowerName.endsWith(".epub") || lowerName.endsWith(".pdf")) {
      const isPdf = lowerName.endsWith(".pdf");
      const readBtn = document.createElement("button");
      readBtn.className = "ql-detach-btn ql-read-btn";
      readBtn.textContent = "Read";
      readBtn.title = "Open in the built-in reader";
      readBtn.addEventListener("click", () => {
        void (async () => {
          const path =
            item.provider_key ??
            (await api.getItemPath(item.id).catch(() => null));
          if (!path) return;
          this.close();
          if (isPdf) {
            const m = await import("./pdfreader");
            m.openPdfReader(path);
          } else {
            const m = await import("./reader");
            m.openReader(path);
          }
        })();
      });
      card.appendChild(readBtn);
    }
    return true;
  }

  private async renderCard(item: Item, kind: PreviewKind | null): Promise<void> {
    // Carry snapshot (fix batch item 1): a re-render for the SAME
    // media — the preview-kind round-trip re-renders every open —
    // must not restart playback from zero. Capture the outgoing
    // player's position + play state before teardown; the media
    // branches below hand them to the replacement player.
    let carry: { path: string; t: number; playing: boolean } | null = null;
    if (this.videoPlayer) {
      carry = {
        path: this.videoPlayer.sourcePath,
        t: this.videoPlayer.position(),
        playing: this.videoPlayer.isPlaying(),
      };
    } else if (this.audioPath) {
      const s = this.core.snap;
      if (s) {
        carry = {
          path: this.audioPath,
          t: s.positionSecs,
          playing: s.state === "playing",
        };
      }
    }
    if (carry && carry.path !== item.provider_key) carry = null;
    if (this.cardItemId !== item.id) this.cardArt = null;
    this.cardItemId = item.id;
    // A re-render replaces the card: stop any audio AND video the old
    // card was playing before its elements are discarded — except
    // when the new card IS the audio track the core is holding for
    // this preview (the core carries it; stopping would kill a
    // detach trip home).
    if (this.audioPath !== item.provider_key) this.stopAudio();
    this.stopVideo();
    const body = this.el.querySelector(".ql-body")!;
    body.innerHTML = "";
    const card = document.createElement("div");
    card.className = "ql-card";

    const badge = document.createElement("span");
    badge.className = "ql-kind";
    badge.textContent = kind ? PREVIEW_LABELS[kind] : "Metadata";
    card.appendChild(badge);

    const name = document.createElement("h2");
    name.className = "ql-name";
    name.textContent = item.name;
    name.title = item.name;
    card.appendChild(name);

    const rows: Array<[string, string]> = [
      ["Kind", item.kind === "folder" ? "Folder" : "File"],
      ["Size", item.kind === "folder" ? "—" : formatBytes(item.size_bytes)],
      ["Modified", formatDate(item.modified)],
    ];
    if (item.provider_key) rows.push(["Path", item.provider_key]);
    if (item.starred) rows.push(["Starred", "Yes"]);
    if (item.labels.length > 0) rows.push(["Labels", item.labels.join(", ")]);

    const dl = document.createElement("dl");
    dl.className = "ql-meta";
    for (const [k, v] of rows) {
      const dt = document.createElement("dt");
      dt.textContent = k;
      const dd = document.createElement("dd");
      if (k === "Path") {
        dd.className = "ql-path";
        dd.title = v;
        v.split("/").forEach((seg, i) => {
          if (i > 0) {
            dd.append("/");
            dd.appendChild(document.createElement("wbr"));
          }
          if (seg) dd.append(seg);
        });
      } else {
        dd.textContent = v;
      }
      dl.appendChild(dt);
      dl.appendChild(dd);
    }
    card.appendChild(dl);

    body.appendChild(card);

    // Rich content preview. Driven by extension (known synchronously from
    // the item name) rather than the async preview-kind round-trip, with
    // the item's provider path as a fallback for id resolution.
    if (item.kind !== "folder") {
      const ext = item.name.split(".").pop()?.toLowerCase() ?? "";
      const textExts = ["txt", "md", "csv", "tsv", "log", "srt", "vtt", "sub", "ssa", "ass", "nfo", "json", "xml", "yaml", "yml", "toml", "ini", "cfg", "sh", "py", "rs", "ts", "js"];
      // Round 7.1: documents get a text preview too — the backend
      // extracts readable text from docx/odt/epub/pdf in preview_text.
      // Round 7.2: ods/odp join (same ODF content.xml mechanism).
      const docExts = ["docx", "odt", "ods", "odp", "epub", "pdf"];
      // Phase 4: PDFs and EPUBs get the RICH document preview first —
      // rendered first page / book cover hero, book metadata, opening
      // text (see renderDocumentPreview). The plain text extraction
      // below remains the fallback if the decode fails outright.
      let richDocument = false;
      if (kind === "document" && (ext === "pdf" || ext === "epub")) {
        richDocument = await this.renderDocumentPreview(item, card, dl, name);
      }
      if (
        !richDocument &&
        ((kind === "native" && textExts.includes(ext)) ||
          (kind === "document" && docExts.includes(ext)))
      ) {
        try {
          const result = await api.previewText(item.id, 50);
          const pre = document.createElement("pre");
          pre.className = "ql-text-preview";
          pre.textContent = result.lines.join("\n") + (result.truncated ? "\n… (truncated)" : "");
          card.appendChild(pre);
        } catch {
          // Text preview failed — metadata card stands alone
        }
      }
      // Image preview — the photo IS the preview: hero at the top of
      // the card, as large as the pane allows (full image, not a thumb).
      // Video: the full player lives at the top of the card —
      // transport controls on hover, pop-out, fullscreen.
      if (isVideoName(item.name) && item.provider_key) {
        const vpath = item.provider_key;
        if (this.detachedFor === vpath) {
          card.insertBefore(this.buildDetachedPlaceholder(item, vpath), dl);
        } else {
          // Start state, in priority order (fix batch item 1):
          // a detach trip home resumes where the window left off;
          // a same-card re-render carries the live position; a
          // FRESH open plays — Quick Look on playable media means
          // playing, never a paused preview waiting for a click.
          const fromDetach = this.detachedState.get(vpath);
          if (fromDetach) this.detachedState.delete(vpath);
          const player = new VideoPlayer({
            path: vpath,
            startAt: fromDetach?.t ?? carry?.t ?? 0,
            autoPlay: fromDetach
              ? fromDetach.playing
              : carry
                ? carry.playing
                : true,
            onDetach: () => this.detachVideo(item, player),
          });
          if (this.castPicker) player.setCastPicker(this.castPicker);
          this.videoPlayer = player;
          card.insertBefore(player.el, dl);
          // Detach, discoverable (fix batch item 6): the transport
          // bar's ⧉ hides with the bar and Dickles never found it
          // on video — the audio card's always-visible Detach
          // button is the pattern, mirrored here exactly.
          const detachBtn = document.createElement("button");
          detachBtn.className = "ql-detach-btn ql-media-detach";
          detachBtn.textContent = "⧉ Detach";
          detachBtn.title = "Play in a detachable mini window";
          detachBtn.addEventListener("click", () =>
            this.detachVideo(item, player),
          );
          card.insertBefore(detachBtn, dl);
        }
      }
      // Audio (Music Glow-Up Phase 2): the preview is an
      // arrangement of the shared transport core. The card claims
      // the core for this track — starting this preview stops
      // whatever played before, on every surface — and renders
      // the shared compact transport plus the synced lyrics.
      // Art comes from the shared folder/embedded chain.
      const audioExts = [
        "mp3", "flac", "ogg", "oga", "wav", "m4a", "m4b", "opus", "aac",
        "aif", "aiff", "wma", "dsf", "dff",
      ];
      if (audioExts.includes(ext) && item.provider_key) {
        const apath = item.provider_key;
        this.audioPath = apath;
        const snap0 = this.core.snap;
        const cur0 =
          snap0 && snap0.index !== null ? snap0.queue[snap0.index] : null;
        const miniOwns =
          snap0?.owner === "mini" && cur0?.providerKey === apath;
        if (this.detachedAudioFor === apath || miniOwns) {
          card.insertBefore(this.buildDetachedPlaceholder(item, apath), dl);
        } else {
          const holder = document.createElement("div");
          holder.className = "ql-audio";
          const art = document.createElement("img");
          art.className = "ql-audio-art";
          art.alt = "";
          art.hidden = true;
          const glyph = document.createElement("div");
          glyph.className = "ql-audio-glyph";
          glyph.textContent = "🎵";
          art.addEventListener("error", () => {
            art.hidden = true;
            glyph.hidden = false;
          });
          holder.append(art, glyph);
          // Art chain: folder art, then the thumbnailer's
          // embedded art (the shared resolution).
          void (async () => {
            try {
              const folder = apath.slice(0, apath.lastIndexOf("/"));
              const url =
                (await folderArtUrl(item, folder)) ??
                (await api.getThumbnail(item.id, apath, true));
              if (url && this.openItem === item) {
                this.cardArt = url;
                art.src = url;
                art.hidden = false;
                glyph.hidden = true;
              }
            } catch {
              // no art anywhere — the glyph stays
            }
          })();
          const detachBtn = document.createElement("button");
          detachBtn.className = "ql-open-with pill-btn";
          detachBtn.textContent = "⧉ Detach";
          detachBtn.title = "Pop the player out into its own window";
          detachBtn.addEventListener("click", () => {
            this.detachedState.set(apath, {
              t: this.core.snap?.positionSecs ?? 0,
              playing: this.core.snap?.state === "playing",
            });
            this.detachedAudioFor = apath;
            this.startDetachWatch();
            void api.openPlayerWindow(apath, item.name, {
              kind: "audio",
              start: this.core.snap?.positionSecs ?? 0,
              autoplay: this.core.snap?.state === "playing",
              art: this.cardArt,
            });
            void this.renderCard(item, this.currentKind);
          });
          const stageBtn = document.createElement("button");
          stageBtn.className = "ql-open-with pill-btn";
          stageBtn.textContent = "⤢ Stage";
          stageBtn.title = "Open the Stage window";
          stageBtn.addEventListener("click", () => {
            void api.openPlayerWindow(apath, item.name, {
              kind: "audio-stage",
              start: this.core.snap?.positionSecs ?? 0,
              autoplay: this.core.snap?.state === "playing",
              art: null,
            });
          });
          holder.appendChild(detachBtn);
          holder.appendChild(stageBtn);
          // Claim the core for this preview (or open it fresh).
          if (cur0?.providerKey === apath) {
            if (snap0 && snap0.owner !== "quicklook") {
              void this.core.claim("quicklook");
            }
          } else {
            void this.core.openQueue({
              owner: "quicklook",
              entries: [
                { providerKey: apath, name: item.name, durationSecs: null },
              ],
              startIndex: 0,
              autoplay: true,
            });
          }
          // The shared transport, compact arrangement.
          if (!this.qlBar) {
            this.qlBar = new TransportBar(
              {
                onToggle: () => void this.qlEngine?.toggle(),
                onStep: (d) => void this.core.step(d),
                onSeek: (s) => void this.qlEngine?.seekTo(s),
                onRepeat: () => {
                  const s = this.core.snap;
                  if (s) {
                    void this.core.setRepeat(
                      s.repeat === "off"
                        ? "all"
                        : s.repeat === "all"
                          ? "one"
                          : "off",
                    );
                  }
                },
                onShuffle: () => {
                  const s = this.core.snap;
                  if (s) void this.core.setShuffle(!s.shuffle);
                },
                onVolume: (v) => void this.core.setVolume(v),
                onMute: () => {
                  const s = this.core.snap;
                  if (s) void this.core.setMuted(!s.muted);
                },
                onCast: () => void this.qlEngine?.startCastFlow(),
              },
              { compact: true },
            );
          }
          const barHost = document.createElement("div");
          barHost.className = "ql-tbar-host";
          barHost.appendChild(this.qlBar.el);
          holder.appendChild(barHost);
          const s1 = this.core.snap;
          if (s1) this.qlBar.update(s1);
          const status = document.createElement("div");
          status.className = "ql-audio-status";
          this.qlStatusEl = status;
          holder.appendChild(status);
          // Credential line: honest badge + the tag headline.
          const meta = document.createElement("div");
          meta.className = "ql-audio-meta";
          const badge = document.createElement("span");
          badge.className = "music-badge";
          badge.textContent = audioBadge(item.name);
          const tagsLine = document.createElement("span");
          meta.append(badge, tagsLine);
          holder.appendChild(meta);
          void audioTags(item).then((tags) => {
            const who = [tags.artist, tags.album]
              .filter((x): x is string => !!x)
              .join(" · ");
            if (who && this.openItem === item) tagsLine.textContent = ` ${who}`;
          });
          void audioProps(item).then((p) => {
            if (this.openItem === item) {
              badge.textContent = audioBadge(item.name, p);
            }
          });
          card.insertBefore(holder, dl);
          // Synced lyrics — the shared pane, driven by core
          // snapshots (the subscription lives in the constructor).
          card.insertBefore(this.qlLyrics.el, dl);
          void this.qlLyrics.load(apath);
        }
      }
      const imgExts = ["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "tif", "tiff", "ico"];
      if (imgExts.includes(ext)) {
        try {
          const dataUrl = await api.getImagePreview(item.id, item.provider_key);
          const img = document.createElement("img");
          img.className = "ql-image-preview";
          img.src = dataUrl;
          img.alt = item.name;
          card.insertBefore(img, card.firstChild);
          card.classList.add("has-photo");
        } catch (err) {
          const note = document.createElement("p");
          note.className = "ql-preview-error";
          note.textContent = `Image preview unavailable: ${errText(err)}`;
          card.appendChild(note);
        }
      }
    }
    // Facts (steal 7): the details stored INSIDE the file, under
    // everything else in the same pane — only in Facts mode, and
    // only for files (a folder stores no embedded facts to read).
    if (this.factsOn && item.kind !== "folder") {
      void this.appendFacts(item, card);
    }
  }

  /** Steal 7: fetch (once per item per session) and append the
   * Facts section. Guarded like every async card append: a card
   * the user has already moved past never grows a late section. */
  private async appendFacts(item: Item, card: HTMLElement): Promise<void> {
    let tags = this.factsCache.get(item.id);
    if (!tags) {
      tags = await api.readMetadata(item.id).catch(() => [] as MetaTag[]);
      this.factsCache.set(item.id, tags);
    }
    if (this.cardItemId !== item.id || !card.isConnected) return;
    card.appendChild(this.buildFactsSection(item, tags));
  }

  /** The Facts section for one item: kind-appropriate fields out
   * of the file's own embedded metadata (the read_metadata
   * plumbing the Metadata dialog uses — no second engine).
   * Fields the file doesn't carry are omitted, never faked; an
   * all-empty file gets the honest one-liner. Common fields that
   * ARE present are inline-editable through edit_metadata. */
  private buildFactsSection(item: Item, tags: MetaTag[]): HTMLElement {
    const byTag = new Map<string, MetaTag>();
    for (const t of tags) {
      if (!byTag.has(t.tag)) byTag.set(t.tag, t);
    }
    const get = (name: string): string => byTag.get(name)?.value ?? "";
    const rows: { label: string; value: string; tag?: string }[] = [];
    const push = (label: string, value: string, tag?: string) => {
      if (value) rows.push({ label, value, tag });
    };
    const firstOf = (names: string[]): { value: string; tag?: string } => {
      for (const n of names) {
        const v = get(n);
        if (v) return { value: v, tag: n };
      }
      return { value: "" };
    };
    const dimensions = (): string => {
      const size = get("ImageSize");
      if (size) return size;
      const w = get("ImageWidth") || get("SourceImageWidth");
      const h = get("ImageHeight") || get("SourceImageHeight");
      return w && h ? `${w} × ${h}` : "";
    };
    const camera = (): string => {
      const make = get("Make");
      const model = get("Model");
      if (make && model && !model.startsWith(make)) return `${make} ${model}`;
      return model || make;
    };

    const ext = item.name.split(".").pop()?.toLowerCase() ?? "";
    const imageExts = ["png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "heic", "heif", "ico"];
    const audioExts = ["mp3", "flac", "ogg", "oga", "wav", "m4a", "m4b", "opus", "aac", "aif", "aiff", "wma", "dsf", "dff"];
    const docExts = ["pdf", "epub", "docx", "odt", "ods", "odp", "doc"];
    if (imageExts.includes(ext)) {
      push("Dimensions", dimensions());
      const taken = firstOf(["DateTimeOriginal", "CreateDate"]);
      push("Date taken", taken.value);
      push("Camera", camera());
      push("Lens", get("LensModel") || get("Lens"));
    } else if (isVideoName(item.name)) {
      push("Dimensions", dimensions());
      push("Duration", get("Duration"));
      const rec = firstOf(["DateTimeOriginal", "CreateDate"]);
      push("Recorded", rec.value);
    } else if (audioExts.includes(ext)) {
      const title = firstOf(["Title"]);
      push("Title", title.value, title.tag);
      const artist = firstOf(["Artist"]);
      push("Artist", artist.value, artist.tag);
      push("Album", get("Album"));
      push("Track", get("Track"));
      push("Duration", get("Duration"));
    } else if (docExts.includes(ext)) {
      const title = firstOf(["Title"]);
      push("Title", title.value, title.tag);
      const author = firstOf(["Author", "Creator"]);
      push("Author", author.value, author.tag);
      push("Pages", get("PageCount") || get("Pages"));
      push("Words", get("WordCount"));
      const created = firstOf(["CreateDate", "CreationDate"]);
      push("Created", created.value);
    }
    const comment = firstOf(["Comment", "ImageDescription", "Description"]);
    push("Comment", comment.value, comment.tag);

    const section = document.createElement("section");
    section.className = "ql-facts";
    const head = document.createElement("div");
    head.className = "ql-facts-head";
    head.textContent = "Facts";
    section.appendChild(head);
    if (rows.length === 0) {
      const empty = document.createElement("p");
      empty.className = "ql-facts-empty";
      empty.textContent = "No extra facts are stored inside this file.";
      section.appendChild(empty);
      return section;
    }
    const dl = document.createElement("dl");
    dl.className = "ql-meta ql-facts-list";
    for (const row of rows) {
      const dt = document.createElement("dt");
      dt.textContent = row.label;
      const dd = document.createElement("dd");
      dd.textContent = row.value;
      if (row.tag && FACTS_EDITABLE.has(row.tag)) {
        dd.classList.add("ql-fact-editable");
        dd.title = `Click to edit ${row.tag}`;
        dd.addEventListener("click", () =>
          this.editFact(item, section, row.tag!, dd),
        );
      }
      dl.appendChild(dt);
      dl.appendChild(dd);
    }
    section.appendChild(dl);
    return section;
  }

  /** Inline-edit one fact: the value becomes an input; Enter (or
   * blur) saves through edit_metadata, Esc cancels. The section
   * rebuilds from the updated cache either way, so a failed save
   * shows the file's real value again, never the rejected edit. */
  private editFact(
    item: Item,
    section: HTMLElement,
    tag: string,
    dd: HTMLElement,
  ): void {
    const current = dd.textContent ?? "";
    const input = document.createElement("input");
    input.className = "ql-fact-input";
    input.value = current;
    input.setAttribute("aria-label", `Edit ${tag}`);
    dd.replaceChildren(input);
    input.focus();
    input.select();
    let done = false;
    const finish = async (save: boolean): Promise<void> => {
      if (done) return;
      done = true;
      const value = input.value.trim();
      if (save && value !== current) {
        try {
          await api.editMetadata(item.id, [[tag, value]]);
          const cached = this.factsCache.get(item.id);
          const hit = cached?.find((t) => t.tag === tag);
          if (hit) hit.value = value;
          Toast.success(`Saved ${tag}.`);
        } catch (err) {
          Toast.error(`Could not save ${tag}: ${errText(err)}`);
        }
      }
      if (section.isConnected) {
        section.replaceWith(
          this.buildFactsSection(item, this.factsCache.get(item.id) ?? []),
        );
      }
    };
    input.addEventListener("keydown", (e) => {
      // The pane's keys (Space = Quick Look, arrows = selection)
      // must never fire while a fact is being typed.
      e.stopPropagation();
      if (e.key === "Enter") void finish(true);
      if (e.key === "Escape") void finish(false);
    });
    input.addEventListener("click", (e) => e.stopPropagation());
    input.addEventListener("blur", () => void finish(true));
  }
}
