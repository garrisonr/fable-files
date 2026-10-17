/**
 * WelcomeTour — the first-run welcome (Dickles, 2026-10-16: "we need
 * a welcome orientation… a beautiful splash screen the first time you
 * open.. kind of like an old video game. It pauses intentionally…
 * super easy ways to instantly kill the welcome. I don't like
 * welcome's that drag you along and make you click. I like the ones
 * that do beautiful presentations or videos and play by themselves.
 * Soothing… Could even have a soundtrack.").
 *
 * His law, as built:
 * - It plays by itself: a timed scene director, slow crossfades, no
 *   click-to-continue anywhere. The only buttons are exits (Skip),
 *   sound, and — while an account scene is being used — Continue.
 * - It dies instantly and permanently: Esc, the Skip pill, closing
 *   the window. The seen-flag is written when the show STARTS, so
 *   every exit (crash and unplug included) is final. Replay lives in
 *   the main menu, the About box, and Preferences → Appearance.
 * - State is dual-store like every durable pref in this app
 *   (7.1 QA: localStorage alone lost state across sessions): the
 *   backend ui-prefs sidecar is the truth, localStorage the
 *   pre-paint mirror.
 * - The score is procedural WebAudio (a console boot chime is
 *   synthesized, not shipped): zero bundled assets, and it can bow
 *   out the instant the show is killed. Patch constants live in
 *   SCORE_PATCH and are asserted by the timing harness.
 *
 * Design + full script: ~/workspace/fable-welcome/DESIGN.md.
 */
import { api } from "./api";
import { reducedMotion } from "./motion";
import { sectionGlyph } from "./sectionicons";
import { GameAccountsSection } from "./rommedia";
import { TmdbAccountSection } from "./mediares";
import { OpenSubtitlesAccountSection } from "./mediacomplete";

// ---------------------------------------------------------------- store

const SEEN_KEY = "ff-welcome-seen";
const MUTED_KEY = "ff-welcome-muted";

function lsRead(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function lsWrite(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    // fast path only — the sidecar below is the durable copy
  }
}

/** Merge-write into the backend ui-prefs sidecar (read-modify-write,
 * the shell's own pattern). `after` defers the write until a gate
 * promise settles — the first-run gate reads the sidecar BEFORE the
 * seen-flag may land in it, or the gate would read its own write. */
function sidecarMerge(
  patch: Record<string, unknown>,
  after?: Promise<unknown>,
): void {
  const write = (): void => {
    void api
      .getUiPrefs()
      .then((prefs) => api.setUiPrefs({ ...(prefs ?? {}), ...patch }))
      .catch(() => undefined);
  };
  if (after) {
    void after.then(write, write);
  } else {
    write();
  }
}

export const welcomeStore = {
  /** Pre-paint fast path: true only when localStorage is certain. */
  seenFast(): boolean {
    return lsRead(SEEN_KEY) === "1";
  },
  mutedFast(): boolean {
    return lsRead(MUTED_KEY) === "1";
  },
  /** The durable truth. Null = unreadable (treat as unseen). */
  async seenDurable(): Promise<boolean | null> {
    try {
      const prefs = await api.getUiPrefs();
      const v = prefs?.[SEEN_KEY];
      return typeof v === "boolean" ? v : null;
    } catch {
      return null;
    }
  },
  /** The show has started: this viewing counts, whatever happens
   * next. localStorage lands synchronously (a relaunch a second
   * later must not re-show); the sidecar write waits for `gate`. */
  markSeen(gate?: Promise<unknown>): void {
    lsWrite(SEEN_KEY, "1");
    sidecarMerge({ [SEEN_KEY]: true }, gate);
  },
  setMuted(muted: boolean): void {
    lsWrite(MUTED_KEY, muted ? "1" : "0");
    sidecarMerge({ [MUTED_KEY]: muted });
  },
};

// ---------------------------------------------------------------- score

/** The whole instrument, in one table — retuning the score is a
 * one-line change, and the harness asserts every bound here. */
export const SCORE_PATCH = {
  /** Master ceiling (linear). The fade-in takes fadeInSec to get
   * here; nothing ever starts loud. */
  masterCap: 0.18,
  fadeInSec: 5,
  chordSec: 8,
  slotSec: 1,
  lowpassHz: 900,
  padPeak: 0.035,
  padAttackSec: 2.5,
  padReleaseSec: 3.0,
  arpPeak: 0.045,
  arpDecaySec: 1.6,
  bassPeak: 0.05,
  bassDecaySec: 4.0,
} as const;

/** Composed arpeggio density: 9 of 16 one-second slots sound. */
export const ARP_PATTERN: readonly number[] = [
  1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 0, 1, 0, 1, 0, 0,
];
/** Degree walk through each chord's arp set; consecutive entries
 * always differ, so the same pitch never repeats back-to-back. */
const ARP_SEQ: readonly number[] = [0, 2, 1, 3, 2, 4, 3, 5, 4, 2];

interface PadVoice {
  /** Semitones above A2 (110 Hz). */
  n: number;
  type: "sine" | "triangle";
  detune?: number;
  gainScale?: number;
}

interface Chord {
  /** Semitones above A2 for the bass root. */
  bass: number;
  pad: PadVoice[];
  /** Semitones above A4 (440 Hz) — the chord's pentatonic subset. */
  arp: number[];
}

/** Amaj9 → F♯m7 → Dmaj9 → Eadd9, in A-major pentatonic country:
 * no scheduled pitch exists outside these sets. */
export const SCORE_CHORDS: readonly Chord[] = [
  {
    bass: 0,
    pad: [
      { n: 7, type: "sine" },
      { n: 12, type: "sine" },
      { n: 14, type: "sine" },
      { n: 16, type: "sine" },
      { n: 28, type: "triangle", detune: 4, gainScale: 0.5 },
    ],
    arp: [0, 2, 4, 7, 9, 12],
  },
  {
    bass: -3,
    pad: [
      { n: 9, type: "sine" },
      { n: 12, type: "sine" },
      { n: 16, type: "sine" },
      { n: 19, type: "sine" },
      { n: 28, type: "triangle", detune: 4, gainScale: 0.5 },
    ],
    arp: [-3, 0, 4, 7, 9, 12],
  },
  {
    bass: 5,
    pad: [
      { n: 12, type: "sine" },
      { n: 17, type: "sine" },
      { n: 19, type: "sine" },
      { n: 21, type: "sine" },
      { n: 29, type: "triangle", detune: 4, gainScale: 0.5 },
    ],
    arp: [0, 5, 7, 9, 12],
  },
  {
    bass: -5,
    pad: [
      { n: 7, type: "sine" },
      { n: 14, type: "sine" },
      { n: 19, type: "sine" },
      { n: 23, type: "sine" },
      { n: 31, type: "triangle", detune: 4, gainScale: 0.5 },
    ],
    arp: [2, 7, 9, 11],
  },
];

const A2 = 110;
const A4 = 440;
const fromA2 = (n: number): number => A2 * Math.pow(2, n / 12);
const fromA4 = (n: number): number => A4 * Math.pow(2, n / 12);

// Minimal structural surface of WebAudio the score uses — the real
// AudioContext satisfies it, and the harness's recording fake does
// too, so the shipped scheduler is what gets tested.
export interface AudioParamLike {
  value: number;
  setValueAtTime(v: number, t: number): unknown;
  linearRampToValueAtTime(v: number, t: number): unknown;
  exponentialRampToValueAtTime(v: number, t: number): unknown;
  cancelScheduledValues(t: number): unknown;
}
export interface OscLike {
  type: string;
  frequency: AudioParamLike;
  detune: AudioParamLike;
  connect(node: unknown): unknown;
  start(t?: number): void;
  stop(t?: number): void;
}
export interface GainLike {
  gain: AudioParamLike;
  connect(node: unknown): unknown;
}
export interface FilterLike {
  type: string;
  frequency: AudioParamLike;
  connect(node: unknown): unknown;
}
export interface CtxLike {
  currentTime: number;
  state: string;
  destination: unknown;
  createOscillator(): OscLike;
  createGain(): GainLike;
  createBiquadFilter(): FilterLike;
  createDynamicsCompressor(): { connect(node: unknown): unknown };
  resume(): Promise<void>;
  close(): Promise<void>;
}

function defaultCtxFactory(): CtxLike | null {
  try {
    const w = window as unknown as {
      AudioContext?: new () => CtxLike;
      webkitAudioContext?: new () => CtxLike;
    };
    const Ctor = w.AudioContext ?? w.webkitAudioContext;
    return Ctor ? new Ctor() : null;
  } catch {
    return null;
  }
}

export interface ClockFace {
  now(): number;
  setInterval(fn: () => void, ms: number): unknown;
  clearInterval(h: unknown): void;
  setTimeout(fn: () => void, ms: number): unknown;
  clearTimeout(h: unknown): void;
}

export const systemClock: ClockFace = {
  now: () => Date.now(),
  setInterval: (fn, ms) => window.setInterval(fn, ms),
  clearInterval: (h) => window.clearInterval(h as number),
  setTimeout: (fn, ms) => window.setTimeout(fn, ms),
  clearTimeout: (h) => window.clearTimeout(h as number),
};

/** The procedural score: pad + sparse arpeggio + soft root, composed
 * at runtime. Silent failure is a design feature — any audio error
 * leaves the show running in silence; sound never gates visuals. */
export class WelcomeScore {
  private ctx: CtxLike | null = null;
  private master: GainLike | null = null;
  private padBus: GainLike | null = null;
  private timer: unknown = null;
  private nextChordAt = 0;
  private nextSlotAt = 0;
  private absSlot = 0;
  private arpPos = 0;
  private startedAt = 0;
  private stopped = false;
  muted: boolean;

  constructor(
    private clock: ClockFace = systemClock,
    private createCtx: () => CtxLike | null = defaultCtxFactory,
    muted = false,
  ) {
    this.muted = muted;
  }

  /** True once a context exists and is actually running — the
   * sound toggle tells the truth about this, not about intent. */
  get sounding(): boolean {
    return !!this.ctx && this.ctx.state === "running";
  }

  start(): void {
    if (this.ctx || this.stopped) return;
    let ctx: CtxLike | null = null;
    try {
      ctx = this.createCtx();
    } catch {
      ctx = null;
    }
    if (!ctx) return;
    this.ctx = ctx;
    try {
      const comp = ctx.createDynamicsCompressor();
      comp.connect(ctx.destination);
      const master = ctx.createGain();
      master.connect(comp);
      master.gain.value = 0;
      if (!this.muted) {
        master.gain.linearRampToValueAtTime(
          SCORE_PATCH.masterCap,
          ctx.currentTime + SCORE_PATCH.fadeInSec,
        );
      }
      const filter = ctx.createBiquadFilter();
      filter.type = "lowpass";
      filter.frequency.value = SCORE_PATCH.lowpassHz;
      filter.connect(master);
      const padBus = ctx.createGain();
      padBus.connect(filter);
      this.master = master;
      this.padBus = padBus;
      this.startedAt = ctx.currentTime;
      this.nextChordAt = ctx.currentTime + 0.1;
      this.nextSlotAt = ctx.currentTime + 0.1;
      this.absSlot = 0;
      this.timer = this.clock.setInterval(() => this.schedule(), 250);
      this.schedule();
    } catch {
      this.ctx = null;
      this.master = null;
      this.padBus = null;
    }
  }

  private schedule(): void {
    const ctx = this.ctx;
    if (!ctx || this.stopped) return;
    try {
      const ahead = ctx.currentTime + 0.5;
      while (this.nextChordAt < ahead) {
        const idx =
          Math.floor(
            (this.nextChordAt - this.startedAt) / SCORE_PATCH.chordSec,
          ) % SCORE_CHORDS.length;
        this.scheduleChord(this.nextChordAt, idx);
        this.nextChordAt += SCORE_PATCH.chordSec;
      }
      while (this.nextSlotAt < ahead) {
        if (ARP_PATTERN[this.absSlot % ARP_PATTERN.length]) {
          this.scheduleArp(this.nextSlotAt, this.absSlot);
        }
        this.absSlot += 1;
        this.nextSlotAt += SCORE_PATCH.slotSec;
      }
    } catch {
      // a scheduling hiccup must never take the show down with it
    }
  }

  private scheduleChord(when: number, chordIdx: number): void {
    const ctx = this.ctx;
    const padBus = this.padBus;
    if (!ctx || !padBus) return;
    const chord = SCORE_CHORDS[chordIdx];
    const P = SCORE_PATCH;
    for (const voice of chord.pad) {
      const osc = ctx.createOscillator();
      osc.type = voice.type;
      osc.frequency.value = fromA2(voice.n);
      if (voice.detune) osc.detune.value = voice.detune;
      const g = ctx.createGain();
      const peak = P.padPeak * (voice.gainScale ?? 1);
      g.gain.setValueAtTime(0, when);
      g.gain.linearRampToValueAtTime(peak, when + P.padAttackSec);
      g.gain.setValueAtTime(peak, when + P.chordSec);
      g.gain.linearRampToValueAtTime(0, when + P.chordSec + P.padReleaseSec);
      osc.connect(g);
      g.connect(padBus);
      osc.start(when);
      osc.stop(when + P.chordSec + P.padReleaseSec + 0.1);
    }
    const bass = ctx.createOscillator();
    bass.type = "sine";
    bass.frequency.value = fromA2(chord.bass);
    const bg = ctx.createGain();
    bg.gain.setValueAtTime(0, when);
    bg.gain.linearRampToValueAtTime(P.bassPeak, when + 0.05);
    bg.gain.exponentialRampToValueAtTime(0.0001, when + P.bassDecaySec);
    bass.connect(bg);
    bg.connect(this.master);
    bass.start(when);
    bass.stop(when + P.bassDecaySec + 0.1);
  }

  private scheduleArp(when: number, absSlot: number): void {
    const ctx = this.ctx;
    if (!ctx || !this.master) return;
    const P = SCORE_PATCH;
    const chordIdx =
      Math.floor(absSlot / (P.chordSec / P.slotSec)) % SCORE_CHORDS.length;
    const set = SCORE_CHORDS[chordIdx].arp;
    const degree = set[ARP_SEQ[this.arpPos % ARP_SEQ.length] % set.length];
    this.arpPos += 1;
    const osc = ctx.createOscillator();
    osc.type = "sine";
    osc.frequency.value = fromA4(degree);
    const g = ctx.createGain();
    g.gain.setValueAtTime(0, when);
    g.gain.linearRampToValueAtTime(P.arpPeak, when + 0.01);
    g.gain.exponentialRampToValueAtTime(0.0001, when + P.arpDecaySec);
    osc.connect(g);
    g.connect(this.master);
    osc.start(when);
    osc.stop(when + P.arpDecaySec + 0.1);
  }

  /** WebKitGTK may hold the context suspended until a gesture. The
   * show never waits for this — the score simply joins at the first
   * interaction of any kind. */
  resumeIfSuspended(): void {
    if (this.ctx && this.ctx.state === "suspended") {
      void this.ctx.resume().catch(() => undefined);
    }
  }

  setMuted(muted: boolean): void {
    this.muted = muted;
    const ctx = this.ctx;
    const master = this.master;
    if (!ctx || !master) return;
    try {
      const t = ctx.currentTime;
      master.gain.cancelScheduledValues(t);
      master.gain.setValueAtTime(master.gain.value, t);
      master.gain.linearRampToValueAtTime(
        muted ? 0 : SCORE_PATCH.masterCap,
        t + 0.25,
      );
    } catch {
      // silence is an acceptable failure mode for a mute toggle
    }
  }

  stop(): void {
    if (this.stopped) return;
    this.stopped = true;
    if (this.timer !== null) {
      this.clock.clearInterval(this.timer);
      this.timer = null;
    }
    const ctx = this.ctx;
    const master = this.master;
    if (!ctx || !master) return;
    try {
      const t = ctx.currentTime;
      master.gain.cancelScheduledValues(t);
      master.gain.setValueAtTime(master.gain.value, t);
      master.gain.linearRampToValueAtTime(0, t + 0.4);
      this.clock.setTimeout(() => {
        void ctx.close().catch(() => undefined);
      }, 600);
    } catch {
      // the context is already gone; nothing to bow out of
    }
  }
}

// ---------------------------------------------------------------- scenes

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  e.className = className;
  if (text !== undefined) e.textContent = text;
  return e;
}

/** Tiny rich text: **bold** and *italic* spans, everything else is
 * textContent — copy can carry emphasis, never markup. */
function rich(parent: HTMLElement, text: string): void {
  const parts = text.split("**");
  parts.forEach((part, i) => {
    if (i % 2 === 1) {
      const b = document.createElement("strong");
      rich(b, part);
      parent.appendChild(b);
      return;
    }
    part.split("*").forEach((seg, j) => {
      if (j % 2 === 1) {
        const em = document.createElement("em");
        em.textContent = seg;
        parent.appendChild(em);
      } else if (seg) {
        parent.appendChild(document.createTextNode(seg));
      }
    });
  });
}

export interface SceneMeta {
  id: string;
  dwellMs: number;
  interactive: boolean;
}

interface SceneDef extends SceneMeta {
  build: (tour: WelcomeTour) => HTMLElement;
}

function textScene(
  meta: SceneMeta,
  eyebrow: string,
  title: string,
  paras: string[],
  glyph?: string,
): SceneDef {
  return {
    ...meta,
    build: () => {
      const s = el("section", "w-scene");
      const inner = el("div", "w-inner");
      if (glyph) {
        const g = sectionGlyph(glyph);
        g.classList.add("w-glyph");
        inner.appendChild(g);
      }
      inner.appendChild(el("p", "w-eyebrow w-line", eyebrow));
      inner.appendChild(el("h1", "w-title w-line", title));
      for (const p of paras) {
        const para = el("p", "w-body w-line");
        rich(para, p);
        inner.appendChild(para);
      }
      s.appendChild(inner);
      return s;
    },
  };
}

const SPLASH_MARK_SVG = `
<svg viewBox="0 0 240 150" class="w-mark-svg" aria-hidden="true">
  <rect class="mk-book" style="animation-delay:.15s" x="52" y="40" width="19" height="86" rx="3"/>
  <rect class="mk-book soft" style="animation-delay:.3s" x="73" y="48" width="16" height="78" rx="3"/>
  <rect class="mk-book" style="animation-delay:.45s" x="91" y="34" width="21" height="92" rx="3"/>
  <g class="mk-out"><rect x="115" y="38" width="20" height="88" rx="3"/></g>
  <rect class="mk-book soft" style="animation-delay:.6s" x="141" y="46" width="17" height="80" rx="3"/>
  <rect class="mk-book" style="animation-delay:.75s" x="160" y="38" width="20" height="88" rx="3"/>
  <rect class="mk-shelf" x="18" y="126" width="204" height="7" rx="3.5"/>
</svg>`;

function splashScene(): SceneDef {
  return {
    id: "splash",
    dwellMs: 9000,
    interactive: false,
    build: () => {
      const s = el("section", "w-scene w-splash");
      const inner = el("div", "w-inner w-splash-inner");
      const mark = el("div", "w-mark");
      mark.innerHTML = SPLASH_MARK_SVG;
      const word = el("div", "w-wordmark", "Fable Files");
      const sub = el(
        "p",
        "w-sub",
        "Sit back. This part plays itself.",
      );
      const hint = el("p", "w-esc-hint", "Esc — skip");
      inner.append(mark, word, sub, hint);
      s.appendChild(inner);
      return s;
    },
  };
}

interface AccountStep {
  /** Fallback name for the step chrome — a rendered provider
   * block's own heading wins when it has one. */
  name: string;
  mount: (host: HTMLElement) => void;
}

/** One login at a time (Dickles, 2026-10-17: "This part needs to
 * show each login one at a time with a skip button to go to the
 * next."). The account scenes STEP through their providers instead
 * of stacking them in one tall card — the stack overflowed the
 * card and clipped the second provider's copy mid-sentence.
 *
 * The sections are the very components Preferences → Metadata
 * Accounts mounts; the stepper only decides which provider block
 * is on stage. A block is a section's rendered provider element
 * (.rm-source / .mr-tmdb); a section that renders no such block
 * steps as one whole unit. A MutationObserver re-collects blocks
 * as sections load and re-render (save/forget rebuilds them), so
 * the stepper never forks the shared surface. */
class AccountStepper {
  private units: HTMLElement[] = [];
  private names: string[] = [];
  private idx = 0;
  private observer: MutationObserver | null = null;
  private queued = false;
  private disposed = false;

  constructor(
    private stepsEl: HTMLElement,
    private nameEl: HTMLElement,
    private countEl: HTMLElement,
    private pipsEl: HTMLElement,
  ) {}

  start(): void {
    this.collect();
    this.observer = new MutationObserver(() => this.schedule());
    this.observer.observe(this.stepsEl, { childList: true, subtree: true });
  }

  private schedule(): void {
    if (this.queued || this.disposed) return;
    this.queued = true;
    requestAnimationFrame(() => {
      this.queued = false;
      if (!this.disposed) this.collect();
    });
  }

  private collect(): void {
    const prev = this.units[this.idx];
    // Retire the previous unit set completely: while a section is
    // still loading, its unit is the whole mount; once rendered,
    // the unit becomes the provider block inside it. Without this
    // cleanup the stale mount keeps its w-unit-off and takes the
    // freshly collected block down with it (the invisible-second-
    // card bug, caught in the 2026-10-17 AFTER captures).
    for (const u of this.units) u.classList.remove("w-unit-off", "w-unit-in");
    const units: HTMLElement[] = [];
    const names: string[] = [];
    for (const m of [...this.stepsEl.children] as HTMLElement[]) {
      const blocks = m.querySelectorAll<HTMLElement>(".rm-source, .mr-tmdb");
      if (blocks.length > 0) {
        blocks.forEach((b) => {
          units.push(b);
          names.push(stepNameOf(b) ?? m.dataset.stepName ?? "Account");
        });
      } else {
        units.push(m);
        names.push(m.dataset.stepName ?? "Account");
      }
    }
    this.units = units;
    this.names = names;
    const keep = prev ? units.indexOf(prev) : -1;
    this.idx =
      keep >= 0 ? keep : Math.min(this.idx, Math.max(0, units.length - 1));
    this.apply(false);
  }

  /** Advance to the next card. False when the current card was
   * the last — the scene is complete. */
  next(): boolean {
    if (this.idx + 1 >= this.units.length) return false;
    this.idx += 1;
    this.apply(true);
    return true;
  }

  private apply(animate: boolean): void {
    this.units.forEach((u, i) => {
      const on = i === this.idx;
      u.classList.toggle("w-unit-off", !on);
      if (on && animate) {
        u.classList.remove("w-unit-in");
        void u.offsetWidth; // restart the entrance on the new card
        u.classList.add("w-unit-in");
      }
    });
    this.nameEl.textContent = this.names[this.idx] ?? "";
    this.countEl.textContent =
      this.units.length > 1 ? `${this.idx + 1} of ${this.units.length}` : "";
    this.pipsEl.innerHTML = "";
    if (this.units.length > 1) {
      this.units.forEach((_, i) => {
        const pip = document.createElement("span");
        pip.className = "w-pip" + (i === this.idx ? " on" : "");
        this.pipsEl.appendChild(pip);
      });
    }
    // A new card starts at its top, never mid-scroll.
    this.stepsEl.scrollTop = 0;
  }

  dispose(): void {
    this.disposed = true;
    this.observer?.disconnect();
    this.observer = null;
  }
}

function stepNameOf(block: HTMLElement): string | null {
  const h = block.querySelector(".rm-source-name, .mc-h, h4, h5");
  const t = h?.textContent?.trim();
  if (!t) return null;
  // "OpenSubtitles (subtitles)" steps under its plain name.
  return t.split(" (")[0];
}

function accountsScene(
  meta: SceneMeta,
  eyebrow: string,
  title: string,
  intro: string,
  note: string,
  steps: AccountStep[],
): SceneDef {
  return {
    ...meta,
    build: (tour) => {
      const s = el("section", "w-scene w-accounts-scene");
      const inner = el("div", "w-inner w-accounts-inner");
      inner.appendChild(el("p", "w-eyebrow w-line", eyebrow));
      inner.appendChild(el("h1", "w-title w-line", title));
      const p = el("p", "w-body w-line");
      rich(p, intro);
      inner.appendChild(p);
      const card = el("div", "w-card w-line");
      card.dataset.holdZone = "1";
      const stepsEl = el("div", "w-steps");
      for (const step of steps) {
        const host = el("div", "w-stepmount");
        host.dataset.stepName = step.name;
        stepsEl.appendChild(host);
        step.mount(host);
      }
      card.appendChild(stepsEl);
      const chrome = el("div", "w-stepper");
      const nameEl = el("span", "w-step-name");
      const countEl = el("span", "w-step-count");
      const pipsEl = el("span", "w-pips");
      const skip = el("button", "w-btn w-step-skip", "Skip ▸");
      skip.title = "Skip this login — go to the next one.";
      chrome.append(nameEl, countEl, pipsEl, skip);
      card.appendChild(chrome);
      inner.appendChild(card);
      inner.appendChild(el("p", "w-note w-line", note));
      s.appendChild(inner);
      const stepper = new AccountStepper(stepsEl, nameEl, countEl, pipsEl);
      skip.addEventListener("click", () => {
        if (!stepper.next()) {
          // The last card is skipped: the scene's work is done —
          // tell the tour (it advances like Continue does).
          card.dispatchEvent(
            new CustomEvent("w-steps-done", { bubbles: true }),
          );
        }
      });
      tour.registerStepper(stepper);
      stepper.start();
      return s;
    },
  };
}

const ACCOUNT_NOTE =
  "Anything you save here stays saved — the tour moving on never undoes it. Everything here also lives in Preferences → Metadata Accounts, whenever you're ready.";

function buildScenes(): SceneDef[] {
  return [
    splashScene(),
    textScene(
      { id: "philosophy", dwellMs: 19000, interactive: false },
      "THE IDEA",
      "Your files, understood.",
      [
        "Most file managers show you names. Fable Files does something ruder and kinder: it learns your library. It reads the tags in your music, matches your movies to their real titles, and fingerprints your game cartridges against the archives of record — then remembers all of it, so the next time you arrive, everything is already known.",
        "All of that memory lives here, on your machine. Nothing phones home. The library is yours; so is everything Fable knows about it.",
      ],
    ),
    textScene(
      { id: "hog", dwellMs: 19400, interactive: false },
      "FULL DISCLOSURE",
      "Yes, it's a hog. Here's the receipt.",
      [
        "A plain file browser lists names and goes home early. Fable stays up all night. It keeps a catalog of every file you've ever shown it — over a million entries on a working library — fingerprints media to identify it, builds previews before you ask, and keeps a media server's memory of what everything is.",
        "The weight is the memory. The memory is the point. Every calorie is spent knowing your library, so you don't have to.",
      ],
    ),
    textScene(
      { id: "files", dwellMs: 11400, interactive: false },
      "THE ROOMS · 1 OF 4",
      "First, it's a file manager.",
      [
        "Tabs, panes, Quick Look, and jobs that pause, resume, and survive a crash instead of dying in the night. The ordinary work, done properly — because the extraordinary stuff only matters if Tuesday's copying is boring.",
      ],
      "folder",
    ),
    textScene(
      { id: "roms", dwellMs: 23400, interactive: false },
      "THE ROOMS · 2 OF 4",
      "Games Home.",
      [
        "Your cartridges, identified by their fingerprints and shelved by system — every dump of a game folded into one card, bad dumps badged and never in charge. Your PC games live on the same shelves, imported from Playnite with their art and sizes intact.",
        "Press Play, and Fable dresses the right runner: the right emulator for a cartridge, Proton or Wine for a PC game — and when a game is Windows-only, it says so to your face. Fable fetches emulators on request; it never bundles them. And it never, ever fetches a BIOS. Those are yours to bring.",
      ],
      "roms",
    ),
    textScene(
      { id: "music", dwellMs: 9400, interactive: false },
      "THE ROOMS · 3 OF 4",
      "The music room.",
      [
        "Tags read, lyrics found, artwork completed — and a player that remembers where you left off: mid-chapter, mid-song, mid-thought. The room is small on purpose. It's for listening.",
      ],
      "music",
    ),
    textScene(
      { id: "smart", dwellMs: 10200, interactive: false },
      "THE ROOMS · 4 OF 4",
      "Shelves that keep themselves.",
      [
        "A smart folder is a question Fable re-asks forever: every movie from 1987, every photo of the dog, every game you haven't finished. Ask once. The shelf stays full by itself.",
      ],
      "books",
    ),
    textScene(
      { id: "accounts-intro", dwellMs: 16200, interactive: false },
      "INTRODUCTIONS",
      "A few friends worth knowing.",
      [
        "Fable can introduce your library to the people who keep the records — the movie database, the subtitle library, the game archives. Every introduction is optional. Every key stays in your system keyring, and Fable works fine if you skip them all.",
        "One promise, in writing: **Saved** means Fable is holding your key. **Connected** means they just said yes. Fable will never blur that line.",
      ],
    ),
    accountsScene(
      { id: "accounts-movies", dwellMs: 20000, interactive: true },
      "INTRODUCTIONS · MOVIES & TV",
      "For the movies.",
      "TMDB is how a file named like a license plate becomes *Border (2018)*. OpenSubtitles brings the words when the words are missing.",
      ACCOUNT_NOTE,
      [
        {
          name: "TMDB",
          mount: (host) => {
            void new TmdbAccountSection().mount(host).catch(() => undefined);
          },
        },
        {
          name: "OpenSubtitles",
          mount: (host) => {
            void new OpenSubtitlesAccountSection()
              .mount(host)
              .catch(() => undefined);
          },
        },
      ],
    ),
    accountsScene(
      { id: "accounts-games", dwellMs: 20000, interactive: true },
      "INTRODUCTIONS · GAMES",
      "For the games.",
      "ScreenScraper and EmuMovies keep the box art, the manuals, and the trailers. TheGamesDB and SteamGridDB fill the gaps. Connect what you have — the rest can wait for a rainy day.",
      ACCOUNT_NOTE,
      [
        {
          name: "Game accounts",
          mount: (host) => {
            void new GameAccountsSection().mount(host).catch(() => undefined);
          },
        },
      ],
    ),
    textScene(
      { id: "finale", dwellMs: 9400, interactive: false },
      "THAT'S THE HOUSE",
      "Welcome home.",
      [
        "This welcome never nags: it's gone unless you ask for it. If you ever want the tour again, it's waiting in the menu, the About box, and Preferences.",
        "Now — your library.",
      ],
    ),
  ];
}

/** Scene ids + dwells, exported for the harness's timing proof. */
export const WELCOME_SCENES: readonly SceneMeta[] = buildScenes().map(
  ({ id, dwellMs, interactive }) => ({ id, dwellMs, interactive }),
);

/** Idle time after the last interaction before an account scene
 * lets the show resume on its own. */
const IDLE_RESUME_MS = 20000;

// ---------------------------------------------------------------- tour

export interface WelcomeTourOpts {
  clock?: ClockFace;
  createCtx?: () => CtxLike | null;
  /** First-run gate: resolves true when the durable store says the
   * welcome was already seen. Checked at the splash's end. */
  gate?: Promise<boolean | null>;
  reducedMotion?: boolean;
  onDone?: (reason: string) => void;
}

let activeTour: WelcomeTour | null = null;

export class WelcomeTour {
  private clock: ClockFace;
  private scenes: SceneDef[];
  private sceneEls = new Map<number, HTMLElement>();
  private el: HTMLElement | null = null;
  private stage: HTMLElement | null = null;
  private dotsEl: HTMLElement | null = null;
  private fillEl: HTMLElement | null = null;
  private chipEl: HTMLElement | null = null;
  private continueBtn: HTMLButtonElement | null = null;
  private soundBtn: HTMLButtonElement | null = null;
  private score: WelcomeScore;
  private steppers: AccountStepper[] = [];
  private idx = -1;
  private tickHandle: unknown = null;
  private elapsedBase = 0;
  private stretchStart = 0;
  private held = false;
  private pointerIn = false;
  private focusIn = false;
  private lastTouch = -1;
  private holdDefeated = false;
  private chipText = "";
  private done = false;
  private gateSeen = false;
  private reduced: boolean;
  private onDone?: (reason: string) => void;
  private keyHandler = (e: KeyboardEvent): void => this.onKey(e);
  private gestureHandler = (): void => this.score.resumeIfSuspended();

  constructor(private opts: WelcomeTourOpts = {}) {
    this.clock = opts.clock ?? systemClock;
    this.scenes = buildScenes();
    this.reduced = opts.reducedMotion ?? reducedMotion();
    this.onDone = opts.onDone;
    this.score = new WelcomeScore(
      this.clock,
      opts.createCtx ?? defaultCtxFactory,
      welcomeStore.mutedFast(),
    );
    if (opts.gate) {
      void opts.gate.then((v) => {
        if (v === true) this.gateSeen = true;
      });
    }
  }

  get finished(): boolean {
    return this.done;
  }

  /** Account scenes register their stepper here so the tour can
   * dispose its observers when the show ends. */
  registerStepper(stepper: AccountStepper): void {
    this.steppers.push(stepper);
  }

  get currentScene(): number {
    return this.idx;
  }

  play(reason: "first-run" | "replay"): void {
    if (this.el) return;
    if (reason === "first-run") {
      // The seen-flag lands NOW: every exit from here — Esc, skip,
      // window close, crash, unplug — is permanent by design.
      welcomeStore.markSeen(this.opts.gate);
    }
    const overlay = el("div", "welcome-overlay");
    if (this.reduced) overlay.classList.add("reduced");
    overlay.setAttribute("role", "dialog");
    overlay.setAttribute("aria-label", "Welcome to Fable Files");
    overlay.tabIndex = -1;

    this.stage = el("div", "w-stage");
    overlay.appendChild(this.stage);

    const bottom = el("div", "w-bottom");
    this.dotsEl = el("div", "w-dots");
    for (let i = 0; i < this.scenes.length; i++) {
      this.dotsEl.appendChild(el("span", "w-dot"));
    }
    this.chipEl = el("p", "w-chip");
    this.chipEl.hidden = true;
    const actions = el("div", "w-actions");
    this.continueBtn = el("button", "w-btn w-continue", "Continue ▸");
    this.continueBtn.hidden = true;
    this.continueBtn.addEventListener("click", () => {
      // Continue means the human is done with this scene: the show
      // moves on at once (it does not make them sit out the dwell).
      this.holdDefeated = true;
      this.advance();
    });
    this.soundBtn = el("button", "w-btn w-sound");
    this.soundBtn.title =
      "Sound on or off. If the system holds the music at first, any key or click lets it start.";
    this.refreshSoundBtn();
    this.soundBtn.addEventListener("click", () => {
      const muted = !this.score.muted;
      this.score.setMuted(muted);
      welcomeStore.setMuted(muted);
      this.refreshSoundBtn();
    });
    const skip = el("button", "w-btn w-skip", "Skip the welcome ✕");
    skip.title = "End the welcome now (Esc works too) — it won't nag you again.";
    skip.addEventListener("click", () => this.endShow("skip"));
    actions.append(this.continueBtn, this.soundBtn, skip);
    bottom.append(this.dotsEl, this.chipEl, actions);
    overlay.appendChild(bottom);

    const progress = el("div", "w-progress");
    this.fillEl = el("div", "w-progress-fill");
    progress.appendChild(this.fillEl);
    overlay.appendChild(progress);

    document.body.appendChild(overlay);
    this.el = overlay;
    overlay.addEventListener("w-steps-done", () => {
      // The last account card was skipped: the scene is complete,
      // so the show moves on — same contract as Continue.
      if (this.done || !this.scenes[this.idx]?.interactive) return;
      this.holdDefeated = true;
      this.advance();
    });
    overlay.addEventListener("pointerdown", this.gestureHandler);
    window.addEventListener("keydown", this.keyHandler, true);
    overlay.focus();

    this.score.start();
    this.startScene(0);
    this.tickHandle = this.clock.setInterval(() => this.tick(), 100);
  }

  private refreshSoundBtn(): void {
    if (!this.soundBtn) return;
    this.soundBtn.textContent = this.score.muted ? "🔇 Muted" : "🔊 Sound";
  }

  private onKey(e: KeyboardEvent): void {
    this.score.resumeIfSuspended();
    if (e.key === "Escape" && !this.done) {
      e.preventDefault();
      e.stopPropagation();
      this.endShow("esc");
    }
  }

  private sceneEl(i: number): HTMLElement {
    let s = this.sceneEls.get(i);
    if (!s && this.stage) {
      s = this.scenes[i].build(this);
      this.sceneEls.set(i, s);
      this.stage.appendChild(s);
      // Lines settle in one by one when the scene takes the stage.
      s.querySelectorAll<HTMLElement>(".w-line").forEach((n, li) => {
        n.style.animationDelay = `${li * 130}ms`;
      });
      if (this.scenes[i].interactive) this.wireHold(s);
    }
    return s!;
  }

  private wireHold(scene: HTMLElement): void {
    const zone = scene.querySelector<HTMLElement>("[data-hold-zone]");
    if (!zone) return;
    const touch = (): void => {
      this.lastTouch = this.clock.now();
    };
    zone.addEventListener("pointerenter", () => {
      this.pointerIn = true;
      touch();
    });
    zone.addEventListener("pointerleave", () => {
      this.pointerIn = false;
      touch();
    });
    zone.addEventListener("focusin", () => {
      this.focusIn = true;
      touch();
    });
    zone.addEventListener("focusout", () => {
      this.focusIn = false;
      touch();
    });
    zone.addEventListener("input", touch);
    zone.addEventListener("click", touch);
  }

  private computeHeld(now: number): boolean {
    if (!this.scenes[this.idx]?.interactive || this.holdDefeated) {
      return false;
    }
    if (this.pointerIn || this.focusIn) return true;
    return this.lastTouch > 0 && now - this.lastTouch < IDLE_RESUME_MS;
  }

  private startScene(i: number): void {
    const prev = this.idx >= 0 ? this.sceneEls.get(this.idx) : undefined;
    this.idx = i;
    this.elapsedBase = 0;
    this.stretchStart = this.clock.now();
    this.held = false;
    this.pointerIn = false;
    this.focusIn = false;
    this.lastTouch = -1;
    this.holdDefeated = false;
    const s = this.sceneEl(i);
    s.classList.add("on");
    if (prev && prev !== s) prev.classList.remove("on");
    // Prebuild the next scene so account sections have fetched
    // their state by the time the show arrives.
    if (i + 1 < this.scenes.length) void this.sceneEl(i + 1);
    if (this.dotsEl) {
      [...this.dotsEl.children].forEach((d, di) => {
        (d as HTMLElement).classList.toggle("lit", di <= i);
      });
    }
    if (this.fillEl) this.fillEl.style.width = "0%";
    this.setChip("");
  }

  private setChip(text: string): void {
    if (!this.chipEl || text === this.chipText) return;
    this.chipText = text;
    this.chipEl.textContent = text;
    this.chipEl.hidden = text === "";
    if (this.continueBtn) {
      this.continueBtn.hidden = !(
        text !== "" && this.scenes[this.idx]?.interactive
      );
    }
  }

  private tick(): void {
    if (this.done || this.idx < 0) return;
    const now = this.clock.now();
    const scene = this.scenes[this.idx];
    const heldNow = this.computeHeld(now);
    if (heldNow && !this.held) {
      this.elapsedBase += now - this.stretchStart;
      this.held = true;
    } else if (!heldNow && this.held) {
      this.held = false;
      this.stretchStart = now;
    }
    if (this.held) {
      const idleFor =
        this.lastTouch > 0 && !this.pointerIn && !this.focusIn
          ? now - this.lastTouch
          : 0;
      this.setChip(
        idleFor > IDLE_RESUME_MS - 5000
          ? "Continuing shortly…"
          : "Take your time — the tour is holding.",
      );
      return;
    }
    this.setChip("");
    const elapsed = this.elapsedBase + (now - this.stretchStart);
    if (this.fillEl) {
      this.fillEl.style.width = `${Math.min(100, (elapsed / scene.dwellMs) * 100)}%`;
    }
    if (elapsed >= scene.dwellMs) this.advance();
  }

  private advance(): void {
    if (this.done) return;
    if (this.idx === 0 && this.gateSeen) {
      // The durable store says this user has seen the welcome
      // (localStorage had been wiped): bow out inside the splash.
      this.endShow("already-seen");
      return;
    }
    const active = document.activeElement as HTMLElement | null;
    if (active && this.el?.contains(active) && active !== this.el) {
      active.blur();
    }
    if (this.idx + 1 >= this.scenes.length) {
      this.endShow("finished");
      return;
    }
    this.startScene(this.idx + 1);
  }

  endShow(reason: string): void {
    if (this.done) return;
    this.done = true;
    if (this.tickHandle !== null) {
      this.clock.clearInterval(this.tickHandle);
      this.tickHandle = null;
    }
    window.removeEventListener("keydown", this.keyHandler, true);
    this.score.stop();
    for (const s of this.steppers) s.dispose();
    this.steppers = [];
    const overlay = this.el;
    if (activeTour === this) activeTour = null;
    if (overlay) {
      overlay.classList.add("bye");
      let fadeMs: number;
      if (reason === "finished" && !this.reduced) {
        // Natural end (Dickles, 2026-10-17): "the last message to
        // fade out and into your files" — the finale's lines bow
        // out first, one by one, then the room itself dissolves
        // into the live file view underneath. Only the natural
        // end earns this; Esc/Skip stay snappy below.
        overlay.classList.add("finale-bye");
        const scene = this.sceneEls.get(this.idx);
        scene
          ?.querySelectorAll<HTMLElement>(".w-line")
          .forEach((n, li) => {
            // Retire the entrance animation first — its fill
            // would hold opacity at 1 and fight the bow-out.
            n.style.animation = "none";
            n.style.transition =
              "opacity 500ms ease, transform 500ms ease";
            n.style.transitionDelay = `${li * 70}ms`;
            n.style.opacity = "0";
            n.style.transform = "translateY(-6px)";
          });
        overlay.style.transitionDuration = "1500ms";
        overlay.style.transitionDelay = "400ms";
        fadeMs = 400 + 1500;
      } else if (reason === "finished") {
        // Reduced motion: a plain opacity dissolve, nothing moves.
        overlay.style.transitionDuration = "600ms";
        fadeMs = 600;
      } else {
        overlay.style.transitionDuration = "180ms";
        fadeMs = 200;
      }
      const kill = (): void => overlay.remove();
      // Removal is cosmetic from here (.bye already released the
      // pointer) — the timeout is the guarantee, transitionend
      // just lets a healthy webview finish on the fade's own beat.
      overlay.addEventListener(
        "transitionend",
        (e) => {
          if (e.target === overlay && e.propertyName === "opacity") kill();
        },
        { once: true },
      );
      this.clock.setTimeout(kill, fadeMs + 60);
    }
    this.onDone?.(reason);
  }
}

/** Show the welcome if this is a first run. Returns the tour, or
 * null when the fast path already knows better. The shell keeps
 * booting underneath either way. */
export function maybeShowFirstRunWelcome(
  opts: WelcomeTourOpts = {},
): WelcomeTour | null {
  if (welcomeStore.seenFast()) return null;
  if (activeTour) return activeTour;
  const tour = new WelcomeTour({
    ...opts,
    gate: opts.gate ?? welcomeStore.seenDurable(),
  });
  activeTour = tour;
  tour.play("first-run");
  return tour;
}

/** On-demand replay (menu / About / Preferences). Never gated by
 * the seen-flag and never unsets it. */
export function playWelcomeTour(): WelcomeTour {
  if (activeTour && !activeTour.finished) activeTour.endShow("replaced");
  const tour = new WelcomeTour();
  activeTour = tour;
  tour.play("replay");
  return tour;
}
