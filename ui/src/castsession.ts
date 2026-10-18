/**
 * The cast session, shared (Music Glow-Up Phase 2, crew Diva).
 * Extracted from VideoPlayer, where the whole session — picker,
 * start/stop, the 2 s status poll, the resume-at handback —
 * lived welded to the video surface. Music surfaces cast too
 * now, so the session is its own object and each host renders
 * its state (badge, transport) in its own arrangement.
 *
 * Behavior is the shipped video behavior, unchanged: receivers
 * start at 0, so a start seeks to the local position once the
 * receiver has loaded (polls ignore position through the grace
 * window); stopping hands playback back to the local surface at
 * the receiver's position; a receiver that ends on its own ends
 * the session and resumes locally.
 */
import { api, type CastMirror } from "./api";
import { Toast } from "./toast";
import { errText } from "./toast";

export interface CastDeviceLike {
  name: string;
  kind: string;
  address: string;
}

export interface CastSessionHooks {
  /** The path being cast (register + play target). */
  path: () => string;
  /** Local position/duration at start (the hand-in point). */
  localPosition: () => number;
  localDuration: () => number;
  /** Pause the local element (the receiver is the sound now). */
  pauseLocal: () => void;
  /** Resume locally at a position (the hand-back). */
  resumeLocal: (at: number, play: boolean) => void;
  /** The receiver ended the media on its own (queue hosts use
   * this to advance and re-cast the next entry). When provided,
   * it REPLACES the resumeLocal hand-back for a natural end. */
  onNaturalEnd?: () => void;
  /** Mirror state changed — the host re-renders its cast UI and
   * forwards the mirror to the player core when it owns it. */
  onMirror: (mirror: CastMirror | null) => void;
}

export class CastSession {
  device: CastDeviceLike | null = null;
  pos = 0;
  dur = 0;
  playing = false;
  /** Hosts set this while their scrubber is being dragged. */
  scrubbing = false;
  private sawPlaying = false;
  private seekAt = 0;
  private timer: number | null = null;
  private destroyed = false;

  constructor(private hooks: CastSessionHooks) {}

  get active(): boolean {
    return this.device !== null;
  }

  private emitMirror(): void {
    if (this.device) {
      this.hooks.onMirror({
        deviceName: this.device.name,
        playing: this.playing,
        positionSecs: this.pos,
        durationSecs: this.dur,
      });
    } else {
      this.hooks.onMirror(null);
    }
  }

  async start(device: CastDeviceLike): Promise<void> {
    const resumeAt = this.hooks.localPosition();
    try {
      const url = await api.castRegisterPath(this.hooks.path());
      await api.castPlay(device, url);
    } catch (err) {
      Toast.error(`Cast failed: ${errText(err)}`);
      return;
    }
    if (this.destroyed) {
      void api.castStop(device).catch(() => undefined);
      return;
    }
    this.device = device;
    this.pos = resumeAt;
    const d = this.hooks.localDuration();
    this.dur = isFinite(d) && d > 0 ? d : 0;
    this.playing = true;
    this.sawPlaying = false;
    this.hooks.pauseLocal();
    // Receivers start at 0; seek to where local playback was once
    // the receiver has had a moment to load (polls ignore position
    // during the grace window so the bar doesn't snap to 0).
    if (resumeAt > 1) {
      this.seekAt = Date.now() + 2500;
      window.setTimeout(() => {
        if (this.device === device) {
          void api.castSeek(device, resumeAt).catch(() => undefined);
        }
      }, 1200);
    }
    if (this.timer !== null) window.clearInterval(this.timer);
    this.timer = window.setInterval(() => void this.poll(), 2000);
    void this.poll();
    this.emitMirror();
    Toast.success(`Casting to ${device.name}.`);
  }

  /** Stop the receiver and (normally) resume locally at the
   * position the receiver had reached. */
  async stop(resumeLocal: boolean): Promise<void> {
    const dev = this.device;
    if (!dev) return;
    const at = this.pos;
    const wasPlaying = this.playing;
    this.teardown();
    try {
      await api.castStop(dev);
    } catch {
      // The receiver may already be gone; local resume proceeds.
    }
    if (resumeLocal && !this.destroyed) {
      this.hooks.resumeLocal(at, wasPlaying);
      Toast.success(`Back on this device — stopped casting to ${dev.name}.`);
    }
  }

  /** Silent teardown (surface destroyed): stop the receiver
   * without resuming anywhere. */
  destroy(): void {
    this.destroyed = true;
    const dev = this.device;
    this.teardown();
    if (dev) void api.castStop(dev).catch(() => undefined);
  }

  private teardown(): void {
    this.device = null;
    if (this.timer !== null) {
      window.clearInterval(this.timer);
      this.timer = null;
    }
    this.emitMirror();
  }

  async toggle(): Promise<void> {
    const dev = this.device;
    if (!dev) return;
    try {
      if (this.playing) {
        await api.castPause(dev);
        this.playing = false;
      } else {
        await api.castResume(dev);
        this.playing = true;
      }
    } catch {
      // A failed toggle resyncs on the next poll — no dead end.
    }
    this.emitMirror();
  }

  async seekTo(secs: number): Promise<void> {
    const dev = this.device;
    if (!dev) return;
    this.pos = secs;
    await api.castSeek(dev, secs).catch(() => undefined);
    this.emitMirror();
  }

  private async poll(): Promise<void> {
    const dev = this.device;
    if (!dev) return;
    let st: { state: string; position: number; duration: number };
    try {
      st = await api.castStatus(dev);
    } catch {
      return; // transient — next poll retries
    }
    if (this.device !== dev) return; // session changed mid-flight
    if (st.duration > 0) this.dur = st.duration;
    if (st.state === "playing") this.sawPlaying = true;
    if (st.state === "playing" || st.state === "paused") {
      this.playing = st.state === "playing";
      if (!this.scrubbing && Date.now() >= this.seekAt) {
        this.pos = st.position;
      }
    } else if (st.state === "idle" && this.sawPlaying) {
      // The receiver stopped on its own — the media ended, or
      // someone stopped it at the TV. End the session here and
      // hand playback back at the last known position.
      const at = this.pos;
      const wasPlaying = this.playing;
      const name = dev.name;
      this.teardown();
      if (!this.destroyed) {
        if (this.hooks.onNaturalEnd) {
          this.hooks.onNaturalEnd();
        } else {
          this.hooks.resumeLocal(at, wasPlaying);
          Toast.success(`Casting to ${name} ended — back on this device.`);
        }
      }
      return;
    }
    this.emitMirror();
  }
}
