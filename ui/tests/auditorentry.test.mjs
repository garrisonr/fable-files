// Red Pen (2026-10-18): the sidebar's File Auditor entry must tell
// the truth in every state — the label is derived from persisted
// run history + the open count, never hardcoded. Run via the gates
// script (tsc compiles src/auditorentry.ts to ui/.test-build first).
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  auditorEntryState,
  formatStamp,
} from "../.test-build/auditorentry.js";

const done = (over = {}) => ({
  status: "completed",
  target: "/mnt/media",
  started: "2026-10-15T10:33:07",
  finished: "2026-10-15T10:38:28",
  summary: { scannedFiles: 1200, flagsOpen: 5 },
  ...over,
});

const died = (over = {}) => ({
  status: "interrupted",
  target: "/x",
  started: "2026-10-17T03:00:00",
  finished: null,
  summary: {},
  ...over,
});

test("never run: the entry invites a first run, claims no findings", () => {
  const s = auditorEntryState([], 0);
  assert.equal(s.state, "first-run");
  assert.equal(s.label, "Run a file audit");
  assert.equal(s.badge, "");
  assert.match(s.title, /No audits yet/);
});

test("findings: 'Review findings' is earned — with the count as badge", () => {
  const s = auditorEntryState([done()], 3737);
  assert.equal(s.state, "findings");
  assert.equal(s.label, "Review findings");
  assert.equal(s.badge, "3737");
  assert.match(s.title, /3737 findings wait/);
  assert.match(s.title, /Oct 15, 2026, 10:38 AM/);
});

test("clean run: All clear, and the title owns the history", () => {
  const s = auditorEntryState(
    [done({ summary: { scannedFiles: 42, flagsOpen: 0 } })],
    0,
  );
  assert.equal(s.state, "all-clear");
  assert.equal(s.label, "All clear");
  assert.equal(s.badge, "");
  assert.match(s.title, /scanned 42 files, found nothing/);
});

test("settled run: findings existed, all handled — All clear, said plainly", () => {
  const s = auditorEntryState([done()], 0);
  assert.equal(s.state, "all-clear");
  assert.equal(s.label, "All clear");
  assert.match(s.title, /everything it found has been settled/);
});

test("running beats findings: an audit in flight owns the label", () => {
  const running = {
    status: "running",
    target: "/mnt/big",
    started: "2026-10-18T23:00:00",
    finished: null,
    summary: {},
  };
  const s = auditorEntryState([running, done()], 12);
  assert.equal(s.state, "running");
  assert.equal(s.label, "Auditing…");
  assert.equal(s.badge, "12");
  assert.match(s.title, /Auditing \/mnt\/big/);
});

test("an interrupted only-run reads as a first run with the truth attached", () => {
  const s = auditorEntryState([died()], 0);
  assert.equal(s.state, "first-run");
  assert.equal(s.label, "Run a file audit");
  assert.match(s.title, /didn't finish/);
});

test("findings from an interrupted run still surface for review", () => {
  const s = auditorEntryState([died()], 3);
  assert.equal(s.state, "findings");
  assert.equal(s.label, "Review findings");
  assert.equal(s.badge, "3");
  assert.match(s.title, /didn't finish/);
});

test("a newer completion buries an older interruption", () => {
  const fresh = done({
    started: "2026-10-18T09:00:00",
    finished: "2026-10-18T09:05:00",
  });
  const s = auditorEntryState([fresh, died()], 0);
  assert.equal(s.state, "all-clear");
  assert.doesNotMatch(s.title, /didn't finish/);
});

test("formatStamp speaks human, and passes garbage through untouched", () => {
  assert.equal(formatStamp("2026-10-15T10:38:28"), "Oct 15, 2026, 10:38 AM");
  assert.equal(formatStamp("2026-01-02T00:05:00"), "Jan 2, 2026, 12:05 AM");
  assert.equal(formatStamp("2026-12-31T12:00:00"), "Dec 31, 2026, 12:00 PM");
  assert.equal(formatStamp("not a stamp"), "not a stamp");
});
