/**
 * VideoPlayer — inline video playback for Quick Look / Gallery / the
 * pop-out player window.
 *
 * Dickles' spec (2026-10-13): transport controls appear on hover and
 * fade away after a few seconds of inactivity so playback is
 * uninterrupted; fullscreen on demand; detach to a pop-out window;
 * one click back from pop-out to the inline preview.
 */
import { fileSrc } from "./api";

const VIDEO_EXTS = new Set([
  "mp4", "webm", "mov", "m4v", "ogv", "mkv", "avi", "mpg", "mpeg",
]);

export function isVideoName(name: string): boolean {
  const ext = name.includes(".")
    ? name.slice(name.lastIndexOf(".") + 1).toLowerCase()
    : "";
  return VIDEO_EXTS.has(ext);
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

  constructor(opts: VideoPlayerOpts) {
    this.el = document.createElement("div");
    this.el.className = "vp" + (opts.standalone ? " vp-standalone" : "");

    this.video = document.createElement("video");
    this.video.className = "vp-video";
    this.video.src = fileSrc(opts.path);
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
    });
    this.video.addEventListener("pause", () => {
      this.playBtn.textContent = "▶";
      this.showControls(true);
    });
    this.video.addEventListener("timeupdate", () => this.syncProgress());
    this.video.addEventListener("loadedmetadata", () => this.syncProgress());
    this.video.addEventListener("ended", () => this.showControls(true));
    this.video.addEventListener("error", () => this.showFallback(), {
      once: true,
    });

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
    if (isFinite(d) && d > 0) {
      this.seek.value = String(Math.round((c / d) * 1000));
    }
    this.timeLabel.textContent = `${fmtTime(c)} / ${fmtTime(d)}`;
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
    this.bar.remove();
    this.video.remove();
    const note = document.createElement("div");
    note.className = "vp-fallback";
    note.textContent =
      "🎬 This video format can't play in the preview. Double-click the file to open it in your video player.";
    this.el.appendChild(note);
  }
}
