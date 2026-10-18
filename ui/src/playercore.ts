/**
 * The shared music transport (Music Glow-Up Phase 2, crew Diva).
 *
 * Every music surface — the Music Player panel, the Stage window,
 * the detached Mini, Quick Look's audio card — renders from the
 * Rust player core's snapshots and drives playback through the
 * pieces here:
 *
 * - PlayerCoreClient: the snapshot subscription + command verbs.
 * - OwnerEngine: a surface's single <audio> element, driven by
 *   snapshots. Only the owner's engine sounds; a claim revokes
 *   the previous owner and its engine goes silent — Dickles'
 *   talking-over bug (2026-10-16) is structural history.
 * - TransportBar: the one transport UI (family geometry), in
 *   full and compact arrangements.
 *
 * The engine also owns the DSD truth: DSF/DFF go straight to the
 * media server's FLAC transcode route; any other format that the
 * native pipeline refuses gets exactly one transcode retry before
 * an honest error. And it hosts the cast session — casting is a
 * property of the playing engine, so every surface casts.
 */
import {
  api,
  mediaSrc,
  onPlayerCore,
  onPlayerRevoked,
  onPlayerSpectrum,
  transcodeAudioSrc,
  type CastMirror,
  type PlayerOwner,
  type PlayerSnapshot,
  type QueueEntry,
} from "./api";
import { CastSession, type CastDeviceLike } from "./castsession";
import { errText } from "./toast";

export { onPlayerSpectrum };

export function extOf(name: string): string {
  const dot = name.lastIndexOf(".");
  return dot < 0 ? "" : name.slice(dot + 1).toLowerCase();
}

export function isDsdName(name: string): boolean {
  const e = extOf(name);
  return e === "dsf" || e === "dff";
}

export function fmtTime(secs: number): string {
  if (!Number.isFinite(secs) || secs < 0) return "–:––";
  const m = Math.floor(secs / 60);
  const s = Math.floor(secs % 60);
  return `${m}:${String(s).padStart(2, "0")}`;
}

export function fmtClock(secs: number): string {
  if (!Number.isFinite(secs) || secs < 0) return "0:00";
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = Math.floor(secs % 60);
  return h > 0
    ? `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`
    : `${m}:${String(s).padStart(2, "0")}`;
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

export class PlayerCoreClient {
  snap: PlayerSnapshot | null = null;
  private listeners = new Set<(s: PlayerSnapshot) => void>();
  private started = false;

  async start(): Promise<void> {
    if (this.started) return;
    this.started = true;
    try {
      this.snap = await api.playerState();
      this.emitLocal();
    } catch {
      // core unreachable — surfaces render their empty state
    }
    void onPlayerCore((s) => {
      this.snap = s;
      this.emitLocal();
    }).catch(() => undefined);
    void onPlayerRevoked((s) => {
      this.snap = s;
      this.emitLocal();
    }).catch(() => undefined);
  }

  onSnapshot(cb: (s: PlayerSnapshot) => void): () => void {
    this.listeners.add(cb);
    if (this.snap) cb(this.snap);
    return () => this.listeners.delete(cb);
  }

  private emitLocal(): void {
    if (!this.snap) return;
    for (const cb of [...this.listeners]) cb(this.snap);
  }

  isOwner(kind: PlayerOwner): boolean {
    return this.snap?.owner === kind;
  }

  currentEntry(): QueueEntry | null {
    if (!this.snap || this.snap.index === null) return null;
    return this.snap.queue[this.snap.index] ?? null;
  }

  // Command verbs (thin, snapshot-returning).
  openQueue(args: {
    owner: PlayerOwner;
    entries: QueueEntry[];
    startIndex?: number;
    startPosition?: number;
    autoplay?: boolean;
  }): Promise<PlayerSnapshot> {
    return api.playerOpenQueue(args);
  }
  claim(owner: PlayerOwner): Promise<PlayerSnapshot> {
    return api.playerClaim(owner);
  }
  toggle(): Promise<PlayerSnapshot> {
    return api.playerToggle();
  }
  step(dir: number): Promise<PlayerSnapshot> {
    return api.playerStep(dir);
  }
  playIndex(index: number): Promise<PlayerSnapshot> {
    return api.playerPlayIndex(index);
  }
  seek(secs: number): Promise<PlayerSnapshot> {
    return api.playerSeek(secs);
  }
  setRepeat(mode: PlayerSnapshot["repeat"]): Promise<PlayerSnapshot> {
    return api.playerSetRepeat(mode);
  }
  setShuffle(on: boolean): Promise<PlayerSnapshot> {
    return api.playerSetShuffle(on);
  }
  setVolume(v: number): Promise<PlayerSnapshot> {
    return api.playerSetVolume(v);
  }
  setMuted(m: boolean): Promise<PlayerSnapshot> {
    return api.playerSetMuted(m);
  }
  enqueue(entries: QueueEntry[], next?: boolean): Promise<PlayerSnapshot> {
    return api.playerEnqueue(entries, next);
  }
  remove(index: number): Promise<PlayerSnapshot> {
    return api.playerRemove(index);
  }
  clearQueue(keepCurrent?: boolean): Promise<PlayerSnapshot> {
    return api.playerClearQueue(keepCurrent);
  }
  reorder(from: number, to: number): Promise<PlayerSnapshot> {
    return api.playerReorder(from, to);
  }
  stop(): Promise<PlayerSnapshot> {
    return api.playerStop();
  }
  fact(kind: string, value?: number, message?: string): void {
    void api.playerFact(kind, value, message).catch(() => undefined);
  }

  /** MediaSession: OS media keys + the desktop's now-playing
   * surfaces drive the core; position state follows snapshots. */
  installMediaSession(meta: () => {
    title: string;
    artist?: string;
    album?: string;
    artwork?: string;
  }): void {
    try {
      if (!("mediaSession" in navigator)) return;
      const ms = navigator.mediaSession;
      ms.setActionHandler("play", () => void this.toggle().catch(() => undefined));
      ms.setActionHandler("pause", () => void this.toggle().catch(() => undefined));
      ms.setActionHandler("previoustrack", () => void this.step(-1).catch(() => undefined));
      ms.setActionHandler("nexttrack", () => void this.step(1).catch(() => undefined));
      ms.setActionHandler("seekto", (d) => {
        if (d.seekTime !== null && d.seekTime !== undefined) {
          void this.seek(d.seekTime).catch(() => undefined);
        }
      });
      this.onSnapshot((s) => {
        try {
          const e = this.currentEntry();
          if (!e) {
            ms.metadata = null;
            return;
          }
          const m = meta();
          ms.metadata = new MediaMetadata({
            title: m.title,
            artist: m.artist ?? "",
            album: m.album ?? "",
            artwork: m.artwork
              ? [{ src: m.artwork, sizes: "512x512", type: "image/png" }]
              : undefined,
          });
          ms.playbackState =
            s.state === "playing" ? "playing" : s.state === "paused" ? "paused" : "none";
          if (s.durationSecs > 0 && "setPositionState" in ms) {
            ms.setPositionState({
              duration: s.durationSecs,
              position: Math.min(s.positionSecs, s.durationSecs),
              playbackRate: 1,
            });
          }
        } catch {
          // MediaSession is a progressive enhancement only.
        }
      });
    } catch {
      // Not available in this webview — the transport still works.
    }
  }
}

// ---------------------------------------------------------------------------
// Owner engine
// ---------------------------------------------------------------------------

export interface EngineHooks {
  /** The user asked to cast (the surface renders the picker). */
  castPicker: (name: string, isVideo: boolean) => Promise<CastDeviceLike | null>;
  /** A snapshot was applied (surface re-renders chrome). */
  onSnapshot?: (s: PlayerSnapshot) => void;
  /** Terminal playback failure for the current entry. */
  onError?: (message: string) => void;
}

export class OwnerEngine {
  readonly el: HTMLAudioElement;
  cast: CastSession;
  private loadedToken = -1;
  private loadedPath: string | null = null;
  private transcoded = false;
  private streamBase = 0;
  private retried = false;
  private lastFactSent = 0;
  private destroyed = false;
  /** A cast session ended naturally: re-cast the next entry to
   * this device when its load lands. */
  private recastDevice: CastDeviceLike | null = null;
  /** Last snapshot the engine applied (transport decisions). */
  snap: PlayerSnapshot | null = null;

  constructor(
    private kind: PlayerOwner,
    private client: PlayerCoreClient,
    private hooks: EngineHooks,
  ) {
    this.el = new Audio();
    this.el.preload = "auto";
    this.cast = new CastSession({
      path: () => this.client.currentEntry()?.providerKey ?? "",
      localPosition: () => this.client.snap?.positionSecs ?? 0,
      localDuration: () => this.client.snap?.durationSecs ?? 0,
      pauseLocal: () => this.el.pause(),
      onNaturalEnd: () => {
        // The receiver played the entry out: the core advances
        // the queue, and the next entry goes to the same receiver
        // (album casting stays on the TV).
        this.recastDevice = this.cast.device;
        this.client.fact("ended");
      },
      resumeLocal: (at, play) => {
        // Back on this device: line the core clock up with where
        // the receiver left off, then follow the core's state.
        void this.client.seek(at).catch(() => undefined);
        try {
          this.el.currentTime = Math.max(0, at);
        } catch {
          // stream not seekable yet — the snapshot echo corrects
        }
        if (play && this.client.snap?.state !== "playing") {
          void this.client.toggle().catch(() => undefined);
        }
      },
      onMirror: (mirror) => {
        void api.playerSetCast(mirror).catch(() => undefined);
        if (mirror) {
          // The receiver's clock keeps the core honest while the
          // local element sits paused behind the cast badge.
          this.client.fact("position", mirror.positionSecs);
        }
      },
    });
    this.wireElement();
    this.client.onSnapshot((s) => this.apply(s));
  }

  private wireElement(): void {
    const el = this.el;
    el.addEventListener("loadedmetadata", () => {
      if (this.destroyed) return;
      const d = el.duration;
      if (Number.isFinite(d) && d > 0) this.client.fact("duration", d);
      const snap = this.client.snap;
      if (snap && !this.cast.active) {
        const target = snap.positionSecs;
        if (!this.transcoded && Math.abs(el.currentTime - target) > 1) {
          try {
            el.currentTime = Math.min(target, Math.max(0, d - 0.25));
          } catch {
            // not seekable yet — plays from the top instead
          }
        }
        if (snap.state === "playing") void el.play().catch(() => undefined);
      }
    });
    el.addEventListener("timeupdate", () => {
      if (this.destroyed || this.cast.active) return;
      const now = Date.now();
      if (now - this.lastFactSent < 1000) return;
      this.lastFactSent = now;
      this.client.fact("position", this.elementPosition());
    });
    el.addEventListener("play", () => {
      if (!this.destroyed && !this.cast.active) this.client.fact("playing");
    });
    el.addEventListener("pause", () => {
      if (!this.destroyed && !this.cast.active) this.client.fact("paused");
    });
    el.addEventListener("ended", () => {
      if (this.destroyed) return;
      if (this.cast.active) return;
      this.client.fact("ended");
    });
    el.addEventListener("error", () => {
      if (this.destroyed) return;
      if (!this.loadedPath || !el.src) return; // unload noise
      void this.handleError();
    });
  }

  /** The element's position in track time (transcode streams
   * count from their input-seek base). */
  elementPosition(): number {
    const c = this.el.currentTime || 0;
    return this.transcoded ? this.streamBase + c : c;
  }

  private async handleError(): Promise<void> {
    const entry = this.client.currentEntry();
    if (!entry) return;
    // One transcode retry for anything the native pipeline
    // refused (the DSD path is direct and never lands here first).
    if (!this.transcoded && !this.retried) {
      this.retried = true;
      const at = this.client.snap?.positionSecs ?? 0;
      const url = await transcodeAudioSrc(entry.providerKey, at);
      if (url && !this.destroyed) {
        this.transcoded = true;
        this.streamBase = Math.floor(at);
        this.el.src = url;
        if (this.client.snap?.state === "playing") {
          void this.el.play().catch(() => undefined);
        }
        return;
      }
    }
    const msg = isDsdName(entry.name)
      ? "This DSD file could not be decoded — the format may be damaged or unsupported."
      : "That track could not be played.";
    this.client.fact("error", undefined, msg);
    this.hooks.onError?.(msg);
  }

  /** Start casting the current entry (picker → session). */
  async startCastFlow(): Promise<void> {
    if (this.cast.active) {
      await this.cast.stop(true);
      return;
    }
    const entry = this.client.currentEntry();
    if (!entry) return;
    const name = entry.name.replace(/\.[^.]+$/, "");
    const device = await this.hooks.castPicker(name, false);
    if (!device || this.destroyed) return;
    await this.cast.start(device);
  }

  /** Transport verbs that know about cast: while a session runs,
   * the receiver is the transport. */
  async toggle(): Promise<void> {
    if (this.cast.active) {
      await this.cast.toggle();
      return;
    }
    await this.client.toggle();
  }

  async seekTo(secs: number): Promise<void> {
    if (this.cast.active) {
      await this.cast.seekTo(secs);
      return;
    }
    await this.client.seek(secs);
    // Local responsiveness: move the element now; the snapshot
    // echo confirms (drift adoption tolerates the small gap).
    if (!this.transcoded) {
      try {
        this.el.currentTime = secs;
      } catch {
        // not seekable yet
      }
    }
  }

  private apply(s: PlayerSnapshot): void {
    this.snap = s;
    this.hooks.onSnapshot?.(s);
    if (this.destroyed) return;
    const mine = s.owner === this.kind;
    if (!mine) {
      // Revoked or never ours: silence, deterministically.
      this.recastDevice = null;
      if (this.loadedPath !== null) {
        this.el.pause();
        this.el.removeAttribute("src");
        this.el.load();
        this.loadedPath = null;
        this.loadedToken = -1;
      }
      if (this.cast.active) this.cast.destroy();
      return;
    }
    // Volume/mute are core-owned (the one shared level).
    this.el.volume = s.volume;
    this.el.muted = s.muted;
    const entry = s.index !== null ? s.queue[s.index] : null;
    if (!entry) {
      if (this.loadedPath !== null) {
        this.el.pause();
        this.el.removeAttribute("src");
        this.el.load();
        this.loadedPath = null;
      }
      return;
    }
    if (s.playToken !== this.loadedToken || entry.providerKey !== this.loadedPath) {
      void this.load(entry, s);
      if (this.recastDevice) {
        const dev = this.recastDevice;
        this.recastDevice = null;
        void this.cast.start(dev);
      }
      return;
    }
    // Same entry: follow play state + seek drift.
    if (this.cast.active) return;
    if (s.state === "playing" && this.el.paused) {
      void this.el.play().catch(() => undefined);
    } else if (s.state !== "playing" && !this.el.paused) {
      this.el.pause();
    }
    if (s.state === "stopped") {
      try {
        this.el.currentTime = 0;
      } catch {
        // ignore
      }
      return;
    }
    const elPos = this.elementPosition();
    if (Math.abs(s.positionSecs - elPos) > 2) {
      if (this.transcoded) {
        void this.load(entry, s);
      } else {
        try {
          this.el.currentTime = s.positionSecs;
        } catch {
          // ignore
        }
      }
    }
  }

  private async load(entry: QueueEntry, s: PlayerSnapshot): Promise<void> {
    const token = s.playToken;
    this.loadedToken = token;
    this.loadedPath = entry.providerKey;
    this.retried = false;
    const startAt = s.positionSecs;
    try {
      if (isDsdName(entry.name)) {
        // The real DSD path: straight to the decode route, no
        // native attempt (the lying toast is gone).
        const url = await transcodeAudioSrc(entry.providerKey, startAt);
        if (this.destroyed || this.loadedToken !== token) return;
        if (!url) throw new Error("no transcode route");
        this.transcoded = true;
        this.streamBase = Math.floor(startAt);
        this.el.src = url;
      } else {
        const url = await mediaSrc(entry.providerKey);
        if (this.destroyed || this.loadedToken !== token) return;
        this.transcoded = false;
        this.streamBase = 0;
        this.el.src = url;
      }
      if (s.state === "playing" && !this.cast.active && !this.recastDevice) {
        void this.el.play().catch(() => undefined);
      }
    } catch (e) {
      if (this.loadedToken !== token) return;
      const msg = `That track could not be played. (${errText(e)})`;
      this.client.fact("error", undefined, msg);
      this.hooks.onError?.(msg);
    }
  }

  destroy(): void {
    this.destroyed = true;
    if (this.cast.active) this.cast.destroy();
    this.el.pause();
    this.el.removeAttribute("src");
    this.el.load();
    this.loadedPath = null;
  }
}

// ---------------------------------------------------------------------------
// Transport bar
// ---------------------------------------------------------------------------

export interface TransportHooks {
  onToggle: () => void;
  onStep: (dir: number) => void;
  onSeek: (secs: number) => void;
  onRepeat: () => void;
  onShuffle: () => void;
  onVolume: (v: number) => void;
  onMute: () => void;
  onCast: () => void;
}

/** The one transport, two arrangements (Forge §F geometry: 2rem
 * controls, pill buttons, 9px input radius). The scrubber is
 * pointer-true (no 0–1000 range input): drag with a live time
 * tooltip and a buffered hint underneath. */
export class TransportBar {
  readonly el: HTMLDivElement;
  private playBtn: HTMLButtonElement;
  private seekEl: HTMLDivElement;
  private seekFill: HTMLDivElement;
  private seekBuf: HTMLDivElement;
  private seekTip: HTMLDivElement;
  private timeEl: HTMLSpanElement;
  private repeatBtn: HTMLButtonElement;
  private shuffleBtn: HTMLButtonElement;
  private castBtn: HTMLButtonElement;
  private volEl: HTMLInputElement;
  private dragging = false;
  private lastSnap: PlayerSnapshot | null = null;
  private bufferedFrac = 0;

  constructor(
    private hooks: TransportHooks,
    opts?: { compact?: boolean },
  ) {
    this.el = document.createElement("div");
    this.el.className = "tbar" + (opts?.compact ? " tbar-compact" : "");
    this.el.innerHTML = `
      <button class="tbar-btn tbar-shuffle" title="Shuffle">🔀</button>
      <button class="tbar-btn" data-act="prev" title="Previous">⏮</button>
      <button class="tbar-btn tbar-play" title="Play / pause (Space)">▶</button>
      <button class="tbar-btn" data-act="next" title="Next">⏭</button>
      <button class="tbar-btn tbar-repeat" title="Repeat">🔁</button>
      <div class="tbar-seek">
        <div class="tbar-seek-buf"></div>
        <div class="tbar-seek-fill"></div>
        <div class="tbar-seek-tip" hidden>0:00</div>
      </div>
      <span class="tbar-time">0:00 / –:––</span>
      <button class="tbar-btn tbar-cast" title="Cast to a device…">📡</button>
      <input class="tbar-vol" type="range" min="0" max="100" title="Volume" />
    `;
    const q = <T extends HTMLElement>(sel: string) =>
      this.el.querySelector(sel) as T;
    this.playBtn = q<HTMLButtonElement>(".tbar-play");
    this.seekEl = q<HTMLDivElement>(".tbar-seek");
    this.seekFill = q<HTMLDivElement>(".tbar-seek-fill");
    this.seekBuf = q<HTMLDivElement>(".tbar-seek-buf");
    this.seekTip = q<HTMLDivElement>(".tbar-seek-tip");
    this.timeEl = q<HTMLSpanElement>(".tbar-time");
    this.repeatBtn = q<HTMLButtonElement>(".tbar-repeat");
    this.shuffleBtn = q<HTMLButtonElement>(".tbar-shuffle");
    this.castBtn = q<HTMLButtonElement>(".tbar-cast");
    this.volEl = q<HTMLInputElement>(".tbar-vol");
    this.playBtn.addEventListener("click", () => this.hooks.onToggle());
    q<HTMLButtonElement>('[data-act="prev"]').addEventListener("click", () =>
      this.hooks.onStep(-1),
    );
    q<HTMLButtonElement>('[data-act="next"]').addEventListener("click", () =>
      this.hooks.onStep(1),
    );
    this.repeatBtn.addEventListener("click", () => this.hooks.onRepeat());
    this.shuffleBtn.addEventListener("click", () => this.hooks.onShuffle());
    this.castBtn.addEventListener("click", () => this.hooks.onCast());
    this.volEl.addEventListener("input", () => {
      this.hooks.onVolume(Number(this.volEl.value) / 100);
    });
    this.volEl.addEventListener("dblclick", () => this.hooks.onMute());
    this.wireSeek();
  }

  private wireSeek(): void {
    const fracAt = (e: PointerEvent): number => {
      const r = this.seekEl.getBoundingClientRect();
      if (r.width <= 0) return 0;
      return Math.min(1, Math.max(0, (e.clientX - r.left) / r.width));
    };
    this.seekEl.addEventListener("pointerdown", (e) => {
      const dur = this.displayDuration();
      if (dur <= 0) return;
      this.dragging = true;
      this.seekEl.setPointerCapture(e.pointerId);
      this.showTip(fracAt(e));
    });
    this.seekEl.addEventListener("pointermove", (e) => {
      if (this.dragging) this.showTip(fracAt(e));
    });
    this.seekEl.addEventListener("pointerup", (e) => {
      if (!this.dragging) return;
      this.dragging = false;
      this.seekTip.hidden = true;
      const dur = this.displayDuration();
      if (dur > 0) this.hooks.onSeek(fracAt(e) * dur);
    });
    this.seekEl.addEventListener("pointercancel", () => {
      this.dragging = false;
      this.seekTip.hidden = true;
    });
  }

  private showTip(frac: number): void {
    const dur = this.displayDuration();
    this.seekTip.hidden = false;
    this.seekTip.textContent = fmtClock(frac * dur);
    this.seekTip.style.left = `${frac * 100}%`;
    this.seekFill.style.width = `${frac * 100}%`;
  }

  private displayDuration(): number {
    if (this.lastSnap?.cast) return this.lastSnap.cast.durationSecs;
    return this.lastSnap?.durationSecs ?? 0;
  }

  private displayPosition(): number {
    if (this.lastSnap?.cast) return this.lastSnap.cast.positionSecs;
    return this.lastSnap?.positionSecs ?? 0;
  }

  setBuffered(frac: number): void {
    this.bufferedFrac = Math.min(1, Math.max(0, frac));
    this.seekBuf.style.width = `${this.bufferedFrac * 100}%`;
  }

  update(s: PlayerSnapshot): void {
    this.lastSnap = s;
    const playing = s.cast ? s.cast.playing : s.state === "playing";
    this.playBtn.textContent = playing ? "❚❚" : "▶";
    const dur = this.displayDuration();
    const pos = this.displayPosition();
    if (!this.dragging) {
      const frac = dur > 0 ? Math.min(1, pos / dur) : 0;
      this.seekFill.style.width = `${frac * 100}%`;
    }
    this.timeEl.textContent = `${fmtClock(pos)} / ${dur > 0 ? fmtClock(dur) : "–:––"}`;
    this.repeatBtn.classList.toggle("on", s.repeat !== "off");
    this.repeatBtn.textContent = s.repeat === "one" ? "🔂" : "🔁";
    this.repeatBtn.title =
      s.repeat === "off" ? "Repeat: off" : s.repeat === "all" ? "Repeat: all" : "Repeat: one";
    this.shuffleBtn.classList.toggle("on", s.shuffle);
    if (document.activeElement !== this.volEl) {
      this.volEl.value = String(Math.round(s.volume * 100));
    }
    this.castBtn.classList.toggle("on", !!s.cast);
    this.castBtn.title = s.cast
      ? `Casting to ${s.cast.deviceName} — stop`
      : "Cast to a device…";
  }
}

// ---------------------------------------------------------------------------
// Spectrum — the audit's dot-matrix, fed through Rust.
// The OWNER's engine taps its element (WebAudio analyser), reduces
// it to 24 log-spaced bands, and sends them to the core, which
// rebroadcasts them to every window (`player-spectrum`). Surfaces
// render; only the owner ever computes. The tap is spike-gated:
// if WebKitGTK's media-element source misbehaves on this stack,
// enable() reports the failure and the surface shows why the
// toggle is off instead of faking a meter.
// ---------------------------------------------------------------------------

export const SPECTRUM_BANDS = 24;
export const SPECTRUM_ROWS = 12;

export class SpectrumFeed {
  private ctx: AudioContext | null = null;
  private analyser: AnalyserNode | null = null;
  private timer: number | null = null;
  private freq: Uint8Array | null = null;
  failed: string | null = null;

  constructor(
    private engine: OwnerEngine,
    private isOwner: () => boolean,
  ) {}

  /** Connect the tap. Returns false (with `failed` set) when the
   * platform refuses — the caller renders the reason. */
  enable(): boolean {
    if (this.ctx) {
      void this.ctx.resume().catch(() => undefined);
      this.startLoop();
      return true;
    }
    try {
      const AC: typeof AudioContext | undefined =
        window.AudioContext ??
        (window as unknown as { webkitAudioContext?: typeof AudioContext })
          .webkitAudioContext;
      if (!AC) {
        this.failed = "WebAudio is not available in this webview.";
        return false;
      }
      this.ctx = new AC();
      const src = this.ctx.createMediaElementSource(this.engine.el);
      const gain = this.ctx.createGain();
      this.analyser = this.ctx.createAnalyser();
      this.analyser.fftSize = 2048;
      this.analyser.smoothingTimeConstant = 0.72;
      src.connect(gain);
      gain.connect(this.ctx.destination);
      src.connect(this.analyser);
      this.freq = new Uint8Array(this.analyser.frequencyBinCount);
      void this.ctx.resume().catch(() => undefined);
      this.startLoop();
      return true;
    } catch (e) {
      this.failed = `Spectrum tap failed: ${errText(e)}`;
      this.ctx = null;
      this.analyser = null;
      return false;
    }
  }

  disable(): void {
    if (this.timer !== null) {
      window.clearInterval(this.timer);
      this.timer = null;
    }
    void api.playerSpectrum(new Array(SPECTRUM_BANDS).fill(0)).catch(() => undefined);
  }

  private startLoop(): void {
    if (this.timer !== null) return;
    this.timer = window.setInterval(() => {
      if (!this.analyser || !this.freq || !this.ctx) return;
      if (!this.isOwner()) return;
      this.analyser.getByteFrequencyData(
        this.freq as Uint8Array<ArrayBuffer>,
      );
      const bands = new Array(SPECTRUM_BANDS).fill(0) as number[];
      const minLog = Math.log10(40);
      const maxLog = Math.log10(16000);
      const nyquist = this.ctx.sampleRate / 2;
      for (let b = 0; b < SPECTRUM_BANDS; b++) {
        const f0 = Math.pow(10, minLog + ((maxLog - minLog) * b) / SPECTRUM_BANDS);
        const f1 = Math.pow(10, minLog + ((maxLog - minLog) * (b + 1)) / SPECTRUM_BANDS);
        const i0 = Math.max(1, Math.floor((f0 / nyquist) * this.freq.length));
        const i1 = Math.min(this.freq.length - 1, Math.max(i0 + 1, Math.floor((f1 / nyquist) * this.freq.length)));
        let peak = 0;
        for (let i = i0; i < i1; i++) peak = Math.max(peak, this.freq[i]);
        bands[b] = peak / 255;
      }
      void api.playerSpectrum(bands).catch(() => undefined);
    }, 50);
  }
}

/** Dot-matrix renderer: 24 columns × 12 rows of dots, lit from
 * the bottom by band level, in theme tokens only. */
export class SpectrumCanvas {
  readonly el: HTMLCanvasElement;
  private accent = "";
  private dim = "";
  private styleTick = 0;

  constructor() {
    this.el = document.createElement("canvas");
    this.el.className = "spectrum-canvas";
    this.el.width = 480;
    this.el.height = 96;
    this.el.setAttribute("aria-hidden", "true");
  }

  private readTokens(): void {
    const cs = getComputedStyle(document.body);
    this.accent = cs.getPropertyValue("--ufm-accent").trim() || "#d9930d";
    this.dim = cs.getPropertyValue("--ufm-border").trim() || "#888888";
  }

  draw(bands: number[]): void {
    const ctx = this.el.getContext("2d");
    if (!ctx) return;
    if (this.styleTick++ % 60 === 0 || !this.accent) this.readTokens();
    const w = this.el.width;
    const h = this.el.height;
    ctx.clearRect(0, 0, w, h);
    const cols = SPECTRUM_BANDS;
    const rows = SPECTRUM_ROWS;
    const cellW = w / cols;
    const cellH = h / rows;
    const r = Math.min(cellW, cellH) * 0.32;
    for (let c = 0; c < cols; c++) {
      const level = Math.min(1, Math.max(0, bands[c] ?? 0));
      const lit = Math.round(level * rows);
      for (let row = 0; row < rows; row++) {
        const cx = cellW * c + cellW / 2;
        const cy = h - (cellH * row + cellH / 2);
        ctx.beginPath();
        ctx.arc(cx, cy, r, 0, Math.PI * 2);
        ctx.fillStyle = row < lit ? this.accent : this.dim;
        ctx.globalAlpha = row < lit ? 0.95 : 0.25;
        ctx.fill();
      }
    }
    ctx.globalAlpha = 1;
  }
}
