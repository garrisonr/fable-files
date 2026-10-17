/**
 * Add-location safety gate (Hyperbase Solution #3, 2026-10-16).
 *
 * Adding a folder used to be one click straight into a full index
 * walk. For a game folder that's right; for a 12 TB preloaded drive
 * it would triple the catalog with PC-game asset files before the
 * user saw a single number. The gate:
 *
 * 1. Census first (metadata-only, read-only — cancelling after it
 *    changes nothing anywhere: no location, no scope, no rows).
 * 2. Under the threshold, the add proceeds exactly as it always
 *    did — no sheet, no extra click.
 * 3. Over the threshold, a sheet shows the census: per-folder
 *    counts with pick defaults from the census classes (ROM trees
 *    on, giant asset trees off and flagged), a ROM-focus toggle,
 *    and live estimates (catalog growth, identify read volume,
 *    walk time). Confirming registers the location WITH its scope;
 *    the backend then walks only what the scope admits, on this
 *    and every later indexing pass.
 *
 * The pick/estimate math lives in addscope.ts (pure, harness-
 * proven); this module is the DOM and the flow.
 */
import { api, formatBytes, type LocationSummary } from "./api";
import {
  buildScope,
  defaultPicks,
  entryItems,
  estIdentifySecs,
  estIndexSecs,
  humanDuration,
  selectionStats,
  type CensusReport,
  type LocationScopeWire,
  type TopEntryWire,
} from "./addscope";

export interface AddGateHooks {
  /** After registration (either door): refresh the sidebar, kick
   * indexing, toast — the caller's existing post-add routine. */
  onAdded(summary: LocationSummary): void;
}

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

const COUNTING_DELAY_MS = 600;

export class AddLocationGate {
  private overlay: HTMLDivElement;
  private bodyEl!: HTMLDivElement;
  private footEl!: HTMLDivElement;
  private titleEl!: HTMLHeadingElement;
  /** gateExisting() plumbing: the pending promise's settlers, and
   * the abort flag a counting-phase Cancel raises via close(). */
  private gateResolve: ((ok: boolean) => void) | null = null;
  private gateReject: ((err: unknown) => void) | null = null;
  private gateCounting = false;
  private gateAborted = false;

  constructor(private hooks: AddGateHooks) {
    this.overlay = document.createElement("div");
    this.overlay.className = "picker-overlay";
    this.overlay.hidden = true;
    this.overlay.innerHTML = `
      <div class="picker-card labels-card mrx-card gate-card" role="dialog" aria-label="Add location">
        <h3 class="picker-title mrx-title"></h3>
        <div class="mrx-body"></div>
        <div class="picker-actions mrx-foot"></div>
      </div>`;
    document.body.appendChild(this.overlay);
    this.titleEl = this.overlay.querySelector(".mrx-title")!;
    this.bodyEl = this.overlay.querySelector(".mrx-body")!;
    this.footEl = this.overlay.querySelector(".mrx-foot")!;
    this.overlay.addEventListener("pointerdown", (e) => {
      if (e.target === this.overlay) this.close();
    });
  }

  private close(): void {
    this.overlay.hidden = true;
    // Closing is cancelling: a counting gate aborts, a sheet gate
    // settles false — the caller must not proceed.
    if (this.gateCounting) this.gateAborted = true;
    this.settleGate(false);
  }

  /** Settle a pending gateExisting() promise. Returns whether a
   * gate caller was waiting. */
  private settleGate(ok: boolean): boolean {
    const resolve = this.gateResolve;
    this.gateResolve = null;
    this.gateReject = null;
    if (!resolve) return false;
    resolve(ok);
    return true;
  }

  /** Reject a pending gateExisting() promise (the failure is the
   * caller's to report). Returns whether a gate caller waited. */
  private rejectGate(err: unknown): boolean {
    const reject = this.gateReject;
    this.gateResolve = null;
    this.gateReject = null;
    if (!reject) return false;
    this.close();
    reject(err);
    return true;
  }

  /** The slow-census sheet: appears only if the count runs long,
   * so fast trees never show UI at all. Its Cancel closes the
   * overlay — for a gate caller, close() reads as an abort. */
  private armCountingSheet(name: string): number {
    return window.setTimeout(() => {
      this.titleEl.textContent = `Counting “${name}”…`;
      this.bodyEl.innerHTML = "";
      this.bodyEl.appendChild(
        el(
          "p",
          "prefs-note",
          "Counting files before anything is indexed — this reads names and sizes only, never file contents.",
        ),
      );
      this.footEl.innerHTML = "";
      const cancel = el("button", "pill-btn", "Cancel");
      cancel.addEventListener("click", () => this.close());
      this.footEl.appendChild(cancel);
      this.overlay.hidden = false;
    }, COUNTING_DELAY_MS);
  }

  /**
   * The add flow for a user-picked folder. `romDoor` marks the
   * ROM Home door: it only pre-checks the ROM-focus toggle if the
   * sheet opens. Throws on census/registration failure — the
   * caller toasts, as the un-gated flow did.
   */
  async add(
    name: string,
    path: string,
    opts: { romDoor: boolean },
  ): Promise<void> {
    // Slow censuses (a multi-million-file tree) get a sheet in a
    // counting state; fast ones never show UI at all.
    const slow = this.armCountingSheet(name);
    let report: CensusReport;
    try {
      report = await api.censusLocation(path);
    } finally {
      window.clearTimeout(slow);
    }
    if (!report.overThreshold) {
      this.close();
      const summary = await api.addPathLocation(name, path);
      this.hooks.onAdded(summary);
      return;
    }
    this.renderSheet(name, path, report, opts.romDoor, "add", async (scope) => {
      const summary = await api.addScopedLocation(name, path, scope);
      this.hooks.onAdded(summary);
    });
  }

  /**
   * The gate for an ALREADY-LISTED location — the Re-index
   * Library amendment (Dickles, 2026-10-16): a volume must pass
   * the same census before it queues, so this door can never walk
   * a huge drive unscoped either.
   *
   * Resolves true when the caller may queue the location: either
   * the census came in under the threshold (nothing recorded —
   * the walk is the historic one) or the user confirmed the pick
   * sheet, whose scope is saved for the root first. Registration
   * is reused by path, so the volume keeps its identity; only
   * the scope is new. Resolves false on cancel — the caller must
   * NOT queue it. Rejects on census/save failure: fail closed.
   */
  async gateExisting(name: string, path: string): Promise<boolean> {
    this.gateAborted = false;
    this.gateCounting = true;
    const slow = this.armCountingSheet(name);
    let report: CensusReport;
    try {
      report = await api.censusLocation(path);
    } finally {
      window.clearTimeout(slow);
      this.gateCounting = false;
    }
    if (this.gateAborted) {
      this.close();
      return false;
    }
    if (!report.overThreshold) {
      this.close();
      return true;
    }
    return new Promise<boolean>((resolve, reject) => {
      this.gateResolve = resolve;
      this.gateReject = reject;
      this.renderSheet(name, path, report, false, "reindex", async (scope) => {
        await api.addScopedLocation(name, path, scope);
      });
    });
  }

  private renderSheet(
    name: string,
    path: string,
    report: CensusReport,
    romDoor: boolean,
    mode: "add" | "reindex",
    onConfirm: (scope: LocationScopeWire) => Promise<void>,
  ): void {
    const census = report.census;
    const defaults = defaultPicks(census);
    const picked = new Set(defaults.picked);
    let rootFiles = defaults.rootFiles;
    let romFocus = romDoor;

    this.titleEl.textContent =
      mode === "add"
        ? `Add “${name}” — it's a big one`
        : `Re-index “${name}” — it's a big one`;
    const body = this.bodyEl;
    body.innerHTML = "";
    body.appendChild(
      el(
        "p",
        "mrx-intro",
        `Fable counted first, before indexing anything: ${census.files.toLocaleString()} files in ${census.dirs.toLocaleString()} folders, ${formatBytes(census.bytes)} in all. Choose what joins the library — everything stays on the drive and stays browsable either way; this only decides what gets cataloged.`,
      ),
    );

    // EmulationStation data rides along with a detected tree: the
    // sheet names it and where the import lives — the census
    // itself imports nothing (Hyperbase #5).
    if (report.es && report.es.systems.length > 0) {
      const names = report.es.systems.slice(0, 4).map((s) => s.esName);
      const more =
        report.es.systems.length > names.length
          ? `, +${report.es.systems.length - names.length} more`
          : "";
      body.appendChild(
        el(
          "p",
          "prefs-note gate-es-note",
          `EmulationStation lists found for ${report.es.systems.length} systems (${names.join(", ")}${more}). After indexing, Games Home's ⋯ menu → “Import EmulationStation data” brings in their descriptions, artwork and play history — game identity still comes from the DAT sets, never the lists.`,
        ),
      );
    }

    const list = el("div", "mrx-list gate-list");
    const statsLine = el("p", "gate-stats prefs-note");
    const addBtn = el("button", "pill-btn primary") as HTMLButtonElement;

    const renderStats = (): void => {
      const sel = selectionStats(census, picked, rootFiles, romFocus);
      const walk = humanDuration(estIndexSecs(census, sel.items));
      const identify = romFocus
        ? ` · identifying reads ${formatBytes(sel.candidateBytes)} of games (${humanDuration(estIdentifySecs(sel.candidateBytes))})`
        : "";
      statsLine.textContent =
        sel.items > 0
          ? `Catalog grows by ≈ ${sel.items.toLocaleString()} items · indexing ${walk} (est.)${identify}`
          : "Nothing selected — pick at least one folder to add.";
      addBtn.disabled = sel.items === 0;
      const verb = mode === "add" ? "Add" : "Re-index";
      addBtn.textContent =
        sel.items > 0
          ? `${verb} ${sel.items.toLocaleString()} items`
          : verb;
    };

    for (const entry of census.top) {
      list.appendChild(
        this.entryRow(entry, defaults.flagged.get(entry.name), romFocus, {
          checked: entry.name === "" ? rootFiles : picked.has(entry.name),
          onToggle: (on) => {
            if (entry.name === "") rootFiles = on;
            else if (on) picked.add(entry.name);
            else picked.delete(entry.name);
            renderStats();
          },
        }),
      );
    }
    body.appendChild(list);

    // ROM focus: the row that turns a drive of everything into a
    // library of games.
    const romRow = el("label", "gate-row gate-rom-row");
    const romBox = document.createElement("input");
    romBox.type = "checkbox";
    romBox.checked = romFocus;
    romBox.addEventListener("change", () => {
      romFocus = romBox.checked;
      // Row counts restate themselves in the new mode.
      for (const row of list.querySelectorAll<HTMLElement>(".gate-row")) {
        const counts = row.querySelector<HTMLElement>(".gate-counts");
        const entry = (row as unknown as { __entry?: TopEntryWire }).__entry;
        if (counts && entry) {
          counts.textContent = countsText(entry, romFocus);
        }
      }
      renderStats();
    });
    const romText = el("span", "gate-row-text");
    romText.appendChild(el("span", "gate-row-name", "This is a ROM (Games) drive — index games only"));
    romText.appendChild(
      el(
        "span",
        "gate-row-note",
        "Folders and game files are cataloged; artwork, manuals and other files stay browsable but out of the catalog.",
      ),
    );
    romRow.append(romBox, romText);
    body.appendChild(romRow);
    body.appendChild(statsLine);

    const foot = this.footEl;
    foot.innerHTML = "";
    const cancel = el("button", "pill-btn", "Cancel");
    cancel.addEventListener("click", () => this.close());
    addBtn.addEventListener("click", () => {
      void (async () => {
        const scope = buildScope(census, picked, rootFiles, romFocus);
        try {
          await onConfirm(scope);
        } catch (err) {
          // A pending gate caller hears the failure and decides;
          // the add door keeps its historic behavior.
          if (this.rejectGate(err)) return;
          throw err;
        }
        this.settleGate(true);
        this.close();
      })();
    });
    foot.append(cancel, addBtn);
    renderStats();
    this.overlay.hidden = false;
  }

  private entryRow(
    entry: TopEntryWire,
    flag: string | undefined,
    romFocus: boolean,
    opts: {
      checked: boolean;
      onToggle(on: boolean): void;
    },
  ): HTMLElement {
    const row = el("label", "gate-row");
    (row as unknown as { __entry?: TopEntryWire }).__entry = entry;
    const box = document.createElement("input");
    box.type = "checkbox";
    box.checked = opts.checked;
    box.addEventListener("change", () => opts.onToggle(box.checked));
    const text = el("span", "gate-row-text");
    text.appendChild(
      el(
        "span",
        "gate-row-name",
        entry.name === "" ? "Files at the top level" : entry.name,
      ),
    );
    if (flag) {
      text.appendChild(el("span", "gate-row-note", `Starts off: ${flag}`));
    }
    const counts = el("span", "gate-counts", countsText(entry, romFocus));
    row.append(box, text, counts);
    return row;
  }
}

function countsText(entry: TopEntryWire, romFocus: boolean): string {
  const items = entryItems(entry, romFocus);
  return romFocus
    ? `${entry.candidateFiles.toLocaleString()} games · ${items.toLocaleString()} items · ${formatBytes(entry.bytes)}`
    : `${entry.files.toLocaleString()} files · ${formatBytes(entry.bytes)}`;
}
