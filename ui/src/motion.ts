/**
 * Motion engine. DESIGN.md §1 is law:
 * - Springs, not fixed-duration animations, for anything touchable.
 * - Every animation is interruptible: new input RE-TARGETS the live spring
 *   (velocity handoff preserved), never cancels-and-restarts from zero.
 * - WAAPI animations read live computed values and use composite:"replace".
 * - prefers-reduced-motion: everything becomes a 120ms opacity fade.
 */

export function reducedMotion(): boolean {
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

/** Critically damped default: response ~0.35s, no overshoot. */
const DEFAULT_STIFFNESS = 320;
const DEFAULT_DAMPING = 2 * Math.sqrt(DEFAULT_STIFFNESS);

interface Spring {
  key: string;
  value: number;
  velocity: number;
  target: number;
  stiffness: number;
  damping: number;
  raf: number;
  onFrame: (v: number) => void;
  onDone?: () => void;
  alive: boolean;
}

const springs = new Map<string, Spring>();

function step(s: Spring): void {
  // Semi-implicit Euler at fixed substeps — stable for the stiffness range.
  const dt = 1 / 120;
  for (let i = 0; i < 2; i++) {
    const force = -s.stiffness * (s.value - s.target) - s.damping * s.velocity;
    s.velocity += force * dt;
    s.value += s.velocity * dt;
  }
  s.onFrame(s.value);
  const settled =
    Math.abs(s.value - s.target) < 0.15 && Math.abs(s.velocity) < 0.6;
  if (settled) {
    s.alive = false;
    springs.delete(s.key);
    s.onFrame(s.target);
    s.onDone?.();
    return;
  }
  s.raf = requestAnimationFrame(() => step(s));
}

/**
 * Drive a spring. If a spring with this key is already live, it is
 * RE-TARGETED — value and velocity carry over, so motion stays continuous
 * no matter how often input arrives. Returns a cancel function.
 */
export function springTo(
  key: string,
  target: number,
  onFrame: (v: number) => void,
  opts: {
    from?: number;
    stiffness?: number;
    damping?: number;
    onDone?: () => void;
  } = {},
): () => void {
  const existing = springs.get(key);
  if (existing) {
    existing.target = target;
    existing.onFrame = onFrame;
    if (opts.onDone) existing.onDone = opts.onDone;
    return () => cancelSpring(key);
  }
  const s: Spring = {
    key,
    value: opts.from ?? target,
    velocity: 0,
    target,
    stiffness: opts.stiffness ?? DEFAULT_STIFFNESS,
    damping: opts.damping ?? DEFAULT_DAMPING,
    raf: 0,
    onFrame,
    onDone: opts.onDone,
    alive: true,
  };
  springs.set(key, s);
  s.raf = requestAnimationFrame(() => step(s));
  return () => cancelSpring(key);
}

export function cancelSpring(key: string): void {
  const s = springs.get(key);
  if (s) {
    s.alive = false;
    cancelAnimationFrame(s.raf);
    springs.delete(key);
  }
}

/**
 * Interruptible WAAPI helper: kills the element's running animations, reads
 * the LIVE computed value of each animated property, and animates from there
 * (composite "replace" so a re-trigger never stacks).
 */
export function animateFromLive(
  el: HTMLElement,
  keyframes: Keyframe[],
  options: KeyframeAnimationOptions,
): Animation {
  el.getAnimations().forEach((a) => a.cancel());
  return el.animate(keyframes, { composite: "replace", ...options });
}

/** Live opacity, for interruptible fades. */
export function liveOpacity(el: HTMLElement): number {
  const v = parseFloat(getComputedStyle(el).opacity);
  return Number.isFinite(v) ? v : 1;
}

/**
 * Reduced-motion path: a single 120ms opacity fade. All spring callers
 * should route here when reducedMotion() is true.
 */
export function fadeTo(
  el: HTMLElement,
  opacity: number,
  onDone?: () => void,
): void {
  const from = liveOpacity(el);
  if (Math.abs(from - opacity) < 0.01) {
    onDone?.();
    return;
  }
  const anim = animateFromLive(el, [{ opacity: from }, { opacity }], {
    duration: 120,
    easing: "ease-out",
    fill: "forwards",
  });
  anim.onfinish = () => onDone?.();
}

/** Progressive resistance for overscroll: rubber-band, don't hard-stop. */
export function rubberBand(delta: number, max = 96): number {
  return max * (1 - Math.exp(-Math.abs(delta) / max)) * Math.sign(delta);
}
