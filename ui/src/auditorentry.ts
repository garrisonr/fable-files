/**
 * File Auditor — sidebar entry state (Red Pen, 2026-10-18).
 *
 * The sidebar's File Auditor row used to read "Review findings"
 * unconditionally — a claim that is only true after an audit has
 * run and left findings open. This module derives the entry's
 * label, badge and hover title from the auditor's REAL persisted
 * state (the run history in the catalog + the live open-flag
 * count), so the row tells the truth in every state:
 *
 *   running    — an audit is in flight right now
 *   findings   — open findings wait for a verdict
 *   all-clear  — an audit has run; nothing is waiting
 *   first-run  — no audit has ever completed
 *
 * Pure and import-free (house pattern, cf. roomnav.ts) so the node
 * test suite can compile and exercise it standalone. The structural
 * run shape below is satisfied by api.ts's AuditorRun.
 */

export interface AuditorRunSummaryLike {
  scannedFiles?: number;
  flagsOpen?: number;
}

export interface AuditorRunLike {
  status: string;
  target: string;
  started: string;
  finished: string | null;
  summary: AuditorRunSummaryLike;
}

export type AuditorEntryStateName =
  | "running"
  | "findings"
  | "all-clear"
  | "first-run";

export interface AuditorEntryState {
  state: AuditorEntryStateName;
  label: string;
  badge: string;
  title: string;
}

const MONTHS = [
  "Jan", "Feb", "Mar", "Apr", "May", "Jun",
  "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/** Format a catalog stamp ("2026-10-15T10:38:28", local time) the
 * way a person says it: "Oct 15, 2026, 10:38 AM". Anything that
 * doesn't parse comes back untouched — showing the raw stamp is
 * honest; inventing a prettier one would not be. */
export function formatStamp(stamp: string): string {
  const m = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})/.exec(stamp);
  if (!m) return stamp;
  const [, y, mo, d, hh, mm] = m;
  const month = MONTHS[Number(mo) - 1];
  if (!month) return stamp;
  let hour = Number(hh);
  const ampm = hour >= 12 ? "PM" : "AM";
  hour = hour % 12 === 0 ? 12 : hour % 12;
  return `${month} ${Number(d)}, ${y}, ${hour}:${mm} ${ampm}`;
}

function plural(n: number, one: string, many: string): string {
  return n === 1 ? one : many;
}

/** Derive the sidebar entry from persisted state. `runs` is the
 * catalog's run history, newest first (recent_auditor_runs order);
 * `openCount` is the live number of open flags. */
export function auditorEntryState(
  runs: AuditorRunLike[],
  openCount: number,
): AuditorEntryState {
  const badge = openCount > 0 ? String(openCount) : "";
  const running = runs.find((r) => r.status === "running");
  const lastDone = runs.find((r) => r.status === "completed");
  const lastDied = runs.find((r) => r.status === "interrupted");
  // An interrupted run only colors the story when it is the most
  // recent thing that happened (newer than the last completion).
  const diedLatest =
    lastDied && (!lastDone || lastDied.started > lastDone.started)
      ? lastDied
      : undefined;

  if (running) {
    return {
      state: "running",
      label: "Auditing…",
      badge,
      title:
        `Auditing ${running.target} — started ${formatStamp(running.started)}.` +
        (openCount > 0
          ? ` ${openCount} ${plural(openCount, "finding", "findings")} already open.`
          : " Findings land here as they're found."),
    };
  }

  if (openCount > 0) {
    const when = lastDone
      ? ` · Last run ${formatStamp(lastDone.finished ?? lastDone.started)}`
      : diedLatest
        ? ` · From an audit that didn't finish (${formatStamp(diedLatest.started)})`
        : "";
    const diedNote =
      lastDone && diedLatest
        ? ` The latest audit didn't finish (${formatStamp(diedLatest.started)}).`
        : "";
    return {
      state: "findings",
      label: "Review findings",
      badge,
      title: `${openCount} ${plural(openCount, "finding waits", "findings wait")} for your verdict${when}.${diedNote}`,
    };
  }

  if (lastDone) {
    const found = lastDone.summary?.flagsOpen ?? 0;
    const when = formatStamp(lastDone.finished ?? lastDone.started);
    const body =
      found > 0
        ? `Last run ${when} · everything it found has been settled — nothing waiting.`
        : `Last run ${when} · scanned ${lastDone.summary?.scannedFiles ?? 0} files, found nothing.`;
    return {
      state: "all-clear",
      label: "All clear",
      badge,
      title: diedLatest
        ? `${body} The latest audit didn't finish (${formatStamp(diedLatest.started)}).`
        : body,
    };
  }

  if (diedLatest) {
    return {
      state: "first-run",
      label: "Run a file audit",
      badge,
      title: `The last audit didn't finish (${formatStamp(diedLatest.started)}) — run it again. The Auditor reads your library and reports; it never changes anything on its own.`,
    };
  }

  return {
    state: "first-run",
    label: "Run a file audit",
    badge,
    title:
      "No audits yet. Audit folders and drives for typos, pattern breaks, corruption, duplicates, junk and drift — the Auditor reports; it never changes anything on its own.",
  };
}
