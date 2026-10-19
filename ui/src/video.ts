/**
 * VideoPlayer — inline video playback for Quick Look / Gallery / the
 * pop-out player window.
 *
 * Dickles' spec (2026-10-13): transport controls appear on hover and
 * fade away after a few seconds of inactivity so playback is
 * uninterrupted; fullscreen on demand; detach to a pop-out window;
 * one click back from pop-out to the inline preview.
 */
import { api, mediaSrc, transcodeSrc } from "./api";
import { Toast, errText } from "./toast";

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

/** Stream-truth routing for previews (Usher, 2026-10-18): the
 * probed class outranks the filename. An .mp4 whose streams are
 * audio + cover art routes to the AUDIO card; a genuine video
 * routes to the player; while the probe is in flight the route is
 * "wait" and no player mounts; an unprobeable file falls back to
 * the extension guess it has always had. */
export type MediaRoute = "video" | "audio" | "wait" | "none";

export function mediaRouteFor(
  name: string,
  mc: MediaClass | null,
  pending: boolean,
): MediaRoute {
  if (pending) return "wait";
  if (mc?.media === "audio") return "audio";
  if (mc?.media === "video") return "video";
  if (isVideoName(name)) return "video";
  if (isAudioName(name)) return "audio";
  return "none";
}

/** Human names for the codecs the fallback note can blame. */
const CODEC_NAMES: Record<string, string> = {
  h264: "H.264",
  hevc: "HEVC",
  av1: "AV1",
  vp9: "VP9",
  vp8: "VP8",
  mpeg4: "MPEG-4",
  mpeg2video: "MPEG-2",
  theora: "Theora",
  wmv3: "WMV",
};

function codecDisplayName(codec: string): string {
  return CODEC_NAMES[codec.toLowerCase()] ?? codec.toUpperCase();
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
  /** Embedded in the Quick Look pane: the pane provides Cast (its
   *  header button) and Detach (its always-visible card button)
   *  itself, so the bar omits its duplicates — at the pane's width
   *  the full roster overflows the bar and the tail buttons clip
   *  (measured: fullscreen/cast pushed past the player's edge). */
  preview?: boolean;
  /** Show a "back to preview" button; called when clicked. */
  onReturn?: () => void;
  /** Stream-probed video codec (Usher): named in the fallback
   * note when playback fails, so the dead end says WHY. */
  codecName?: string | null;
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

/** A cast target as the player needs it — the shared session's
 * type (castsession.ts), re-exported for existing importers. */
export type { CastDeviceLike } from "./castsession";
import { CastSession, type CastDeviceLike } from "./castsession";
import type { CastMirror, MediaClass } from "./api";

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
  /** Probed video codec for the honest fallback note. */
  private codecName: string | null = null;
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
  /** Player round — cast session state. While castDevice is set
   * the RECEIVER owns playback: transport routes to it, the
   * scrubber shows its position (polled every 2 s), and the local
   * element stays paused behind the "Casting to …" badge. */
  private castPicker: ((name: string, video: boolean) => Promise<CastDeviceLike | null>) | null = null;
  /** The cast session itself lives in castsession.ts (shared
   * with the music surfaces since the Glow-Up); this class only
   * renders its mirror into the badge + seek bar below. */
  private castSession: CastSession | null = null;
  private get castDevice(): CastDeviceLike | null {
    return this.castSession?.device ?? null;
  }
  private castBtn: HTMLButtonElement | null = null;
  private castBadge: HTMLDivElement | null = null;
  private castBadgeText: HTMLSpanElement | null = null;
  private castPos = 0;
  private castDur = 0;
  private castPlaying = false;
  private castScrubbing = false;
  /** Grace window at session start: the receiver may still be
   * loading, so position polls are ignored until the initial
   * seek (to the local position) has had time to land. */
  /** (The seek-grace clock lives in the session now.) */
  /** Gauntlet G1 — playback setup strip: appears when the host
   * GStreamer plugin set is missing (checked once per player:
   * at standalone-open, or at the first playback failure for
   * inline players — never on scroll). Non-modal by design:
   * nothing prompts until HE clicks the strip's button, and
   * the pkexec prompt that follows belongs to that click. */
  private setupStrip: HTMLDivElement | null = null;
  private setupChecked = false;
  private setupBusy = false;
  private setupRepoName: string | null = null;

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
    // Player round: never leave a receiver playing from a dead
    // surface — stop the cast session on the way out (no local
    // resume; the surface is gone).
    if (this.castSession) {
      this.teardownCast();
    }
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
    this.codecName = opts.codecName ?? null;
    this.onStateCb = opts.onState ?? null;
    if (opts.startAt && opts.startAt > 0) this.pendingSeek = opts.startAt;
    this.wantPlay = opts.autoPlay ?? false;
    this.el = document.createElement("div");
    this.el.className = "vp" + (opts.standalone ? " standalone" : "");

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
      if (this.castDevice) {
        // Scrubbing the RECEIVER: the readout follows the thumb
        // now; the seek itself goes out on release (change).
        const total = this.castDur;
        if (total > 0) {
          this.castPos = (Number(this.seek.value) / 1000) * total;
          this.timeLabel.textContent = `${fmtTime(this.castPos)} / ${fmtTime(total)}`;
        }
        return;
      }
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
    this.seek.addEventListener("change", () => {
      this.castScrubbing = false;
      if (this.castSession) this.castSession.scrubbing = false;
      if (this.castDevice && this.castDur > 0) {
        const target = (Number(this.seek.value) / 1000) * this.castDur;
        this.castPos = target;
        void this.castSession?.seekTo(target);
      }
    });
    this.seek.addEventListener("pointerdown", () => {
      this.castScrubbing = true;
      if (this.castSession) this.castSession.scrubbing = true;
    });
    this.seek.addEventListener("pointerup", () => {
      this.castScrubbing = false;
      if (this.castSession) this.castSession.scrubbing = false;
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

    if (opts.onDetach && !opts.preview) {
      const detachBtn = document.createElement("button");
      detachBtn.className = "vp-btn";
      detachBtn.textContent = "⧉";
      detachBtn.title = "Pop out into its own window";
      detachBtn.addEventListener("click", () => opts.onDetach!());
      this.bar.appendChild(detachBtn);
    }
    if (opts.onReturn) {
      // Return home (Dickles 2026-10-15): a detached player must
      // never read as a one-way door. The old in-bar button hid
      // with the auto-fading transport bar — invisible exactly
      // while a film plays. The Return control is now a labeled
      // chip pinned to the player's top-right corner (the cast
      // badge's visual language), on screen for the whole show.
      const chip = document.createElement("button");
      chip.className = "vp-return-chip";
      chip.textContent = "⤺ Return";
      chip.title = "Return to the preview in the main window";
      chip.setAttribute("aria-label", "Return to preview");
      chip.addEventListener("click", () => opts.onReturn!());
      this.el.appendChild(chip);
    }
    const fsBtn = document.createElement("button");
    fsBtn.className = "vp-btn";
    fsBtn.textContent = "⛶";
    fsBtn.title = "Fullscreen (F) — Esc to leave";
    fsBtn.addEventListener("click", () => this.toggleFullscreen());
    this.bar.appendChild(fsBtn);

    // Cast (Player round): pick a Chromecast / AirPlay / DLNA /
    // Sonos target; while a session runs, this button stops it
    // and playback resumes here at the receiver's position.
    const castBtn = document.createElement("button");
    castBtn.className = "vp-btn";
    castBtn.textContent = "📡";
    castBtn.title = "Cast to a device…";
    castBtn.setAttribute("aria-label", "Cast to a device");
    castBtn.addEventListener("click", () => void this.openCastPicker());
    if (!opts.preview) this.bar.appendChild(castBtn);
    this.castBtn = castBtn;

    this.el.appendChild(this.bar);

    // "Casting to <device>" badge — the local picture is paused
    // while the TV plays; the badge is the honest state, with the
    // way back (Stop returns playback here, same position).
    this.castBadge = document.createElement("div");
    this.castBadge.className = "vp-cast-badge";
    this.castBadge.hidden = true;
    this.castBadgeText = document.createElement("span");
    this.castBadgeText.className = "vp-cast-badge-text";
    const badgeStop = document.createElement("button");
    badgeStop.className = "vp-cast-stop";
    badgeStop.textContent = "Stop casting";
    badgeStop.addEventListener("click", () => void this.stopCast(true));
    this.castBadge.append(this.castBadgeText, badgeStop);
    this.el.appendChild(this.castBadge);

    // Playback setup strip (Gauntlet G1): created hidden — the
    // setup check decides whether it ever appears.
    this.setupStrip = document.createElement("div");
    this.setupStrip.className = "vp-setup";
    this.setupStrip.hidden = true;
    this.el.appendChild(this.setupStrip);

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
    // A pop-out player is a door the user deliberately opened,
    // so its playback-setup check runs up front. Inline (Quick
    // Look) players check at the first playback failure
    // instead (see enterFallback) — never on scroll.
    if (opts.standalone) void this.checkPlaybackSetup();
  }

  /** One-shot playback-setup check (Gauntlet G1): if the host
   * GStreamer set is missing, offer the setup on the strip —
   * in context, in words, with the install behind a button
   * HE clicks. Audio players skip it (no picture pipeline). */
  private async checkPlaybackSetup(): Promise<void> {
    if (this.setupChecked || this.destroyed) return;
    this.setupChecked = true;
    if (isAudioName(this.path)) return;
    try {
      const st = await api.playbackSetupStatus();
      if (this.destroyed || st.pluginsPresent) return;
      this.setupRepoName = st.repoName;
      this.renderSetupStrip(
        "This system is missing the pieces that let Fable play video.",
        "Set up playback",
        false,
      );
    } catch {
      // A failed status probe is not a playback error — the
      // player itself will say what it has to say.
    }
  }

  private renderSetupStrip(text: string, btnLabel: string, withRepo: boolean): void {
    const strip = this.setupStrip;
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
      if (this.destroyed) return;
      if (st.pluginsPresent) {
        this.renderSetupDone();
      } else if (st.repoName && !st.repoEnabled) {
        // The stock set went in and playback is STILL
        // incomplete: on this family the rest lives in the
        // third-party source. Name it, in words, and let the
        // second click be the consent (Q4).
        this.setupRepoName = st.repoName;
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
      if (this.destroyed) return;
      // The backend's calm sentence is the receipt; the button
      // beside it stays the retry.
      this.renderSetupStrip(String(e), "Try again", withRepo);
    }
  }

  private renderSetupDone(): void {
    const strip = this.setupStrip;
    if (!strip) return;
    strip.replaceChildren();
    const span = document.createElement("span");
    span.className = "vp-setup-text";
    span.textContent =
      "Playback is set up — if this video already gave up, reopen it and it will play.";
    strip.appendChild(span);
    strip.hidden = false;
    window.setTimeout(() => {
      if (!this.destroyed && this.setupStrip) this.setupStrip.hidden = true;
    }, 6000);
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
      if (this.castDevice) {
        this.castPos = Math.min(this.castDur || 0, this.castPos + 5);
        void this.castSession?.seekTo(this.castPos);
        this.renderCastProgress();
      } else {
        this.video.currentTime = Math.min(
          this.video.duration || 0,
          this.video.currentTime + 5,
        );
      }
    } else if (e.key === "ArrowLeft") {
      if (this.castDevice) {
        this.castPos = Math.max(0, this.castPos - 5);
        void this.castSession?.seekTo(this.castPos);
        this.renderCastProgress();
      } else {
        this.video.currentTime = Math.max(0, this.video.currentTime - 5);
      }
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
    if (this.castDevice) {
      void this.castToggle();
      return;
    }
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

  /** Hosts with a shared CastDialog (the shell) wire it here so
   * the dialog is the app's one instance; hosts without one (the
   * pop-out window) get a lazily-built dialog from openCastPicker. */
  setCastPicker(
    fn: (name: string, video: boolean) => Promise<CastDeviceLike | null>,
  ): void {
    this.castPicker = fn;
  }

  private async openCastPicker(): Promise<void> {
    if (this.castDevice) {
      // The button is also the way out while a session runs.
      void this.stopCast(true);
      return;
    }
    if (!this.castPicker) {
      try {
        const { CastDialog } = await import("./cast");
        const dlg = new CastDialog();
        this.el.appendChild(dlg.element);
        this.castPicker = (n, v) => dlg.openPicker(n, v);
      } catch {
        return;
      }
    }
    const name = this.path.split("/").pop() ?? "Video";
    const device = await this.castPicker(name, true);
    if (!device || this.destroyed) return;
    await this.startCast(device);
  }

  private async startCast(device: CastDeviceLike): Promise<void> {
    // The session (castsession.ts) owns the receiver from here:
    // register + play, the seek-grace, the 2 s poll, the natural-
    // end handback — this class only renders the mirror.
    const session = new CastSession({
      path: () => this.path,
      localPosition: () => this.position(),
      localDuration: () => this.video.duration || 0,
      pauseLocal: () => this.video.pause(),
      resumeLocal: (at, play) => this.resumeLocal(at, play),
      onMirror: (m) => this.renderCastMirror(m),
    });
    this.castSession = session;
    await session.start(device);
    if (!session.active && this.castSession === session) {
      // Start failed (the session already toasted why).
      this.castSession = null;
    }
  }

  /** Render the session's mirror into the badge + seek bar. */
  private renderCastMirror(m: CastMirror | null): void {
    if (!m) {
      this.castPlaying = false;
      if (this.castBadge) this.castBadge.hidden = true;
      if (this.castBtn) this.castBtn.title = "Cast to a device…";
      this.playBtn.textContent = "▶";
      return;
    }
    if (!this.castScrubbing) this.castPos = m.positionSecs;
    if (m.durationSecs > 0) this.castDur = m.durationSecs;
    this.castPlaying = m.playing;
    if (this.castBadge && this.castBadgeText) {
      this.castBadgeText.textContent = `📡 Casting to ${m.deviceName}`;
      this.castBadge.hidden = false;
    }
    if (this.castBtn) this.castBtn.title = "Stop casting";
    this.playBtn.textContent = this.castPlaying ? "⏸" : "▶";
    this.renderCastProgress();
  }

  /** Stop the receiver and (normally) resume locally at the
   * position the receiver had reached — the round trip Dickles
   * asked for: the TV hands the film back where it left off. */
  private async stopCast(resumeLocal: boolean): Promise<void> {
    const session = this.castSession;
    if (!session) return;
    this.castSession = null;
    await session.stop(resumeLocal);
  }

  private teardownCast(): void {
    const session = this.castSession;
    this.castSession = null;
    session?.destroy();
  }

  private resumeLocal(at: number, play: boolean): void {
    const d = this.video.duration;
    if (at > 0 && isFinite(d) && d > 0) {
      try {
        this.video.currentTime = Math.min(at, Math.max(0, d - 0.25));
      } catch {
        // not seekable yet — starting from the top beats hanging
      }
    }
    if (play) void this.video.play().catch(() => undefined);
  }

  private async castToggle(): Promise<void> {
    await this.castSession?.toggle();
  }

  private renderCastProgress(): void {
    const total = this.castDur;
    if (!(total > 0)) return;
    if (!this.castScrubbing) {
      this.seek.value = String(Math.round((this.castPos / total) * 1000));
    }
    this.timeLabel.textContent = `${fmtTime(this.castPos)} / ${fmtTime(total)}`;
  }

  private syncProgress(): void {
    if (this.castDevice) {
      // The receiver's clock drives the bar while casting (the
      // poll keeps castPos fresh); local timeupdate is paused.
      this.renderCastProgress();
      return;
    }
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
    // A first playback failure is also the inline player's
    // playback-setup moment (Gauntlet G1): if the host plugin
    // set is what's missing, the strip offers the fix while
    // the transcode fallback spins up either way.
    void this.checkPlaybackSetup();
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
    note.textContent = this.codecName
      ? `🎬 Can't play this video here — the built-in player doesn't support its ${codecDisplayName(this.codecName)} video. Double-click to open it in your video player.`
      : "🎬 Can't play this video here — double-click to open it in your video player.";
    this.el.appendChild(note);
  }
}
