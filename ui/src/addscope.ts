/**
 * Add-location safety gate: the pure sheet logic (Hyperbase
 * Solution #3, 2026-10-16).
 *
 * Everything in this module is a function of the census the backend
 * walked — no DOM, no invokes — so the harness can prove the pick
 * defaults and the estimate math against the real Hyperbase census
 * numbers. The DOM sheet lives in addlocation.ts and renders what
 * these functions decide.
 *
 * The wire shapes mirror the Rust structs field-for-field
 * (ufm_search::Census / CensusReport in commands.rs /
 * locscope::LocationScope), camelCase per house serde convention.
 */

export interface TopEntryWire {
  /** Directory name; "" is the synthetic root-files bucket. */
  name: string;
  files: number;
  dirs: number;
  bytes: number;
  candidateFiles: number;
  candidateBytes: number;
}

export interface CensusWire {
  files: number;
  dirs: number;
  bytes: number;
  candidateFiles: number;
  candidateBytes: number;
  top: TopEntryWire[];
  errors: number;
  elapsedMs: number;
}

export interface CensusReport {
  census: CensusWire;
  overThreshold: boolean;
  threshold: number;
}

export interface LocationScopeWire {
  romFocus: boolean;
  /** Picked top-level directory names; null = the whole tree. */
  includeDirs: string[] | null;
  includeRootFiles: boolean;
}

/** Mirror of ufm_search::GATE_THRESHOLD_FILES (the backend is the
 * authority — its `overThreshold` verdict is what opens the sheet;
 * this constant exists so the defaults can speak of the line). */
export const GATE_THRESHOLD_FILES = 50_000;

/** A single top-level tree this large is never a casual add: it
 * starts UNchecked whatever its contents, with its count on show. */
export const HUGE_TREE_FILES = 250_000;

/** Below this share of candidate files, a gate-sized tree reads as
 * program/asset bulk rather than a game library. */
export const LOW_CANDIDATE_SHARE = 0.005;

/** Sequential read rate for the identify estimate — mirror of
 * ufm_search::census::HASH_BYTES_PER_SEC (USB spinning disk). */
export const HASH_BYTES_PER_SEC = 120_000_000;

/** Catalog items one census entry contributes, given ROM focus. */
export function entryItems(e: TopEntryWire, romFocus: boolean): number {
  return romFocus ? e.candidateFiles + e.dirs : e.files + e.dirs;
}

export interface PickDefaults {
  /** Top-level directory names that start checked. */
  picked: Set<string>;
  /** The root-files bucket starts checked. */
  rootFiles: boolean;
  /** name -> why it starts unchecked ("" key = root files). */
  flagged: Map<string, string>;
}

/**
 * Default picks from the census classes: media/ROM trees on; giant
 * app/game-asset trees off and flagged with the reason. A tree is
 * flagged when it is HUGE_TREE_FILES or bigger (bulk is bulk,
 * whatever it holds), or when it alone exceeds the gate threshold
 * while carrying almost no games (the Hyperbase `games/` shape:
 * 1.7M files of program assets). Everything is overridable in the
 * sheet — a default is a suggestion with its reasoning printed.
 */
export function defaultPicks(census: CensusWire): PickDefaults {
  const picked = new Set<string>();
  const flagged = new Map<string, string>();
  let rootFiles = true;
  for (const e of census.top) {
    const share = e.files > 0 ? e.candidateFiles / e.files : 0;
    let why: string | null = null;
    if (e.files >= HUGE_TREE_FILES) {
      why = "very large — check it only if you mean it";
    } else if (e.files > GATE_THRESHOLD_FILES && share < LOW_CANDIDATE_SHARE) {
      why = "mostly program and asset files — very few games";
    }
    if (e.name === "") {
      rootFiles = why === null;
      if (why) flagged.set("", why);
    } else if (why === null) {
      picked.add(e.name);
    } else {
      flagged.set(e.name, why);
    }
  }
  return { picked, rootFiles, flagged };
}

export interface SelectionStats {
  /** Catalog items the current selection would add. */
  items: number;
  /** Candidate bytes inside the selection (what identify reads). */
  candidateBytes: number;
  /** All bytes inside the selection. */
  bytes: number;
}

/** Sum the selection: picked top-level dirs + the root-files
 * bucket when its switch is on, under the ROM-focus setting. */
export function selectionStats(
  census: CensusWire,
  picked: Set<string>,
  rootFiles: boolean,
  romFocus: boolean,
): SelectionStats {
  let items = 0;
  let candidateBytes = 0;
  let bytes = 0;
  for (const e of census.top) {
    const on = e.name === "" ? rootFiles : picked.has(e.name);
    if (!on) continue;
    items += entryItems(e, romFocus);
    candidateBytes += e.candidateBytes;
    bytes += romFocus ? e.candidateBytes : e.bytes;
  }
  return { items, candidateBytes, bytes };
}

/** Estimated seconds for the identify (hash) pass over the given
 * candidate bytes — mirror of the Rust estimate. */
export function estIdentifySecs(candidateBytes: number): number {
  return Math.ceil(candidateBytes / HASH_BYTES_PER_SEC);
}

/** Estimated seconds for the index walk of a selection: the census
 * walk measured this tree; indexing repeats the walk and writes a
 * catalog row per item, so 2x the measured time scaled by the
 * selected fraction. Mirror of Census::est_index_secs. */
export function estIndexSecs(census: CensusWire, selectedItems: number): number {
  const total = census.files + census.dirs;
  if (total === 0 || census.elapsedMs === 0) return 0;
  return Math.ceil((census.elapsedMs / 1000) * 2 * (selectedItems / total));
}

/** Build the scope the sheet confirms. When every top-level
 * directory is picked AND root files ride along, the pick is the
 * whole tree: send null (no pick) so the stored scope says what it
 * means. */
export function buildScope(
  census: CensusWire,
  picked: Set<string>,
  rootFiles: boolean,
  romFocus: boolean,
): LocationScopeWire {
  const allDirs = census.top.filter((e) => e.name !== "").map((e) => e.name);
  const wholeTree =
    rootFiles && allDirs.every((n) => picked.has(n));
  return {
    romFocus,
    includeDirs: wholeTree ? null : [...picked],
    includeRootFiles: rootFiles,
  };
}

/**
 * Re-index partition — the Safety Gate amendment (Dickles,
 * 2026-10-16): Re-index Library queues non-volume locations
 * immediately, but a VOLUME must pass the census gate first, so
 * an over-threshold volume is never queued unscoped from that
 * door either. Pure so the harness can pin the split.
 */
export function partitionReindex<T extends { provider: string }>(
  locs: readonly T[],
): { immediate: T[]; gated: T[] } {
  const immediate: T[] = [];
  const gated: T[] = [];
  for (const l of locs) {
    (l.provider === "volume" ? gated : immediate).push(l);
  }
  return { immediate, gated };
}

/** "about 4 minutes" / "about 40 seconds" / "under a minute" —
 * the sheet speaks in round human time; these are estimates and
 * are labeled as such where they appear. */
export function humanDuration(secs: number): string {
  if (secs < 50) return "under a minute";
  if (secs < 90) return "about a minute";
  if (secs < 3600) return `about ${Math.round(secs / 60)} minutes`;
  const h = Math.floor(secs / 3600);
  const m = Math.round((secs % 3600) / 60);
  return m > 0 ? `about ${h} h ${m} min` : `about ${h} hours`;
}
