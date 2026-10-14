/**
 * Instant search: debounced ~120ms, results replace the file view.
 * Shows name / path / rank; activating a hit navigates into it (folders)
 * or opens Quick Look (files). SearchHit carries no parent-dir field, so
 * "reveal in folder" is impossible until the backend adds one — click opens
 * the item itself, which is the honest subset for this milestone.
 */
import {
  api,
  formatBytes,
  onRomIdentifyProgress,
  type Item,
  type RomScope,
  type SearchHit,
} from "./api";
import { ContextMenu } from "./contextmenu";
import { errText } from "./toast";

export interface SearchHooks {
  onNavigateToItem(item: Item): void;
  /** Absolute path of the folder the file view shows (folder scope). */
  currentDirPath(): string | null | Promise<string | null>;
  /** Locations for the scope picker (null id = everywhere). */
  scopes(): Array<{ id: string | null; name: string; provider?: string }>;
  onPreviewItem(item: Item): void;
  /** Trash the selected hits (shell owns the batch + toast + undo). */
  onTrashItems?(items: Item[]): void;
  /** Right-click on a hit: the shell shows the full item menu. */
  onContextMenu?(item: Item, targets: Item[], x: number, y: number): void;
  /** Results are showing: the shell should hide the file view. */
  onResults(): void;
  onClear(): void;
  /** "Re-index" button in the results header (Round 7.1). */
  onReindex?(): void;
  /** Save the current query + filter as a smart folder. */
  onSaveSmartFolder?(): void;
}

const DEBOUNCE_MS = 120;

const SEARCH_ICON_SVG = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><circle cx="11" cy="11" r="7"/><path d="m20 20-3.8-3.8"/></svg>`;

const TYPE_FILTERS: Array<{ id: string | null; label: string }> = [
  { id: null, label: "All types" },
  { id: "image", label: "Images" },
  { id: "video", label: "Videos" },
  { id: "audio", label: "Audio" },
  { id: "document", label: "Documents" },
  { id: "archive", label: "Archives" },
];

export class SearchView {
  readonly el: HTMLElement;
  private input: HTMLInputElement;
  private fieldEl: HTMLElement;
  private resultsEl: HTMLElement;
  private chipsEl: HTMLElement;
  private typeFilter: string | null = null;
  /** Scope: "all" | "folder" | a location id. Plus modified-date range. */
  private scopeChoice = "all";
  private afterDate = "";
  private beforeDate = "";
  private filtersEl!: HTMLDivElement;
  private scopeBtn!: HTMLButtonElement;
  private afterInput!: HTMLInputElement;
  private beforeInput!: HTMLInputElement;
  private scopeMenu = new ContextMenu();
  private timer = 0;
  private lastQuery = "";
  private hits: SearchHit[] = [];
  private selected = new Set<string>();
  private anchor = -1;
  /** Delete/Backspace trash the selection ONLY when the user's last
   * interaction was with a result (click/arrow), never while they are
   * editing the query — focus alone is unreliable in WebKitGTK, so
   * this is tracked explicitly. */
  private deleteArmed = false;
  active = false;
  /** Display name for the path bar while this view is up (smart folder /
   * ROM system name). Set by runRule and by the shell for saved-query
   * smart folders; null for plain typed searches. */
  titleHint: string | null = null;
  /** Last rule view shown, so a completed re-index can refresh it. */
  private lastRule: {
    name: string;
    rule: { extensions: string[]; minDurationSecs?: number | null };
  } | null = null;
  /** Last ROM collection shown (DAT-identified membership). */
  private lastRom: { name: string; systemKey: string; scope: RomScope } | null =
    null;
  /** Display-name overrides for the current rule/collection render:
   * item id → DAT name (ROM Lists: the DAT name headlines, the real
   * filename stays in the path line). */
  private displayNames: Map<string, string> | null = null;
  private romProgressWired = false;
  /** Set by runExternal so run() can tell a smart-folder invocation
   * (keep titleHint) from a hand-typed search (clear titleHint). */
  private externalRun = false;
  /** Indexing state mirrored from the shell's IndexTracker (inc-2
   * item 13): while the index is incomplete, results are partial
   * and must SAY so — a bare "No results" during indexing is a lie
   * by omission. */
  private indexingBusy = false;
  private indexingFiles: number | null = null;
  private bannerEl!: HTMLDivElement;
  private bannerTextEl!: HTMLSpanElement;
  /** The last typed-search render, so the empty-state copy can be
   * rephrased the moment indexing state flips. */
  private lastResults: { q: string; hits: SearchHit[] } | null = null;

  constructor(private hooks: SearchHooks) {
    this.el = document.createElement("div");
    this.el.className = "searchview";
    this.el.hidden = true;

    // Advanced search: file-type filter chips (persistent above results).
    this.chipsEl = document.createElement("div");
    this.chipsEl.className = "search-chips";
    this.chipsEl.setAttribute("role", "group");
    this.chipsEl.setAttribute("aria-label", "Filter by file type");
    for (const f of TYPE_FILTERS) {
      const chip = document.createElement("button");
      chip.className = "search-chip" + (f.id === null ? " active" : "");
      chip.textContent = f.label;
      chip.dataset.filter = f.id ?? "";
      chip.addEventListener("click", () => {
        this.typeFilter = f.id;
        this.chipsEl.querySelectorAll(".search-chip").forEach((c) =>
          c.classList.toggle("active", c === chip)
        );
        // Re-run under the new filter; type-only browsing (empty query)
        // is a valid search on its own.
        this.lastQuery = "\u0000force";
        void this.run();
      });
      this.chipsEl.appendChild(chip);
    }
    if (this.hooks.onSaveSmartFolder) {
      const save = document.createElement("button");
      save.className = "search-chip search-save";
      save.textContent = "☆ Save as Smart Folder";
      save.title = "Save this search in the sidebar";
      save.addEventListener("click", () => this.hooks.onSaveSmartFolder!());
      this.chipsEl.appendChild(save);
    }
    this.el.appendChild(this.chipsEl);

    // Scope + date filters (Round 5.6): where to search, and a
    // modified-date range. Plain language, always visible.
    this.filtersEl = document.createElement("div");
    this.filtersEl.className = "search-filters";
    const scopeWrap = document.createElement("span");
    scopeWrap.className = "search-filter-field";
    const scopeLab = document.createElement("span");
    scopeLab.className = "search-filter-label";
    scopeLab.textContent = "Search in:";
    this.scopeBtn = document.createElement("button");
    this.scopeBtn.className = "dropdown-btn search-scope-btn";
    this.scopeBtn.textContent = "Everywhere \u25be";
    this.scopeBtn.addEventListener("click", () => this.openScopeMenu());
    scopeWrap.append(scopeLab, this.scopeBtn);
    const afterWrap = document.createElement("label");
    afterWrap.className = "search-filter-field";
    afterWrap.textContent = "Modified after";
    this.afterInput = document.createElement("input");
    this.afterInput.type = "date";
    this.afterInput.className = "search-date";
    this.afterInput.addEventListener("change", () => {
      this.afterDate = this.afterInput.value;
      this.lastQuery = "\u0000force";
      void this.run();
    });
    afterWrap.appendChild(this.afterInput);
    const beforeWrap = document.createElement("label");
    beforeWrap.className = "search-filter-field";
    beforeWrap.textContent = "Modified before";
    this.beforeInput = document.createElement("input");
    this.beforeInput.type = "date";
    this.beforeInput.className = "search-date";
    this.beforeInput.addEventListener("change", () => {
      this.beforeDate = this.beforeInput.value;
      this.lastQuery = "\u0000force";
      void this.run();
    });
    beforeWrap.appendChild(this.beforeInput);
    this.filtersEl.append(scopeWrap, afterWrap, beforeWrap);
    this.el.appendChild(this.filtersEl);

    // Indexing banner (inc-2 item 13): a sibling of the results
    // wrap — the render methods wipe resultsEl, the banner must
    // survive them. Hidden unless indexing is active. Fix block
    // item 8: the spinner is a real CSS spinner in theme tokens,
    // not the ⏳ emoji (which rendered as a Windows-95 hourglass).
    this.bannerEl = document.createElement("div");
    this.bannerEl.className = "search-indexing-banner";
    this.bannerEl.hidden = true;
    const spinner = document.createElement("span");
    spinner.className = "search-spinner";
    spinner.setAttribute("aria-hidden", "true");
    this.bannerTextEl = document.createElement("span");
    this.bannerTextEl.className = "search-indexing-text";
    this.bannerEl.append(spinner, this.bannerTextEl);
    this.el.appendChild(this.bannerEl);

    this.resultsEl = document.createElement("div");
    this.resultsEl.className = "search-results-wrap";
    this.el.appendChild(this.resultsEl);

    // The field: a pill with the magnifier icon INSIDE, Apple-style.
    this.fieldEl = document.createElement("div");
    this.fieldEl.className = "search-field";
    const icon = document.createElement("span");
    icon.className = "search-field-icon";
    icon.innerHTML = SEARCH_ICON_SVG;
    this.fieldEl.appendChild(icon);

    this.input = document.createElement("input");
    this.input.className = "search-input";
    this.input.type = "search";
    this.input.placeholder = "Search files";
    this.input.setAttribute("aria-label", "Search files");
    this.input.addEventListener("input", () => this.onInput());
    this.input.addEventListener("keydown", (e) => {
      if (e.key === "Escape") {
        this.clear();
        this.input.blur();
      }
    });
    this.input.addEventListener("focus", () => {
      // Typing in the query box disarms result deletion: Backspace
      // goes back to editing text until a result is clicked again.
      this.deleteArmed = false;
    });
    this.fieldEl.appendChild(this.input);
    // Clicking anywhere on the pill focuses the input.
    this.fieldEl.addEventListener("mousedown", (e) => {
      if (e.target !== this.input) {
        e.preventDefault();
        this.input.focus();
      }
    });
  }

  /** The active query + type filter (smart folders read these). */
  get currentQuery(): string {
    return this.input.value.trim();
  }

  get currentTypeFilter(): string | null {
    return this.typeFilter;
  }

  /** Run a search programmatically (smart folders, palette). */
  async runExternal(query: string, typeFilter: string | null): Promise<void> {
    this.externalRun = true;
    this.input.value = query;
    this.typeFilter = typeFilter;
    this.chipsEl.querySelectorAll<HTMLElement>(".search-chip").forEach((c) =>
      c.classList.toggle("active", (c.dataset.filter ?? "") === (typeFilter ?? "")),
    );
    this.lastQuery = "\u0000force";
    await this.run();
  }

  get field(): HTMLElement {
    return this.fieldEl;
  }

  private static keyOf(item: Item): string {
    return item.provider_key ?? item.id;
  }

  /** The hits the user has selected (click / Ctrl+click / Shift+click). */
  selectedItems(): Item[] {
    return this.hits
      .filter((h) => this.selected.has(SearchView.keyOf(h.item)))
      .map((h) => h.item);
  }

  /**
   * Fire the trash hook for the current selection, if any. Clears the
   * selection first so a second routed call in the same event is a
   * no-op. Returns true when something was trashed.
   */
  tryTrashSelection(): boolean {
    const items = this.selectedItems();
    if (!this.deleteArmed || items.length === 0 || !this.hooks.onTrashItems)
      return false;
    this.deleteArmed = false;
    this.selected.clear();
    this.applySelection();
    this.hooks.onTrashItems(items);
    return true;
  }

  /** Indexing state from the shell's IndexTracker (inc-2 item 13).
   * The banner updates live (count ticks, no list rebuild); when the
   * busy flag flips while an EMPTY result is on screen, the results
   * re-render so the empty-state copy matches reality ("No matches
   * yet — still indexing…" vs the final "No results"). */
  setIndexingState(busy: boolean, files: number | null): void {
    const flipped = busy !== this.indexingBusy;
    this.indexingBusy = busy;
    if (files !== null) this.indexingFiles = files;
    if (!busy) this.indexingFiles = null;
    this.bannerEl.hidden = !busy;
    if (busy) {
      this.bannerTextEl.textContent =
        `Still indexing… results may be incomplete` +
        (this.indexingFiles !== null
          ? ` · ${this.indexingFiles.toLocaleString()} files indexed so far`
          : "");
    }
    if (flipped && this.active && this.lastResults && this.lastResults.hits.length === 0) {
      this.renderResults(this.lastResults.q, this.lastResults.hits);
    }
  }

  /** Re-run the current query (e.g. after trashing hits from it). */
  refresh(): void {
    if (!this.active) return;
    this.lastQuery = "\u0000force";
    void this.run();
  }

  private selectHit(index: number, e: MouseEvent): void {
    const hit = this.hits[index];
    if (!hit) return;
    const key = SearchView.keyOf(hit.item);
    if (e.ctrlKey || e.metaKey) {
      if (this.selected.has(key)) this.selected.delete(key);
      else this.selected.add(key);
      this.anchor = index;
    } else if (e.shiftKey && this.anchor >= 0) {
      const [a, b] = this.anchor < index ? [this.anchor, index] : [index, this.anchor];
      this.selected.clear();
      for (let i = a; i <= b; i++) this.selected.add(SearchView.keyOf(this.hits[i].item));
    } else {
      this.selected.clear();
      this.selected.add(key);
      this.anchor = index;
    }
    this.deleteArmed = true;
    this.applySelection();
  }

  private applySelection(): void {
    this.resultsEl.querySelectorAll<HTMLElement>(".search-hit").forEach((row) => {
      const idx = Number(row.dataset.index ?? "-1");
      const hit = this.hits[idx];
      const on = !!hit && this.selected.has(SearchView.keyOf(hit.item));
      row.classList.toggle("selected", on);
      row.setAttribute("aria-selected", on ? "true" : "false");
    });
  }

  private activate(item: Item): void {
    if (item.kind === "folder") this.hooks.onNavigateToItem(item);
    else this.hooks.onPreviewItem(item);
  }

  private focusHit(index: number): void {
    const rows = this.resultsEl.querySelectorAll<HTMLElement>(".search-hit");
    const row = rows[Math.max(0, Math.min(index, rows.length - 1))];
    if (row) {
      row.focus();
      this.selected.clear();
      const hit = this.hits[Number(row.dataset.index ?? "0")];
      if (hit) {
        this.selected.add(SearchView.keyOf(hit.item));
        this.anchor = Number(row.dataset.index ?? "0");
        this.deleteArmed = true;
      }
      this.applySelection();
    }
  }

  focus(): void {
    this.input.focus();
    this.input.select();
  }

  clear(): void {
    window.clearTimeout(this.timer);
    this.input.value = "";
    this.lastQuery = "";
    this.typeFilter = null;
    this.chipsEl.querySelectorAll(".search-chip").forEach((c) =>
      c.classList.toggle("active", (c as HTMLElement).dataset.filter === ""),
    );
    this.scopeChoice = "all";
    this.scopeBtn.textContent = "Everywhere \u25be";
    this.afterDate = "";
    this.beforeDate = "";
    this.afterInput.value = "";
    this.beforeInput.value = "";
    this.active = false;
    this.titleHint = null;
    this.lastResults = null;
    this.el.hidden = true;
    this.resultsEl.innerHTML = "";
    this.hooks.onClear();
  }

  /** A library re-index just finished: refresh what the user is
   * looking at so newly downloaded files appear without a restart. */
  rerunAfterIndex(): void {
    if (!this.active) return;
    if (this.lastRom) {
      void this.runRomCollection(
        this.lastRom.name,
        this.lastRom.systemKey,
        this.lastRom.scope,
      );
    } else if (this.lastRule) {
      void this.runRule(this.lastRule.name, this.lastRule.rule);
    } else this.refresh();
  }

  /** Results header: status line + the Re-index button (Round 7.1). */
  private resultsHeader(text: string): HTMLDivElement {
    const row = document.createElement("div");
    row.className = "search-head-row";
    const head = document.createElement("p");
    head.className = "search-status";
    head.textContent = text;
    row.appendChild(head);
    if (this.hooks.onReindex) {
      const b = document.createElement("button");
      b.className = "search-chip search-reindex";
      b.textContent = "↻ Re-index";
      b.title = "Re-index the library so newly added files appear";
      b.addEventListener("click", () => this.hooks.onReindex!());
      row.appendChild(b);
    }
    return row;
  }

  private onInput(): void {
    window.clearTimeout(this.timer);
    this.timer = window.setTimeout(() => void this.run(), DEBOUNCE_MS);
  }

  private async run(): Promise<void> {
    const q = this.input.value.trim();
    if (q === this.lastQuery) return;
    this.lastQuery = q;
    if (!this.externalRun) this.titleHint = null;
    this.externalRun = false;
    if (!q && !this.typeFilter) {
      this.clear();
      return;
    }
    this.active = true;
    this.el.hidden = false;
    this.hooks.onResults();
    this.renderLoading(q);
    try {
      const fa = await this.filterArgs();
      const hits = await api.search(q, fa.scope, 50, this.typeFilter, fa);
      if (this.lastQuery !== q) return; // superseded
      this.renderResults(q, hits);
    } catch {
      if (this.lastQuery !== q) return;
      this.renderError(q);
    }
  }

  /** Evaluate a rule smart folder (extensions + optional minimum
   * duration), looping while the backend is still probing durations. */
  async runRule(
    name: string,
    rule: { extensions: string[]; minDurationSecs?: number | null },
    scope?: RomScope,
  ): Promise<void> {
    this.active = true;
    this.el.hidden = false;
    this.titleHint = name;
    this.lastRule = { name, rule };
    this.lastRom = null;
    this.displayNames = null;
    this.hooks.onResults();
    this.renderStatus(`Gathering “${name}”…`);
    try {
      let items: Item[] = [];
      for (let pass = 0; pass < 40; pass++) {
        const res = await api.ruleItems(
          rule.extensions,
          rule.minDurationSecs ?? null,
          1000,
          250,
        );
        items = res.items;
        if (res.remaining === 0) break;
        this.renderStatus(
          `Reading media durations for “${name}”… (${items.length} so far)`,
        );
      }
      // Collection scope (ROM systems without a DAT keep extension
      // rules; their scope filters client-side by item location).
      if (scope && scope.mode !== "all") {
        const allowed =
          scope.mode === "boot"
            ? new Set(
                this.hooks
                  .scopes()
                  .filter((l) => l.provider !== "volume")
                  .map((l) => l.id),
              )
            : new Set<string | null>(scope.drives);
        items = items.filter((it) => allowed.has(it.location));
      }
      this.renderItems(name, items);
    } catch {
      this.renderStatus(`Could not gather “${name}”. Please try again.`);
    }
  }

  /** ROM collection (ROM Lists): membership = a DAT identification,
   * never an extension. Runs the identification pass first (cached
   * identifications make repeat opens instant), then renders the
   * collection with DAT names headlining each row. */
  async runRomCollection(
    name: string,
    systemKey: string,
    scope: RomScope,
  ): Promise<void> {
    this.active = true;
    this.el.hidden = false;
    this.titleHint = name;
    this.lastRule = null;
    this.lastRom = { name, systemKey, scope };
    this.displayNames = null;
    this.hooks.onResults();
    this.renderStatus(`Identifying ROMs for “${name}”…`);
    if (!this.romProgressWired) {
      this.romProgressWired = true;
      void onRomIdentifyProgress((p) => {
        if (this.active && this.lastRom) {
          this.renderStatus(
            `Identifying ROMs… ${p.done.toLocaleString()} of ${p.total.toLocaleString()} checked · ${p.matched.toLocaleString()} identified`,
          );
        }
      }).catch(() => undefined);
    }
    try {
      await api.identifyRoms();
      const hits = await api.romCollectionItems(systemKey, scope);
      const names = new Map<string, string>();
      const items: Item[] = [];
      for (const h of hits) {
        items.push(h.item);
        names.set(h.item.id, h.datName);
      }
      this.displayNames = names;
      this.renderItems(name, items);
      if (items.length === 0) {
        const note = document.createElement("p");
        note.className = "search-hint";
        note.textContent =
          "No verified ROMs yet. Membership comes from the No-Intro / Redump / MAME lists — a file joins only when its contents match a known game exactly.";
        this.resultsEl.appendChild(note);
      }
    } catch (err) {
      this.renderStatus(
        `Could not open “${name}”: ${errText(err)}`,
      );
    }
  }

  private openScopeMenu(): void {
    const rect = this.scopeBtn.getBoundingClientRect();
    const options: Array<{ id: string; name: string }> = [
      { id: "all", name: "Everywhere" },
      { id: "folder", name: "Current folder" },
      ...this.hooks.scopes().map((s) => ({ id: s.id ?? "all", name: s.name })),
    ];
    const seen = new Set<string>();
    this.scopeMenu.open(
      rect.left,
      rect.bottom + 6,
      options
        .filter((o) => (seen.has(o.id) ? false : (seen.add(o.id), true)))
        .map((o) => ({
          label: (this.scopeChoice === o.id ? "\u2713 " : "   ") + o.name,
          action: () => {
            this.scopeChoice = o.id;
            this.scopeBtn.textContent = o.name + " \u25be";
            this.lastQuery = "\u0000force";
            void this.run();
          },
        })),
    );
  }

  /** Backend filter args for the current scope/date state. */
  private async filterArgs(): Promise<{
    scope: string | null;
    pathPrefix: string | null;
    modifiedAfter: string | null;
    modifiedBefore: string | null;
  }> {
    return {
      scope:
        this.scopeChoice === "all" || this.scopeChoice === "folder"
          ? null
          : this.scopeChoice,
      pathPrefix:
        this.scopeChoice === "folder"
          ? await this.hooks.currentDirPath()
          : null,
      modifiedAfter: this.afterDate || null,
      modifiedBefore: this.beforeDate || null,
    };
  }

  private renderStatus(text: string): void {
    this.resultsEl.innerHTML = "";
    const p = document.createElement("p");
    p.className = "search-status";
    p.textContent = text;
    this.resultsEl.appendChild(p);
  }

  /** Render a fixed item list (rule folders) with the results grammar. */
  private renderItems(heading: string, items: Item[]): void {
    this.hits = items.map((item) => ({ item, rank: 0 }));
    this.selected.clear();
    this.anchor = -1;
    this.deleteArmed = false;
    this.resultsEl.innerHTML = "";
    this.resultsEl.appendChild(
      this.resultsHeader(
        `${items.length} item${items.length === 1 ? "" : "s"} in “${heading}”${items.length >= 1000 ? " (first 1000)" : ""}.`,
      ),
    );
    const hint = document.createElement("p");
    hint.className = "search-hint";
    hint.textContent =
      "Click selects · Enter opens · Space previews · Backspace / Delete moves to Trash";
    this.resultsEl.appendChild(hint);
    const list = document.createElement("div");
    list.className = "search-results";
    list.setAttribute("role", "listbox");
    this.hits.forEach((hit, i) => {
      list.appendChild(this.buildHitRow(hit, i, false));
    });
    this.resultsEl.appendChild(list);
  }

  private renderLoading(q: string): void {
    this.resultsEl.innerHTML = "";
    const p = document.createElement("p");
    p.className = "search-status";
    p.textContent = `Searching for “${q}”…`;
    this.resultsEl.appendChild(p);
  }

  private renderError(q: string): void {
    this.resultsEl.innerHTML = "";
    const p = document.createElement("p");
    p.className = "search-status";
    p.textContent = `Search failed for “${q}”. Please try again.`;
    this.resultsEl.appendChild(p);
  }

  private renderResults(q: string, hits: SearchHit[]): void {
    this.hits = hits;
    this.lastResults = { q, hits };
    this.selected.clear();
    this.anchor = -1;
    this.resultsEl.innerHTML = "";
    const typeLabel = TYPE_FILTERS.find((f) => f.id === this.typeFilter)?.label;
    this.resultsEl.appendChild(
      this.resultsHeader(
        hits.length === 0
          ? this.indexingBusy
            ? // Inc-2 item 13: never state a final "No results"
              // while the index is still being built.
              q
              ? `No matches yet for “${q}” — still indexing…`
              : `No ${typeLabel ?? "files"} found yet — still indexing…`
            : q
              ? `No results for “${q}”.`
              : `No ${typeLabel ?? "files"} found.`
          : q
            ? `${hits.length} result${hits.length === 1 ? "" : "s"} for “${q}”.`
            : `${hits.length} ${typeLabel ?? "files"}${hits.length === 50 ? " (first 50)" : ""}.`,
      ),
    );
    if (hits.length > 0) {
      const hint = document.createElement("p");
      hint.className = "search-hint";
      hint.textContent =
        "Click selects · Enter opens · Space previews · Backspace / Delete moves to Trash";
      this.resultsEl.appendChild(hint);
    }
    const list = document.createElement("div");
    list.className = "search-results";
    list.setAttribute("role", "listbox");
    hits.forEach((hit, i) => {
      list.appendChild(this.buildHitRow(hit, i, true));
    });
    this.resultsEl.appendChild(list);
  }

  private buildHitRow(
    hit: SearchHit,
    i: number,
    showRank: boolean,
  ): HTMLButtonElement {
      const row = document.createElement("button");
      row.className = "search-hit";
      row.setAttribute("role", "option");
      row.dataset.index = String(i);

      const name = document.createElement("span");
      name.className = "hit-name";
      // ROM collections headline the DAT name; the row's path line
      // still shows the real file path.
      name.textContent =
        this.displayNames?.get(hit.item.id) ?? hit.item.name;

      const path = document.createElement("span");
      path.className = "hit-path";
      path.textContent = hit.item.provider_key ?? hit.item.id;

      const meta = document.createElement("span");
      meta.className = "hit-meta";
      meta.textContent = showRank
        ? `rank ${hit.rank.toFixed(2)} · ${hit.item.kind === "folder" ? "Folder" : formatBytes(hit.item.size_bytes)}`
        : hit.item.kind === "folder"
          ? "Folder"
          : formatBytes(hit.item.size_bytes);

      row.append(name, path, meta);
      row.addEventListener("mousedown", (e) => {
      if (e.button !== 0) return;
      this.selectHit(i, e);
        row.focus();
      });
      row.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        if (!this.selected.has(SearchView.keyOf(hit.item))) {
          this.selected.clear();
          this.selected.add(SearchView.keyOf(hit.item));
          this.anchor = i;
          this.applySelection();
        }
        this.deleteArmed = true;
        row.focus();
        this.hooks.onContextMenu?.(
          hit.item,
          this.selectedItems(),
          e.clientX,
          e.clientY,
        );
      });
      row.addEventListener("dblclick", () => this.activate(hit.item));
      row.addEventListener("keydown", (e) => {
        if (e.key === "Enter") {
          this.activate(hit.item);
        } else if (e.key === " ") {
          e.preventDefault();
          this.hooks.onPreviewItem(hit.item);
        } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
          e.preventDefault();
          this.focusHit(i + (e.key === "ArrowDown" ? 1 : -1));
        }
      });
      return row;
    }
}
