/**
 * File Auditor — the review surface (Dickles, 2026-10-15
 * 06:45–06:57). A special section, opened from the sidebar: run an
 * audit, read the summary of alerts, work the checklist.
 *
 * The two lanes are visible in the UI itself: CERTAIN findings are
 * labeled "Fact" and state what was verified; UNCERTAIN ones are
 * labeled "Question" and are ASKED, never asserted — with the
 * four-label answer grammar ("Just this one / Fix All Similar
 * (Irreversible) / Skip This One Once / Skip This One Forever").
 * Nothing here changes a file without
 * a click, and every fix runs as a journaled job the Transfers
 * panel can undo. Dismissals are remembered forever and can be
 * reviewed and reset at the bottom of the page.
 */
import {
  api,
  type AuditorFlag,
  type AuditorRule,
  type AuditorRun,
  type AuditorSettings,
  type AuditorDismissal,
} from "./api";
import { Toast, confirmDialog, errText } from "./toast";
import { ContextMenu } from "./contextmenu";

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

const FAMILY_ORDER: Array<[string, string]> = [
  ["typo", "Possible typos"],
  ["pattern", "Pattern breaks"],
  ["corrupt", "Corrupt & unreadable"],
  ["compat", "Windows / macOS compatibility"],
  ["space", "Space savers"],
  ["junk", "Junk & promo files"],
  ["clutter", "Clutter"],
  ["style", "Naming style drift"],
  ["editions", "Duplicate editions"],
  ["cold", "Cold storage"],
];

const FAMILY_BLURBS: Record<string, string> = {
  typo: "Names that are mostly real words with one word that looks misspelled. Gibberish names (hashes, IDs, scene strings) are left alone on purpose.",
  pattern: "Numbered series in a folder (XY1, XY2…) — outliers that break the shape, and numbers missing from the run.",
  corrupt: "Empty files, files whose contents don't match their extension, unreadable media, broken links.",
  compat: "OPT-IN. Names Windows or macOS would reject or mangle: illegal characters, reserved names like CON, trailing dots, and files that differ only by letter case.",
  space: "Content-verified duplicates (hashed, not guessed) and conversions — lossless ones labeled lossless, re-encodes labeled as re-encodes with estimated savings.",
  junk: "Download-site signatures (YTS ad images, DoNotMirror, “Downloaded from…” notices) and program files hiding among videos.",
  clutter: "Empty folders, stale partial downloads, OS junk (.DS_Store, Thumbs.db), orphaned subtitles/art, “Copy of” chains, archive + extracted-folder pairs, old installers.",
  style: "Folder-relative only: a name is flagged only when it breaks THIS folder's own dominant habit. A folder that's consistently all-caps gets no flags — its style is its style.",
  editions: "The same film in several copies or editions. A consolidation note, never an accusation.",
  cold: "Big files untouched for years — cold-storage candidates. Suggest-only; nothing moves without you.",
};

/** Plain-language names for ask-lane classes (the rules list). */
export function describeClass(classKey: string): string {
  if (classKey.startsWith("typo:")) {
    return `the word “${classKey.slice(5)}” spelled that way`;
  }
  const known: Record<string, string> = {
    "junk:exe-in-video-folder": "program files among videos",
    "junk:url": "website shortcuts (.url)",
    "clutter:copy-chain": "“Copy of” / (1) duplicate names",
    "clutter:archive-pair": "an archive next to its extracted folder",
    "clutter:stale-installer": "old installer images",
    "style:case": "names breaking their folder's case style",
    "style:separator": "names breaking their folder's separator style",
    "space:convert-hevc": "H.264 → H.265 re-encode suggestions",
    "cold:untouched": "cold-storage suggestions",
  };
  if (known[classKey]) return known[classKey];
  if (classKey.startsWith("pattern:outlier:")) {
    return `series outliers (${classKey.slice(16)})`;
  }
  if (classKey.startsWith("editions:")) {
    return `duplicate editions of “${classKey.slice(9)}”`;
  }
  return classKey;
}

const WEEKDAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

export interface AuditorPanelHooks {
  pickFolder: () => Promise<string | null>;
  /** Fired when the open-flag count may have changed (the
   * sidebar badge refreshes). */
  refreshBadge?: () => void;
}

export class AuditorPanel {
  private overlay: HTMLElement | null = null;
  private body: HTMLElement | null = null;
  private hooks: AuditorPanelHooks | null = null;
  private ctxMenu = new ContextMenu();
  private settings: AuditorSettings | null = null;
  private runs: AuditorRun[] = [];
  private flags: AuditorFlag[] = [];
  private dismissals: AuditorDismissal[] = [];
  private rules: AuditorRule[] = [];
  private words: string[] = [];
  private target = "";

  open(hooks: AuditorPanelHooks): void {
    this.close();
    this.hooks = hooks;
    this.target = localStorage.getItem("ff-auditor-target") ?? "";
    const overlay = el("div", "props-overlay");
    overlay.addEventListener("mousedown", (e) => {
      if (e.target === overlay) this.close();
    });
    const dialog = el("div", "props-dialog au-dialog");
    dialog.setAttribute("role", "dialog");
    dialog.setAttribute("aria-label", "File Auditor");
    const head = el("div", "props-head");
    head.appendChild(el("h3", "props-title", "File Auditor"));
    const closeBtn = el("button", "icon-btn props-close", "✕");
    closeBtn.setAttribute("aria-label", "Close File Auditor");
    closeBtn.addEventListener("click", () => this.close());
    head.appendChild(closeBtn);
    dialog.appendChild(head);
    const intro = el(
      "p",
      "prefs-note",
      "The Auditor reads your library and reports — it never changes anything on its own. Findings marked Fact were verified (a signature, a content hash, a rule with no exceptions). Findings marked Question are judgment calls, and they're asked, not asserted: answer once and the answer becomes a standing rule. Every fix you confirm runs as a job in Transfers, where it can be undone.",
    );
    dialog.appendChild(intro);
    this.body = el("div", "au-body");
    this.body.appendChild(el("p", "prefs-note", "Loading…"));
    dialog.appendChild(this.body);
    overlay.appendChild(dialog);
    document.body.appendChild(overlay);
    this.overlay = overlay;
    void this.reload();
  }

  close(): void {
    this.overlay?.remove();
    this.overlay = null;
    this.body = null;
    this.hooks?.refreshBadge?.();
  }

  private async reload(): Promise<void> {
    try {
      const [settings, runs, flags, dismissals, rules, words] =
        await Promise.all([
          api.getAuditorSettings(),
          api.auditorListRuns(),
          api.auditorListFlags(null),
          api.auditorListDismissals(),
          api.auditorListRules(),
          api.auditorListUserWords(),
        ]);
      this.settings = settings;
      this.runs = runs;
      this.flags = flags;
      this.dismissals = dismissals;
      this.rules = rules;
      this.words = words;
      this.render();
    } catch (err) {
      if (this.body) {
        this.body.innerHTML = "";
        this.body.appendChild(
          el("p", "prefs-note", `The Auditor could not load: ${errText(err)}`),
        );
      }
    }
  }

  private async saveSettings(): Promise<void> {
    if (!this.settings) return;
    try {
      this.settings = await api.saveAuditorSettings(this.settings);
      this.render();
    } catch (err) {
      Toast.error(`Could not save: ${errText(err)}`);
    }
  }

  private render(): void {
    const body = this.body;
    const s = this.settings;
    if (!body || !s) return;
    body.innerHTML = "";
    body.appendChild(this.renderRunNow());
    body.appendChild(this.renderLatestSummary());
    body.appendChild(this.renderChecklist());
    body.appendChild(this.renderFamilies(s));
    body.appendChild(this.renderSchedules(s));
    body.appendChild(this.renderMemory());
  }

  // ---------------------------------------------------------------
  // Audit now + latest summary
  // ---------------------------------------------------------------

  private renderRunNow(): HTMLElement {
    const block = el("div", "mr-tmdb");
    block.appendChild(el("h4", "mc-h", "Audit now"));
    block.appendChild(
      el(
        "p",
        "prefs-note",
        "Point the Auditor at a folder or a whole drive. A big drive takes a while — the audit reads file headers and hashes duplicates — and the results land below as a checklist when it's done. Scheduled audits (below) run on the same background agent as Media Completion.",
      ),
    );
    const row = el("div", "mr-keyrow");
    const input = document.createElement("input");
    input.className = "mr-in au-target";
    input.placeholder = "Folder or drive to audit";
    input.value = this.target;
    input.addEventListener("input", () => {
      this.target = input.value;
    });
    const browse = el("button", "pill-btn", "Choose folder…");
    browse.addEventListener("click", async () => {
      const picked = await this.hooks?.pickFolder();
      if (picked) {
        this.target = picked;
        input.value = picked;
      }
    });
    const go = el("button", "pill-btn primary", "Audit now");
    go.addEventListener("click", () => void this.runAuditNow(go));
    row.append(input, browse, go);
    block.appendChild(row);
    return block;
  }

  private renderLatestSummary(): HTMLElement {
    const block = el("div", "mr-tmdb");
    block.appendChild(el("h4", "mc-h", "Summary of alerts"));
    const run = this.runs[0];
    if (!run) {
      block.appendChild(
        el("p", "prefs-note", "No audits yet — run one above, or schedule one below."),
      );
      return block;
    }
    const sm = run.summary;
    const head = el(
      "p",
      "prefs-note",
      `${run.scheduleName} · ${run.target} · ${run.started}${run.wasLate ? " · RAN LATE (machine was off)" : ""} · ${run.status}`,
    );
    block.appendChild(head);
    block.appendChild(
      el(
        "p",
        "prefs-note",
        `Scanned ${sm.scannedFiles ?? 0} files in ${sm.scannedDirs ?? 0} folders · ${sm.flagsOpen ?? 0} open finding(s) (${sm.flagsNew ?? 0} new this run, ${sm.flagsCarried ?? 0} carried over, ${sm.flagsSuperseded ?? 0} no longer true).`,
      ),
    );
    const fams = sm.families ?? {};
    const chips = FAMILY_ORDER.filter(([k]) => (fams[k] ?? 0) > 0).map(
      ([k, label]) => `${label}: ${fams[k]}`,
    );
    if (chips.length) {
      block.appendChild(el("p", "prefs-note", chips.join(" · ")));
    }
    for (const note of sm.notes ?? []) {
      block.appendChild(el("p", "mrx-note", note));
    }
    if (this.runs.length > 1) {
      const hist = el("details", "au-history");
      const sum = el("summary", "", `Earlier audits (${this.runs.length - 1})`);
      hist.appendChild(sum);
      for (const r of this.runs.slice(1)) {
        hist.appendChild(
          el(
            "p",
            "prefs-note",
            `${r.started} · ${r.scheduleName} · ${r.target} · ${r.summary.flagsOpen ?? 0} open`,
          ),
        );
      }
      block.appendChild(hist);
    }
    return block;
  }

  // ---------------------------------------------------------------
  // The checklist
  // ---------------------------------------------------------------

  private async runAuditNow(btn?: HTMLButtonElement): Promise<void> {
    const target = this.target.trim();
    if (!target) {
      Toast.error("Choose a folder or drive to audit first.");
      return;
    }
    localStorage.setItem("ff-auditor-target", target);
    if (btn) {
      btn.textContent = "Auditing… (large drives take a while)";
      btn.toggleAttribute("disabled", true);
    }
    try {
      const run = await api.auditorRunNow(target, null);
      const sm = run.summary;
      Toast.success(
        `Audit finished: ${sm.flagsOpen ?? 0} open finding(s) — ${sm.flagsNew ?? 0} new, ${sm.flagsCarried ?? 0} still open, ${sm.flagsSuperseded ?? 0} cleared up.`,
        9000,
      );
      await this.reload();
      this.hooks?.refreshBadge?.();
    } catch (err) {
      Toast.error(`Audit failed: ${errText(err)}`);
      if (btn) {
        btn.textContent = "Audit now";
        btn.toggleAttribute("disabled", false);
      }
    }
  }

  /** Clear findings (Auditor item 13): right-click the findings
   * surface → warning FIRST → clear on confirm only. Findings
   * only — skips and standing rules survive; the journal of the
   * clear is a run-history row written by the command. */
  private async clearFindings(): Promise<void> {
    const ok = await confirmDialog(
      "Clear findings?",
      "This clears the current findings list. Nothing is renamed, moved, or deleted — only the list is emptied, and a fresh audit rebuilds it. What you taught the Auditor — your skips and standing rules — stays taught. The clear can't be undone.",
      "Clear findings",
      true,
    );
    if (!ok) return;
    try {
      const msg = await api.auditorClearFindings();
      Toast.success(msg, 7000);
      await this.reload();
      this.hooks?.refreshBadge?.();
    } catch (err) {
      Toast.error(errText(err));
    }
  }

  private renderChecklist(): HTMLElement {
    const block = el("div", "mr-tmdb");
    // Right-click the findings surface: Clear findings (item 13).
    block.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      e.stopPropagation();
      this.ctxMenu.open(e.clientX, e.clientY, [
        {
          label: "Clear findings",
          danger: true,
          action: () => void this.clearFindings(),
        },
      ]);
    });
    const open = this.flags.filter((f) => f.state === "open");
    block.appendChild(
      el("h4", "mc-h", `Checklist — ${open.length} open`),
    );
    if (!open.length) {
      block.appendChild(
        el(
          "p",
          "prefs-note",
          "Nothing open. Either the library is clean, or everything found so far has been settled — settled items are listed under Memory, below. Right-click here to clear the findings list and start fresh.",
        ),
      );
      const runBtn = el("button", "pill-btn primary", "Run audit");
      runBtn.addEventListener("click", () => void this.runAuditNow(runBtn));
      block.appendChild(runBtn);
      return block;
    }
    for (const [fam, label] of FAMILY_ORDER) {
      const group = open.filter((f) => f.family === fam);
      if (!group.length) continue;
      block.appendChild(el("h5", "au-fam", `${label} — ${group.length}`));
      for (const flag of group) {
        block.appendChild(this.renderFlag(flag));
      }
    }
    return block;
  }

  private renderFlag(flag: AuditorFlag): HTMLElement {
    const card = el("div", "au-flag");
    const head = el("div", "au-flag-head");
    const badge = el(
      "span",
      flag.lane === "certain" ? "au-lane au-lane-fact" : "au-lane au-lane-ask",
      flag.lane === "certain" ? "Fact" : "Question",
    );
    head.appendChild(badge);
    head.appendChild(el("strong", "", flag.title));
    card.appendChild(head);
    card.appendChild(el("div", "au-path", flag.path));
    if (flag.detail) card.appendChild(el("p", "au-detail", flag.detail));

    // The answer grammar (Dickles, 2026-10-15 — four exact labels,
    // complete action set on question cards): the proposal is the
    // card's BODY text, and the buttons are "Just this one",
    // "Fix All Similar (Irreversible)", "Skip This One Once",
    // "Skip This One Forever". Fact cards keep their direct fix
    // button and share the same two Skip wordings for dismissals.
    // The old scope select + "This one / It's fine as-is" is gone.
    const p = flag.proposal;
    let proposalText: string | null = null;
    let applyResolution: string | null = null;
    if (p?.kind === "rename" && p.to) {
      proposalText = `Rename to “${p.to}”`;
      applyResolution = "apply";
    } else if (p?.kind === "trash") {
      proposalText = "Move to Trash";
      applyResolution = "apply";
    } else if (p?.kind === "convert") {
      proposalText =
        p.convertKind === "flac"
          ? "Convert to FLAC (lossless)"
          : p.convertKind === "hevc"
            ? "Re-encode to H.265"
            : "Convert to PNG (lossless)";
      applyResolution = "apply";
    } else if (
      p?.kind === "none" &&
      flag.family !== "editions" &&
      !flag.classKey.startsWith("pattern:gap")
    ) {
      proposalText = "Move to Trash";
      applyResolution = "trash_it";
    }

    if (flag.lane === "question" && proposalText) {
      card.appendChild(el("p", "au-proposal", `Proposal: ${proposalText}`));
      if (p?.kind === "convert" && p.replace) {
        card.appendChild(
          el("p", "prefs-note", "The original goes to Trash after — restoring it is one Undo."),
        );
      }
    }

    const actions = el("div", "mr-keyrow au-actions");
    const mkBtn = (
      label: string,
      resolution: string,
      primary = false,
    ): HTMLButtonElement => {
      const b = el("button", primary ? "pill-btn primary" : "pill-btn", label);
      b.addEventListener("click", () => void this.resolve(flag, resolution, null, b));
      return b;
    };
    if (flag.lane === "question") {
      if (applyResolution) {
        actions.appendChild(mkBtn("Just this one", applyResolution, true));
        actions.appendChild(mkBtn("Fix All Similar (Irreversible)", "apply_all"));
      }
    } else if (applyResolution && proposalText) {
      // Fact card: the verified fix stays one direct click.
      actions.appendChild(mkBtn(proposalText, applyResolution, true));
      if (p?.kind === "convert" && p.replace) {
        actions.appendChild(
          el("span", "mr-status", "the original goes to Trash after — restoring it is one Undo"),
        );
      }
    }
    actions.appendChild(mkBtn("Skip This One Once", "skip_once"));
    actions.appendChild(mkBtn("Skip This One Forever", "skip_forever"));
    card.appendChild(actions);
    return card;
  }

  private async resolve(
    flag: AuditorFlag,
    resolution: string,
    scope: string | null,
    btn: HTMLButtonElement,
  ): Promise<void> {
    btn.toggleAttribute("disabled", true);
    try {
      const msg = await api.auditorResolve(flag.id, resolution, scope);
      Toast.success(msg, 7000);
      await this.reload();
      this.hooks?.refreshBadge?.();
    } catch (err) {
      Toast.error(errText(err));
      btn.toggleAttribute("disabled", false);
    }
  }

  // ---------------------------------------------------------------
  // What gets checked (families + knobs)
  // ---------------------------------------------------------------

  private renderFamilies(s: AuditorSettings): HTMLElement {
    const block = el("div", "mr-tmdb");
    block.appendChild(el("h4", "mc-h", "What gets checked"));
    const mk = (
      label: string,
      blurb: string,
      get: () => boolean,
      set: (v: boolean) => void,
    ): HTMLElement => {
      const wrap = el("div", "au-famrow");
      const row = el("label", "mc-check");
      const box = document.createElement("input");
      box.type = "checkbox";
      box.checked = get();
      box.addEventListener("change", () => {
        set(box.checked);
        void this.saveSettings();
      });
      row.append(box, el("span", "", label));
      wrap.append(row, el("p", "prefs-note", blurb));
      return wrap;
    };
    const F = s.families;
    block.appendChild(mk("Possible typos", FAMILY_BLURBS.typo, () => F.typo, (v) => (F.typo = v)));
    block.appendChild(mk("Pattern breaks", FAMILY_BLURBS.pattern, () => F.pattern, (v) => (F.pattern = v)));
    block.appendChild(mk("Corrupt & unreadable", FAMILY_BLURBS.corrupt, () => F.corrupt, (v) => (F.corrupt = v)));
    block.appendChild(mk("Windows / macOS compatibility (opt-in)", FAMILY_BLURBS.compat, () => F.compat, (v) => (F.compat = v)));
    block.appendChild(mk("Space savers", FAMILY_BLURBS.space, () => F.space, (v) => (F.space = v)));
    block.appendChild(mk("Junk & promo files", FAMILY_BLURBS.junk, () => F.junk, (v) => (F.junk = v)));
    block.appendChild(mk("Clutter", FAMILY_BLURBS.clutter, () => F.clutter, (v) => (F.clutter = v)));
    block.appendChild(mk("Naming style drift", FAMILY_BLURBS.style, () => F.style, (v) => (F.style = v)));
    block.appendChild(mk("Duplicate editions", FAMILY_BLURBS.editions, () => F.editions, (v) => (F.editions = v)));
    block.appendChild(mk("Cold storage", FAMILY_BLURBS.cold, () => F.cold, (v) => (F.cold = v)));

    const knobs = el("div", "mr-keyrow");
    const mkNum = (
      label: string,
      value: number,
      set: (v: number) => void,
    ): HTMLElement => {
      const wrap = el("label", "mc-check");
      const input = document.createElement("input");
      input.className = "mr-in";
      input.style.maxWidth = "70px";
      input.type = "number";
      input.min = "1";
      input.value = String(value);
      input.addEventListener("change", () => {
        const v = parseInt(input.value, 10);
        if (Number.isFinite(v) && v > 0) {
          set(v);
          void this.saveSettings();
        }
      });
      wrap.append(el("span", "", label), input);
      return wrap;
    };
    knobs.append(
      mkNum("Partial/temp files are stale after (days)", s.staleDays, (v) => (s.staleDays = v)),
      mkNum("Cold storage after (years untouched)", s.coldYears, (v) => (s.coldYears = v)),
    );
    block.appendChild(knobs);
    return block;
  }

  // ---------------------------------------------------------------
  // Schedules
  // ---------------------------------------------------------------

  private renderSchedules(s: AuditorSettings): HTMLElement {
    const block = el("div", "mr-tmdb");
    block.appendChild(el("h4", "mc-h", "Scheduled audits"));
    block.appendChild(
      el(
        "p",
        "prefs-note",
        "Scheduled audits ride the same background agent as Media Completion (turn it on in Preferences → Media Completion). Missed runs catch up once at the next start, and the summary admits it ran late. A scheduled audit only REPORTS — findings wait here for you; nothing is fixed at 3 AM.",
      ),
    );
    for (const sched of s.schedules) {
      const row = el("div", "mc-sched");
      const head = el("div", "mc-sched-head");
      const enabled = document.createElement("input");
      enabled.type = "checkbox";
      enabled.checked = sched.enabled;
      enabled.title = "Enabled";
      enabled.addEventListener("change", () => {
        sched.enabled = enabled.checked;
        void this.saveSettings();
      });
      head.appendChild(enabled);
      head.appendChild(el("strong", "", sched.name));
      head.appendChild(
        el(
          "span",
          "mr-status",
          `${sched.target} · daily ${sched.time}${sched.idleMinutes ? ` · after ${sched.idleMinutes} idle min` : ""}${sched.requireOnAc ? " · on AC only" : ""}`,
        ),
      );
      row.appendChild(head);
      if (sched.lastStatus) {
        row.appendChild(
          el("p", "prefs-note", `Last run: ${sched.lastStarted ?? "?"} — ${sched.lastStatus}`),
        );
      }
      const actions = el("div", "mr-keyrow");
      const runNow = el("button", "pill-btn", "Run now");
      runNow.addEventListener("click", async () => {
        runNow.textContent = "Auditing…";
        runNow.toggleAttribute("disabled", true);
        try {
          await api.auditorRunNow(sched.target, sched.id);
          Toast.success(`Audit of ${sched.target} finished.`, 8000);
          await this.reload();
          this.hooks?.refreshBadge?.();
        } catch (err) {
          Toast.error(`Audit failed: ${errText(err)}`);
          await this.reload();
        }
      });
      const del = el("button", "pill-btn", "Delete schedule");
      del.addEventListener("click", () => {
        s.schedules = s.schedules.filter((x) => x.id !== sched.id);
        void this.saveSettings();
      });
      actions.append(runNow, del);
      row.appendChild(actions);
      block.appendChild(row);
    }

    const form = el("div", "mc-add");
    form.appendChild(el("div", "mc-h", "Add a schedule"));
    const grid = el("div", "mrx-grid");
    const name = document.createElement("input");
    name.className = "mr-in";
    name.placeholder = "Name (e.g. Weekend drive audit)";
    const target = document.createElement("input");
    target.className = "mr-in";
    target.placeholder = "Drive or folder to audit";
    const time = document.createElement("input");
    time.className = "mr-in";
    time.type = "time";
    time.value = "03:00";
    const idle = document.createElement("input");
    idle.className = "mr-in";
    idle.placeholder = "Idle minutes (0 = none)";
    idle.inputMode = "numeric";
    grid.append(name, target, time, idle);
    form.appendChild(grid);
    const pickRow = el("div", "mr-keyrow");
    const pick = el("button", "pill-btn", "Choose target folder…");
    pick.addEventListener("click", async () => {
      const picked = await this.hooks?.pickFolder();
      if (picked) target.value = picked;
    });
    const acWrap = el("label", "mc-check");
    const ac = document.createElement("input");
    ac.type = "checkbox";
    acWrap.append(ac, el("span", "", "On AC only"));
    pickRow.append(pick, acWrap);
    form.appendChild(pickRow);
    const days = el("div", "mr-keyrow");
    days.appendChild(el("span", "mr-status", "Days (none ticked = every day)"));
    const dayBoxes: HTMLInputElement[] = [];
    for (const d of WEEKDAYS) {
      const wrap = el("label", "mc-check");
      const box = document.createElement("input");
      box.type = "checkbox";
      wrap.append(box, el("span", "", d));
      days.appendChild(wrap);
      dayBoxes.push(box);
    }
    form.appendChild(days);
    const add = el("button", "pill-btn primary", "Add schedule");
    add.addEventListener("click", () => {
      const n = name.value.trim();
      const t = target.value.trim();
      if (!n || !t) {
        Toast.error("A schedule needs a name and a target folder.");
        return;
      }
      s.schedules.push({
        id: crypto.randomUUID(),
        name: n,
        target: t,
        time: time.value || "03:00",
        weekdays: dayBoxes.map((b) => b.checked),
        idleMinutes: parseInt(idle.value, 10) || 0,
        requireOnAc: ac.checked,
        enabled: true,
        lastStarted: null,
        lastStatus: null,
      });
      void this.saveSettings();
    });
    form.appendChild(add);
    block.appendChild(form);
    return block;
  }

  // ---------------------------------------------------------------
  // Memory: dismissals, rules, taught words — reviewable, resettable
  // ---------------------------------------------------------------

  private renderMemory(): HTMLElement {
    const block = el("div", "mr-tmdb");
    block.appendChild(el("h4", "mc-h", "Memory — what the Auditor has learned"));
    block.appendChild(
      el(
        "p",
        "prefs-note",
        "Every “Skip This One Forever” and every standing rule lives here, until you remove it. Skipped files won't be flagged again while their entry stands, and ruled classes follow their rule.",
      ),
    );

    block.appendChild(el("h5", "au-fam", `Standing rules (${this.rules.length})`));
    if (!this.rules.length) {
      block.appendChild(el("p", "prefs-note", "No standing rules yet — answer a Question with “Fix All Similar (Irreversible)” to make one."));
    }
    for (const rule of this.rules) {
      const row = el("div", "mr-keyrow");
      row.appendChild(
        el(
          "span",
          "mr-status",
          `${describeClass(rule.classKey)} — ${rule.decision === "suppress" ? "never flag" : rule.decision === "fix" ? "arrives as a fact with the fix proposed" : "this is fine"} (since ${rule.created.slice(0, 10)})`,
        ),
      );
      const del = el("button", "pill-btn", "Remove rule");
      del.addEventListener("click", async () => {
        try {
          await api.auditorDeleteRule(rule.classKey);
          await this.reload();
        } catch (err) {
          Toast.error(errText(err));
        }
      });
      row.appendChild(del);
      block.appendChild(row);
    }

    block.appendChild(el("h5", "au-fam", `Skipped forever (${this.dismissals.length})`));
    if (!this.dismissals.length) {
      block.appendChild(el("p", "prefs-note", "No per-file dismissals yet."));
    }
    for (const d of this.dismissals) {
      const row = el("div", "mr-keyrow");
      row.appendChild(
        el("span", "mr-status au-dismiss-path", `${d.path} (${d.family})`),
      );
      const del = el("button", "pill-btn", "Forget");
      del.addEventListener("click", async () => {
        try {
          await api.auditorDeleteDismissal(d.path, d.family);
          await this.reload();
        } catch (err) {
          Toast.error(errText(err));
        }
      });
      row.appendChild(del);
      block.appendChild(row);
    }

    block.appendChild(el("h5", "au-fam", "Words the typo hunt trusts"));
    block.appendChild(
      el(
        "p",
        "prefs-note",
        "Names and words you've confirmed are spelled right — the dictionary the typo hunt checks against grows with your library. Add family names, places, and project words here.",
      ),
    );
    const wordRow = el("div", "mr-keyrow");
    const wordIn = document.createElement("input");
    wordIn.className = "mr-in";
    wordIn.placeholder = "Add a word (e.g. a family name)";
    const addWord = el("button", "pill-btn", "Teach it");
    addWord.addEventListener("click", async () => {
      const w = wordIn.value.trim();
      if (!w) return;
      try {
        this.words = await api.auditorAddUserWord(w);
        wordIn.value = "";
        this.render();
      } catch (err) {
        Toast.error(errText(err));
      }
    });
    wordRow.append(wordIn, addWord);
    block.appendChild(wordRow);
    if (this.words.length) {
      block.appendChild(
        el("p", "prefs-note", this.words.join(" · ")),
      );
    }
    return block;
  }
}
