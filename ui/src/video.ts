/**
 * VideoPlayer — inline video playback for Quick Look / Gallery / the
 * pop-out player window.
 *
 * Dickles' spec (2026-10-13): transport controls appear on hover and
 * fade away after a few seconds of inactivity so playback is
 * uninterrupted; fullscreen on demand; detach to a pop-out window;
 * one click back from pop-out to the inline preview.
 */
import { mediaSrc, transcodeSrc } from "./api";

const VIDEO_EXTS = new Set([
  "mp4", "webm", "mov", "m4v", "ogv", "mkv", "avi", "mpg", "mpeg",
]);

export function isVideoName(name: string): boolean {
  const ext = name.includes(".")
    ? name.slice(name.lastIndexOf(".") + 1).toLowerCase()
    : "";
  return VIDEO_EXTS.has(ext);
}

/** Audio extensions the players handle — the same set Quick Look
 * has always used (fix batch: one list, so the double-click law
 * and the previews can never disagree about what "playable"
 * means). */
export const AUDIO_EXTS = new Set([
  "mp3", "flac", "ogg", "oga", "wav", "m4a", "m4b", "opus", "aac",
  "aif", "aiff", "wma", "dsf", "dff",
]);

export function isAudioName(name: string): boolean {
  const ext = name.includes(".")
    ? name.slice(name.lastIndexOf(".") + 1).toLowerCase()
    : "";
  return AUDIO_EXTS.has(ext);
}

/** Playable media (fix batch item 7): video or audio. */
export function isPlayableName(name: string): boolean {
  return isVideoName(name) || isAudioName(name);
}

function fmtTime(sec: number): string {
  if (!isFinite(sec) || sec < 0) sec = 0;
  const m = Math.floor(sec / 60);
  const s = Math.floor(sec % 60);
  return `${m}:${String(s).padStart(2, "0")}`;
}

export interface VideoPlayerOpts {
  /** Absolute filesystem path of the video. */
  path: string;
  /** Pop-out window mode: bigger chrome, own close semantics. */
  standalone?: boolean;
  /** Show the detach (pop-out) button; called when clicked. */
  onDetach?: () => void;
  /** Show a "back to preview" button; called when clicked. */
  onReturn?: () => void;
  /** Detach handoff (UI round item 5): start position in seconds —
   * a detached player continues from where the inline one was, and
   * an inline player resuming from a detach starts where the mini
   * window left off. */
  startAt?: number;
  /** Detach handoff: keep playing once the source is ready. */
  autoPlay?: boolean;
  /** Detach window only: stream playback state home (on play /
   * pause / seek / end, and every few seconds while playing) so a
   * reattach — or a plain window close — resumes at the position
   * playback actually reached. */
  onState?: (t: number, playing: boolean) => void;
}

export class VideoPlayer {
  readonly el: HTMLDivElement;
  private video: HTMLVideoElement;
  private bar: HTMLDivElement;
  private playBtn: HTMLButtonElement;
  private seek: HTMLInputElement;
  private timeLabel: HTMLSpanElement;
  private hideTimer: number | null = null;
  private failed = false;
  /** Source path (for the transcode fallback). */
  private path: string;
  /** A real frame reached the screen (Round 7.1 watchdog). */
  private frameSeen = false;
  private watchdog: number | null = null;
  /** Subtle loading overlay while the transcode stream spins up
   * (inc-2 item 8: the fallback is automatic — no notice, no button). */
  private loadingEl: HTMLDivElement | null = null;
  /** Streaming-transcode mode: position = streamStart + currentTime. */
  private transcoding = false;
  private streamStart = 0;
  private origDuration = 0;
  /** Set by destroy(): late async continuations (mediaSrc, transcode
   * spin-up) must not resurrect a torn-down player. */
  private destroyed = false;
  /** Detach handoff state (UI round item 5). */
  private onStateCb: ((t: number, playing: boolean) => void) | null = null;
  private pendingSeek: number | null = null;
  private wantPlay = false;
  private stateTimer: number | null = null;

  /** The file this player is showing (hosts snapshot it before a
   * re-render tears the player down — fix batch item 1). */
  get sourcePath(): string {
    return this.path;
  }

  /** Current playback position in seconds — for a transcode stream
   * this is the position in the ORIGINAL file, not the stream. */
  position(): number {
    return this.transcoding
      ? this.streamStart + this.video.currentTime
      : this.video.currentTime;
  }

  isPlaying(): boolean {
    return !this.video.paused && !this.video.ended;
  }

  private emitState(): void {
    this.onStateCb?.(this.position(), this.isPlaying());
  }

  private armStateTimer(): void {
    if (!this.onStateCb || this.stateTimer !== null) return;
    this.stateTimer = window.setInterval(() => this.emitState(), 5000);
  }

  private stopStateTimer(): void {
    if (this.stateTimer !== null) {
      window.clearInterval(this.stateTimer);
      this.stateTimer = null;
    }
  }

  /** Tear the player down completely (fix block item 3): pause, drop
   * the source, cancel the watchdog + control-hide timers, leave
   * fullscreen, and remove the element. A detached <video> keeps
   * sounding in WebKitGTK after its surface closes — no media element
   * may outlive its surface. Hosts MUST call this. */
  destroy(): void {
    this.destroyed = true;
    this.stopStateTimer();
    if (this.hideTimer !== null) {
      window.clearTimeout(this.hideTimer);
      this.hideTimer = null;
    }
    if (this.watchdog !== null) {
      window.clearTimeout(this.watchdog);
      this.watchdog = null;
    }
    try {
      if (document.fullscreenElement === this.el) {
        void document.exitFullscreen().catch(() => undefined);
      }
      this.video.pause();
      this.video.removeAttribute("src");
      this.video.load();
    } catch {
      // element already gone — nothing left to stop
    }
    this.hideLoading();
    this.el.remove();
  }

  constructor(opts: VideoPlayerOpts) {
    this.path = opts.path;
    this.onStateCb = opts.onState ?? null;
    if (opts.startAt && opts.startAt > 0) this.pendingSeek = opts.startAt;
    this.wantPlay = opts.autoPlay ?? false;
    this.el = document.createElement("div");
    this.el.className = "vp" + (opts.standalone ? " vp-standalone" : "");

    this.video = document.createElement("video");
    this.video.className = "vp-video";
    // Stream from the loopback media server (HTTP Range) — the asset
    // protocol silently fails on real video in WebKitGTK.
    void mediaSrc(opts.path).then((url) => {
      if (!this.failed && !this.destroyed) {
        this.video.src = url;
        this.armWatchdog();
      }
    });
    this.video.preload = "metadata";
    this.video.playsInline = true;
    this.el.appendChild(this.video);

    this.bar = document.createElement("div");
    this.bar.className = "vp-bar show";

    this.playBtn = document.createElement("button");
    this.playBtn.className = "vp-btn";
    this.playBtn.textContent = "▶";
    this.playBtn.title = "Play / pause (Space)";
    this.playBtn.addEventListener("click", () => this.togglePlay());

    this.seek = document.createElement("input");
    this.seek.className = "vp-seek";
    this.seek.type = "range";
    this.seek.min = "0";
    this.seek.max = "1000";
    this.seek.value = "0";
    this.seek.addEventListener("input", () => {
      if (this.transcoding) {
        // The transcode stream is live: seeking restarts it at the
        // target offset (server-side input seek).
        const total = this.origDuration;
        if (total > 0) {
          const target = (Number(this.seek.value) / 1000) * total;
          const now = this.streamStart + this.video.currentTime;
          if (Math.abs(target - now) > 2) void this.startTranscode(target);
        }
        return;
      }
      const d = this.video.duration;
      if (isFinite(d) && d > 0) {
        this.video.currentTime = (Number(this.seek.value) / 1000) * d;
      }
    });

    this.timeLabel = document.createElement("span");
    this.timeLabel.className = "vp-time";
    this.timeLabel.textContent = "0:00 / 0:00";

    const muteBtn = document.createElement("button");
    muteBtn.className = "vp-btn";
    muteBtn.textContent = "🔊";
    muteBtn.title = "Mute (M)";
    muteBtn.addEventListener("click", () => {
      this.video.muted = !this.video.muted;
      muteBtn.textContent = this.video.muted ? "🔇" : "🔊";
    });

    this.bar.append(this.playBtn, this.seek, this.timeLabel, muteBtn);

    if (opts.onDetach) {
      const detachBtn = document.createElement("button");
      detachBtn.className = "vp-btn";
      detachBtn.textContent = "⧉";
      detachBtn.title = "Pop out into its own window";
      detachBtn.addEventListener("click", () => opts.onDetach!());
      this.bar.appendChild(detachBtn);
    }
    if (opts.onReturn) {
      const returnBtn = document.createElement("button");
      returnBtn.className = "vp-btn vp-return";
      returnBtn.textContent = "⤺ Preview";
      returnBtn.title = "Back to the file preview";
      returnBtn.addEventListener("click", () => opts.onReturn!());
      this.bar.appendChild(returnBtn);
    }
    const fsBtn = document.createElement("button");
    fsBtn.className = "vp-btn";
    fsBtn.textContent = "⛶";
    fsBtn.title = "Fullscreen (F) — Esc to leave";
    fsBtn.addEventListener("click", () => this.toggleFullscreen());
    this.bar.appendChild(fsBtn);

    this.el.appendChild(this.bar);

    // Playback wiring.
    this.video.addEventListener("play", () => {
      this.playBtn.textContent = "⏸";
      this.armHideTimer();
      this.emitState();
      this.armStateTimer();
    });
    this.video.addEventListener("pause", () => {
      this.playBtn.textContent = "▶";
      this.showControls(true);
      this.emitState();
      this.stopStateTimer();
    });
    this.video.addEventListener("seeked", () => this.emitState());
    this.video.addEventListener("timeupdate", () => {
      this.checkFrame();
      this.syncProgress();
    });
    this.video.addEventListener("loadedmetadata", () => {
      const d = this.video.duration;
      if (isFinite(d) && d > 0 && !this.transcoding) this.origDuration = d;
      // Detach handoff: land at the handed-off position, then
      // honor the handed-off play state.
      if (this.pendingSeek !== null && !this.transcoding) {
        const target = Math.min(
          this.pendingSeek,
          Math.max(0, (isFinite(d) ? d : 0) - 0.25),
        );
        this.pendingSeek = null;
        try {
          this.video.currentTime = target;
        } catch {
          // not seekable yet — the transcode fallback (if it
          // engages) starts from the same pending position
          this.pendingSeek = target;
        }
        if (this.wantPlay) {
          this.wantPlay = false;
          void this.video.play().catch(() => undefined);
        }
      } else if (this.wantPlay) {
        // Autoplay with no seek to land first (fix batch item 1):
        // wantPlay used to be consumed ONLY inside the pending-seek
        // branch, so a player opened at 0 with autoplay=true sat
        // paused forever — Dickles' "Space opens a paused preview".
        this.wantPlay = false;
        void this.video.play().catch(() => undefined);
      }
      this.syncProgress();
    });
    this.video.addEventListener("loadeddata", () => this.checkFrame());
    this.video.addEventListener("canplay", () => this.checkFrame());
    this.video.addEventListener("ended", () => {
      this.showControls(true);
      this.emitState();
      this.stopStateTimer();
    });
    this.video.addEventListener("error", () => this.enterFallback());

    // Interaction: click toggles play, double-click fullscreen,
    // any pointer activity wakes the controls.
    this.video.addEventListener("click", () => this.togglePlay());
    this.video.addEventListener("dblclick", () => this.toggleFullscreen());
    this.el.addEventListener("pointermove", () => this.showControls(false));
    this.el.addEventListener("pointerdown", () => this.showControls(false));
    this.el.addEventListener("pointerleave", () => {
      if (!this.video.paused) this.armHideTimer();
    });
    this.el.addEventListener("keydown", (e) => this.onKey(e));
    this.el.tabIndex = 0;
    this.showControls(true);
  }

  private onKey(e: KeyboardEvent): void {
    if (e.key === " " || e.key.toLowerCase() === "k") {
      e.preventDefault();
      // The key is handled HERE (fix batch item 1): without the
      // stop, the shell's document-level Space handler ALSO fires
      // and the two toggles cancel out / close the preview.
      e.stopPropagation();
      this.togglePlay();
    } else if (e.key === "ArrowRight") {
      this.video.currentTime = Math.min(
        this.video.duration || 0,
        this.video.currentTime + 5,
      );
    } else if (e.key === "ArrowLeft") {
      this.video.currentTime = Math.max(0, this.video.currentTime - 5);
    } else if (e.key.toLowerCase() === "f") {
      this.toggleFullscreen();
    } else if (e.key.toLowerCase() === "m") {
      this.video.muted = !this.video.muted;
    } else {
      return;
    }
    this.showControls(false);
  }

  togglePlay(): void {
    if (this.failed) return;
    if (this.video.paused) void this.video.play().catch(() => undefined);
    else this.video.pause();
  }

  private toggleFullscreen(): void {
    if (document.fullscreenElement) {
      void document.exitFullscreen().catch(() => undefined);
    } else {
      void this.el.requestFullscreen().catch(() => undefined);
    }
    this.showControls(false);
  }

  private syncProgress(): void {
    const d = this.video.duration;
    const c = this.video.currentTime;
    if (this.transcoding) {
      const total = this.origDuration > 0 ? this.origDuration : d;
      const cur = this.streamStart + c;
      if (isFinite(total) && total > 0) {
        this.seek.value = String(Math.round((cur / total) * 1000));
      }
      this.timeLabel.textContent = `${fmtTime(cur)} / ${fmtTime(total)}`;
      return;
    }
    if (isFinite(d) && d > 0) {
      this.seek.value = String(Math.round((c / d) * 1000));
    }
    this.timeLabel.textContent = `${fmtTime(c)} / ${fmtTime(d)}`;
  }

  /** A frame counts as presented only when the element has dimensions
   * AND buffered current data — metadata alone (the HEVC black-screen
   * case) does not qualify. */
  private checkFrame(): void {
    if (this.video.videoWidth > 0 && this.video.readyState >= 2) {
      this.frameSeen = true;
      this.hideLoading();
    }
  }

  /** First-frame watchdog (Round 7.1): WebKitGTK loads metadata and
   * even seeks HEVC 10-bit files but never presents a frame. If no
   * frame lands within ~2.5s of the source being set, the
   * streaming-transcode fallback engages AUTOMATICALLY (inc-2
   * item 8 — Dickles: the old notice + "Transcode & Play" button was
   * nagging; the player just plays). While transcoding, the watchdog
   * re-arms longer: a stream that produces neither frames nor an
   * error within ~8s counts as failed. */
  private armWatchdog(): void {
    if (this.watchdog !== null) window.clearTimeout(this.watchdog);
    const v = this.video as HTMLVideoElement & {
      requestVideoFrameCallback?: (cb: () => void) => void;
    };
    try {
      v.requestVideoFrameCallback?.(() => {
        this.frameSeen = true;
        this.hideLoading();
      });
    } catch {
      // rVFC is a bonus signal; readyState checks still apply
    }
    const timeout = this.transcoding ? 8000 : 2500;
    this.watchdog = window.setTimeout(() => {
      this.watchdog = null;
      this.checkFrame();
      if (this.frameSeen || this.failed) return;
      if (this.transcoding) {
        this.showFallback();
        return;
      }
      this.enterFallback();
    }, timeout);
  }

  /** Native playback failed (watchdog or error event): switch to the
   * transcode stream and play — no notice card, no button, no user
   * action. Only a subtle loading indicator while the stream spins
   * up. If the transcode stream itself is what failed, land on the
   * quiet one-line note (showFallback). */
  private enterFallback(): void {
    if (this.failed) return;
    if (this.transcoding) {
      this.showFallback();
      return;
    }
    this.checkFrame();
    if (this.frameSeen) return;
    void this.startTranscode(this.video.currentTime || 0);
  }

  private showLoading(): void {
    if (this.loadingEl) return;
    const wrap = document.createElement("div");
    wrap.className = "vp-loading";
    const spinner = document.createElement("div");
    spinner.className = "vp-loading-spinner";
    const caption = document.createElement("div");
    caption.className = "vp-loading-caption";
    caption.textContent = "Preparing playback…";
    wrap.append(spinner, caption);
    this.loadingEl = wrap;
    this.el.appendChild(wrap);
  }

  private hideLoading(): void {
    if (this.loadingEl) {
      this.loadingEl.remove();
      this.loadingEl = null;
    }
  }

  private async startTranscode(startSecs: number): Promise<void> {
    // A detach handoff position that native playback never got to
    // apply is exactly where the transcode stream should start.
    if (this.pendingSeek !== null) {
      startSecs = this.pendingSeek;
      this.pendingSeek = null;
      this.wantPlay = false; // startTranscode always plays
    }
    this.showLoading();
    const url = await transcodeSrc(this.path, startSecs);
    if (this.destroyed) return;
    if (!url) {
      this.showFallback();
      return;
    }
    this.transcoding = true;
    this.streamStart = startSecs;
    this.frameSeen = false;
    this.video.src = url;
    this.armWatchdog();
    try {
      await this.video.play();
    } catch {
      // autoplay refused — the transport bar's play button still works
    }
  }

  private showControls(sticky: boolean): void {
    this.bar.classList.add("show");
    this.el.classList.remove("hidecursor");
    if (this.hideTimer !== null) {
      window.clearTimeout(this.hideTimer);
      this.hideTimer = null;
    }
    if (!sticky && !this.video.paused) this.armHideTimer();
  }

  private armHideTimer(): void {
    if (this.hideTimer !== null) window.clearTimeout(this.hideTimer);
    this.hideTimer = window.setTimeout(() => {
      this.bar.classList.remove("show");
      this.el.classList.add("hidecursor");
      this.hideTimer = null;
    }, 2600);
  }

  private showFallback(): void {
    this.failed = true;
    if (this.watchdog !== null) {
      window.clearTimeout(this.watchdog);
      this.watchdog = null;
    }
    this.hideLoading();
    this.bar.remove();
    // Silence before detaching: a removed-but-loaded <video> can
    // keep sounding (the ghost-playback class, fix block item 3).
    try {
      this.video.pause();
      this.video.removeAttribute("src");
      this.video.load();
    } catch {
      // already detached
    }
    this.video.remove();
    // Quiet single-line inline note (inc-2 item 8) — the old
    // full-card notice is gone by Dickles' order.
    const note = document.createElement("div");
    note.className = "vp-note";
    note.textContent =
      "🎬 Can't play this video here — double-click to open it in your video player.";
    this.el.appendChild(note);
  }
}
