/**
 * Instant search: debounced ~120ms, results replace the file view.
 * Shows name / path / rank; activating a hit navigates into it (folders)
 * or opens Quick Look (files). SearchHit carries no parent-dir field, so
 * "reveal in folder" is impossible until the backend adds one — click opens
 * the item itself, which is the honest subset for this milestone.
 */
import {
  api,
  fileSrc,
  formatBytes,
  onRomIdentifyProgress,
  type Item,
  type ItemVolumeBadge,
  type RuleItemsResult,
  type RomScope,
  type SearchHit,
} from "./api";
import { ContextMenu } from "./contextmenu";
import { errText } from "./toast";
import { isPlayableName } from "./video";
import { openRomMediaReport, startMediaDownload } from "./rommedia";
import { romArchiveLink } from "./roms";
import { openGuideLink } from "./guidelinks";

export interface SearchHooks {
  onNavigateToItem(item: Item): void;
  /** Absolute path of the folder the file view shows (folder scope). */
  currentDirPath(): string | null | Promise<string | null>;
  /** Locations for the scope picker (null id = everywhere). */
  scopes(): Array<{ id: string | null; name: string; provider?: string }>;
  onPreviewItem(item: Item): void;
  /** Selection changed (attachment rules, Dickles 2026-10-15):
   * the single selected hit's item, or null when the selection
   * is empty or multi — so an open Quick Look follows clicks
   * and arrow keys in results exactly like in the file views. */
  onSelectItem?(item: Item | null): void;
  /** Trash the selected hits (shell owns the batch + toast + undo). */
  onTrashItems?(items: Item[]): void;
  /** Right-click on a hit: the shell shows the full item menu. */
  onContextMenu?(item: Item, targets: Item[], x: number, y: number): void;
  /** Results are showing: the shell should hide the file view. */
  onResults(): void;
  onClear(): void;
  /** Double-click on a playable hit (fix batch item 7): pop it
   * out into the player window instead of previewing. */
  onPopOutItem?(item: Item): void;
  /** A hit on a disconnected drive was activated (Indexer v2):
   * the shell shows the plain-language "that drive isn't
   * connected" state instead of a dead click. */
  onOfflineItem?(hit: SearchHit): void;
  /** "Re-index" button in the results header (Round 7.1). */
  onReindex?(): void;
  /** Save the current query + filter as a smart folder. */
  onSaveSmartFolder?(): void;
}

const DEBOUNCE_MS = 120;

/** "Oct 9, 2026" from an RFC 3339 timestamp (badges, toasts). */
export function fmtDay(iso: string | null | undefined): string {
  if (!iso) return "a while ago";
  const d = new Date(iso);
  return Number.isNaN(d.getTime())
    ? "a while ago"
    : d.toLocaleDateString("en-US", {
        month: "short",
        day: "numeric",
        year: "numeric",
      });
}

const SEARCH_ICON_SVG = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><circle cx="11" cy="11" r="7"/><path d="m20 20-3.8-3.8"/></svg>`;

const TYPE_FILTERS: Array<{ id: string | null; label: string }> = [
  { id: null, label: "All types" },
  { id: "image", label: "Images" },
  { id: "video", label: "Videos" },
  { id: "audio", label: "Audio" },
  { id: "document", label: "Documents" },
  // UI round (Dickles: "I'm old"): email is a first-class type —
  // .eml / .msg / .mbox, classified by the backend type_extensions.
  { id: "email", label: "Email" },
  { id: "archive", label: "Archives" },
];

/** Size facet (UI round item 3c): applied client-side to search
 * hits — the backend search contract has no size parameter, and a
 * hit's size_bytes is right there. Folders always pass: a folder's
 * size is its contents, not itself. */
interface SizeFacet {
  id: string;
  label: string;
  min: number | null;
  max: number | null;
}
const SIZE_FACETS: SizeFacet[] = [
  { id: "small", label: "Under 1 MB", min: null, max: 1_000_000 },
  { id: "medium", label: "1 – 100 MB", min: 1_000_000, max: 100_000_000 },
  { id: "large", label: "100 MB – 1 GB", min: 100_000_000, max: 1_000_000_000 },
  { id: "huge", label: "Over 1 GB", min: 1_000_000_000, max: null },
];

export class SearchView {
  readonly el: HTMLElement;
  private input: HTMLInputElement;
  private fieldEl: HTMLElement;
  private resultsEl: HTMLElement;
  private chipsEl: HTMLElement;
  private typeFilter: string | null = null;
  /** Facet popover (UI round item 3c): button + panel + size facet. */
  private facetBtn!: HTMLButtonElement;
  private facetEl!: HTMLDivElement;
  private sizeFacet: SizeFacet | null = null;
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
  /** Placeholder system currently shown, if any (Xbox Series /
   * Switch 2): no collection, no identification — just the shelf
   * card. Tracked so a re-index doesn't flip the view away. */
  private lastPlaceholder: string | null = null;
  /** Display-name overrides for the current rule/collection render:
   * item id → DAT name (ROM Lists: the DAT name headlines, the real
   * filename stays in the path line). */
  private displayNames: Map<string, string> | null = null;
  private romProgressWired = false;
  /** Set by runExternal so run() can tell a smart-folder invocation
   * (keep titleHint) from a hand-typed search (clear titleHint). */
  private externalRun = false;
  /** Token for runRule's background duration sweep: bumped by every
   * new rule run and by clear(), so a sweep loop that wakes after
   * the user moved on stops instead of painting over the new view. */
  private ruleRunToken = 0;
  /** The "still reading durations" note under a rule view's list:
   * sweep progress rides UNDER the results, never instead of them. */
  private ruleNoteEl: HTMLParagraphElement | null = null;
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
  /** View cache (fix batch item 2 — the view caching law): a
   * view's result set survives navigation. Switching back PAINTS
   * the cache — no query, no duration re-probe, scroll restored.
   * Entries die only when the DATA changes: the shell drops the
   * whole cache on mutations and job completions, and a finished
   * re-index drops it here. */
  private viewCache = new Map<
    string,
    {
      items: Item[] | null;
      hits: SearchHit[] | null;
      displayNames: Map<string, string> | null;
      scrollTop: number;
      /** Library Memory badges for `items` (rule views). */
      badges?: ItemVolumeBadge[] | null;
    }
  >();
  /** Cache key of the view currently on screen (scroll is saved
   * under it whenever another view takes over). */
  private currentViewKey: string | null = null;

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
        this.setTypeFilter(f.id);
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
      this.refreshFacetBtn();
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
      this.refreshFacetBtn();
      this.lastQuery = "\u0000force";
      void this.run();
    });
    beforeWrap.appendChild(this.beforeInput);
    this.filtersEl.append(scopeWrap, afterWrap, beforeWrap);
    this.el.appendChild(this.filtersEl);

    // Facet popover (UI round item 3c): kind / date / size facets in
    // one surface, Zorin-style. The kind facet mirrors the chips
    // above (one source of truth: setTypeFilter); the date presets
    // drive the same range the two date fields edit; size filters
    // the returned hits client-side. The button carries the active
    // facet count so a filtered search never looks unfiltered.
    this.facetBtn = document.createElement("button");
    this.facetBtn.className = "search-chip search-facets-btn";
    this.facetBtn.textContent = "Filters";
    this.facetBtn.title = "Filter by kind, date and size";
    this.facetBtn.addEventListener("click", () => this.toggleFacets());
    this.chipsEl.appendChild(this.facetBtn);
    this.facetEl = document.createElement("div");
    this.facetEl.className = "search-facets";
    this.facetEl.hidden = true;
    this.el.appendChild(this.facetEl);

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
    this.setTypeFilter(typeFilter);
    this.lastQuery = "\u0000force";
    await this.run();
  }

  /** Kind facet: one source of truth for the chips row and the
   * facet popover, so the two surfaces can never disagree. Only
   * chips that carry a data-filter participate (the Save / Filters
   * chips keep their own states). */
  private setTypeFilter(id: string | null): void {
    this.typeFilter = id;
    this.chipsEl.querySelectorAll<HTMLElement>(".search-chip").forEach((c) => {
      if (c.dataset.filter === undefined) return;
      c.classList.toggle("active", c.dataset.filter === (id ?? ""));
    });
    this.refreshFacetBtn();
  }

  /** The Filters button label carries the active facet count. */
  private refreshFacetBtn(): void {
    if (!this.facetBtn) return;
    let n = 0;
    if (this.typeFilter) n++;
    if (this.afterDate || this.beforeDate) n++;
    if (this.sizeFacet) n++;
    this.facetBtn.textContent = n > 0 ? `Filters · ${n}` : "Filters";
    this.facetBtn.classList.toggle("active", n > 0);
  }

  private toggleFacets(): void {
    if (this.facetEl.hidden) {
      this.buildFacets();
      this.facetEl.hidden = false;
    } else {
      this.facetEl.hidden = true;
    }
  }

  private buildFacets(): void {
    const el = this.facetEl;
    el.innerHTML = "";
    const group = (title: string): HTMLDivElement => {
      const g = document.createElement("div");
      g.className = "search-facet-group";
      const t = document.createElement("div");
      t.className = "search-facet-title";
      t.textContent = title;
      g.appendChild(t);
      el.appendChild(g);
      return g;
    };
    const opt = (
      g: HTMLDivElement,
      label: string,
      active: boolean,
      fn: () => void,
    ): void => {
      const b = document.createElement("button");
      b.className = "search-facet-opt" + (active ? " active" : "");
      b.textContent = label;
      b.addEventListener("click", fn);
      g.appendChild(b);
    };
    const kind = group("Kind");
    for (const f of TYPE_FILTERS) {
      opt(kind, f.label, this.typeFilter === f.id, () => {
        this.setTypeFilter(f.id);
        this.lastQuery = "\u0000force";
        void this.run();
        this.buildFacets();
      });
    }
    const dates = group("Modified");
    for (const p of this.datePresets()) {
      opt(
        dates,
        p.label,
        this.afterDate === p.after && this.beforeDate === p.before,
        () => {
          this.applyDatePreset(p.after, p.before);
          this.buildFacets();
        },
      );
    }
    const hint = document.createElement("div");
    hint.className = "search-facet-hint";
    hint.textContent = "Custom range: the date fields above.";
    dates.appendChild(hint);
    const sizes = group("Size");
    opt(sizes, "Any size", this.sizeFacet === null, () => {
      this.sizeFacet = null;
      this.refreshFacetBtn();
      this.rerenderLast();
      this.buildFacets();
    });
    for (const s of SIZE_FACETS) {
      opt(sizes, s.label, this.sizeFacet?.id === s.id, () => {
        this.sizeFacet = s;
        this.refreshFacetBtn();
        this.rerenderLast();
        this.buildFacets();
      });
    }
    const done = document.createElement("button");
    done.className = "search-facet-opt search-facet-done";
    done.textContent = "Done";
    done.addEventListener("click", () => {
      this.facetEl.hidden = true;
    });
    el.appendChild(done);
  }

  private datePresets(): Array<{ label: string; after: string; before: string }> {
    const p2 = (n: number) => String(n).padStart(2, "0");
    const fmt = (d: Date): string =>
      `${d.getFullYear()}-${p2(d.getMonth() + 1)}-${p2(d.getDate())}`;
    const today = new Date();
    const daysAgo = (n: number): string => {
      const d = new Date(today);
      d.setDate(d.getDate() - n);
      return fmt(d);
    };
    return [
      { label: "Any time", after: "", before: "" },
      { label: "Today", after: fmt(today), before: "" },
      { label: "Last 7 days", after: daysAgo(6), before: "" },
      { label: "Last 30 days", after: daysAgo(29), before: "" },
      { label: "This year", after: `${today.getFullYear()}-01-01`, before: "" },
    ];
  }

  /** A date preset drives the SAME range state the date fields
   * edit (and the same backend modified-after/before arguments). */
  private applyDatePreset(after: string, before: string): void {
    this.afterDate = after;
    this.beforeDate = before;
    this.afterInput.value = after;
    this.beforeInput.value = before;
    this.refreshFacetBtn();
    this.lastQuery = "\u0000force";
    void this.run();
  }

  private sizeMatches(item: Item): boolean {
    if (!this.sizeFacet) return true;
    if (item.kind === "folder") return true;
    const s = item.size_bytes;
    if (s === null || s === undefined) return false;
    if (this.sizeFacet.min !== null && s < this.sizeFacet.min) return false;
    if (this.sizeFacet.max !== null && s >= this.sizeFacet.max) return false;
    return true;
  }

  /** Re-render the last typed-search results under the current
   * size facet — no new backend round-trip. lastResults always
   * holds the RAW hits; the facet filters at render time. */
  private rerenderLast(): void {
    if (this.lastResults) {
      this.renderResults(this.lastResults.q, this.lastResults.hits);
    }
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
    this.emitSelection();
  }

  /** Where the selection landed, for the shell's Quick Look
   * follow (see SearchHooks.onSelectItem). Mouse (selectHit),
   * keyboard (focusHit), and the right-click re-select all
   * report through here — one door, every gesture. */
  private emitSelection(): void {
    if (!this.hooks.onSelectItem) return;
    if (this.selected.size !== 1) {
      this.hooks.onSelectItem(null);
      return;
    }
    const key = [...this.selected][0];
    const hit = this.hits.find((h) => SearchView.keyOf(h.item) === key);
    this.hooks.onSelectItem(hit ? hit.item : null);
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

  /** Activate a hit (Enter / double-click / Space): a hit on a
   *  disconnected drive gets the friendly not-connected state —
   *  never a dead click, a failed preview, or a broken pop-out. */
  private activateHit(hit: SearchHit): void {
    if (hit.volume_offline) {
      this.hooks.onOfflineItem?.(hit);
      return;
    }
    const item = hit.item;
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
      this.emitSelection();
    }
  }

  focus(): void {
    this.input.focus();
    this.input.select();
  }

  clear(): void {
    this.saveScroll();
    window.clearTimeout(this.timer);
    this.input.value = "";
    this.lastQuery = "";
    this.setTypeFilter(null);
    this.sizeFacet = null;
    this.refreshFacetBtn();
    this.scopeChoice = "all";
    this.scopeBtn.textContent = "Everywhere \u25be";
    this.afterDate = "";
    this.beforeDate = "";
    this.afterInput.value = "";
    this.beforeInput.value = "";
    this.ruleRunToken++;
    void api.ruleCancelProbe().catch(() => undefined);
    this.active = false;
    this.titleHint = null;
    this.lastResults = null;
    this.el.hidden = true;
    this.resultsEl.innerHTML = "";
    this.hooks.onClear();
  }

  /** Cache key fragment for a collection scope. */
  private scopeKey(scope?: RomScope): string {
    if (!scope) return "all";
    return scope.mode === "drives"
      ? `drives:${[...scope.drives].sort().join(",")}`
      : scope.mode;
  }

  /** Remember how far the CURRENT view is scrolled before
   * another view (or the file view) takes the screen. */
  private saveScroll(): void {
    if (!this.currentViewKey) return;
    const e = this.viewCache.get(this.currentViewKey);
    if (e) e.scrollTop = this.el.scrollTop;
  }

  /** Restore a freshly painted view's scroll position. */
  private restoreScroll(key: string): void {
    const e = this.viewCache.get(key);
    if (!e || e.scrollTop <= 0) return;
    const top = e.scrollTop;
    requestAnimationFrame(() => {
      this.el.scrollTop = top;
    });
  }

  /** Data changed (fix batch item 2): every cached view is
   * stale. Called by the shell on mutations and job completions;
   * the next visit to each view re-gathers it. Navigation alone
   * NEVER calls this. */
  invalidateViewCache(): void {
    this.viewCache.clear();
  }

  /** A library re-index just finished: refresh what the user is
   * looking at so newly downloaded files appear without a restart. */
  rerunAfterIndex(): void {
    this.viewCache.clear();
    if (!this.active) return;
    if (this.lastPlaceholder) {
      void this.runRomPlaceholder(this.lastPlaceholder);
    } else if (this.lastRom) {
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
    try {
      const fa = await this.filterArgs();
      // View cache (fix batch item 2): the same query + facets
      // paints its last result set instantly — no round-trip.
      const key = `q|${q}|${this.typeFilter ?? ""}|${this.scopeChoice}|${fa.pathPrefix ?? ""}|${this.afterDate}|${this.beforeDate}|${this.sizeFacet?.id ?? ""}`;
      this.saveScroll();
      const cached = this.viewCache.get(key);
      this.currentViewKey = key;
      if (cached?.hits) {
        this.renderResults(q, cached.hits);
        this.restoreScroll(key);
        return;
      }
      this.renderLoading(q);
      const hits = await api.search(q, fa.scope, 50, this.typeFilter, fa);
      if (this.lastQuery !== q) return; // superseded
      this.viewCache.set(key, {
        items: null,
        hits,
        displayNames: null,
        scrollTop: 0,
      });
      this.renderResults(q, hits);
    } catch {
      if (this.lastQuery !== q) return;
      this.renderError(q);
    }
  }

  /** Show (or clear) the sweep-progress note under the results. */
  private setRuleNote(text: string | null): void {
    if (this.ruleNoteEl) {
      this.ruleNoteEl.remove();
      this.ruleNoteEl = null;
    }
    if (!text) return;
    const p = document.createElement("p");
    p.className = "search-hint";
    p.textContent = text;
    this.resultsEl.appendChild(p);
    this.ruleNoteEl = p;
  }

  /** Collection scope filter for rule views (ROM systems without a
   * DAT keep extension rules; their scope filters client-side by
   * item location). */
  private scopeRuleItems(items: Item[], scope?: RomScope): Item[] {
    if (!scope || scope.mode === "all") return items;
    const allowed =
      scope.mode === "boot"
        ? new Set(
            this.hooks
              .scopes()
              .filter((l) => l.provider !== "volume")
              .map((l) => l.id),
          )
        : new Set<string | null>(scope.drives);
    return items.filter((it) => allowed.has(it.location));
  }

  /** Evaluate a rule smart folder (extensions + optional minimum
   * duration). The backend's rule store remembers the result:
   * rule_items paints the remembered members instantly and NEVER
   * probes; a background delta refresh reconciles new / changed /
   * removed files; the duration sweep then runs in probe batches
   * BEHIND the visible list — it can only add members, never gate
   * them. Leaving the view cancels the sweep; everything probed so
   * far persists in the catalog, so the next visit resumes where
   * this one stopped instead of starting from scratch. */
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
    this.lastPlaceholder = null;
    this.displayNames = null;
    this.hooks.onResults();
    // View cache (fix batch item 2): re-entering a smart folder
    // paints its last result set instantly — the duration probe
    // runs in the background, not per visit.
    const key = `rule|${name}|${rule.extensions.join(",")}|${rule.minDurationSecs ?? ""}|${this.scopeKey(scope)}`;
    this.saveScroll();
    const cached = this.viewCache.get(key);
    this.currentViewKey = key;
    if (cached?.items) {
      this.renderItems(name, cached.items, cached.badges ?? undefined);
      this.restoreScroll(key);
      return;
    }
    const token = ++this.ruleRunToken;
    const stale = () =>
      token !== this.ruleRunToken ||
      !this.active ||
      this.currentViewKey !== key;
    this.renderStatus(`Gathering \u201c${name}\u201d\u2026`);
    try {
      const exts = rule.extensions;
      const minDur = rule.minDurationSecs ?? null;
      let current = await api.ruleItems(exts, minDur, 1000, 0);
      if (stale()) return;
      // Paint a stored result, preserving scroll across the
      // streaming re-renders below.
      const paint = (res: RuleItemsResult): Item[] => {
        const items = this.scopeRuleItems(res.items, scope);
        const scroll = this.el.scrollTop;
        this.viewCache.set(key, {
          items,
          hits: null,
          displayNames: null,
          scrollTop: 0,
          badges: res.badges ?? [],
        });
        this.renderItems(name, items, res.badges ?? undefined);
        this.el.scrollTop = scroll;
        return items;
      };
      let items = paint(current);
      // Background reconciliation: deltas since the remembered
      // evaluation apply without the paint above ever waiting.
      const fresh = await api.ruleRefresh(exts, minDur, 1000).catch(() => null);
      if (stale()) return;
      if (fresh) {
        const before = current.items
          .map((i) => i.provider_key ?? i.id)
          .join("|");
        const after = fresh.items.map((i) => i.provider_key ?? i.id).join("|");
        current = fresh;
        if (before !== after) items = paint(fresh);
      }
      // The duration sweep: batches behind the visible list.
      let pending = current.remaining;
      let offline = current.pending_offline;
      const offlineNote =
        "Some files sit on drives that aren\u2019t connected right now \u2014 they\u2019ll join when the drive returns.";
      if (pending === 0 && offline > 0) this.setRuleNote(offlineNote);
      while (pending > 0) {
        this.setRuleNote(
          `Reading media durations for \u201c${name}\u201d\u2026 (${pending.toLocaleString()} to go) \u2014 results so far are live.`,
        );
        const batch = await api.ruleProbe(exts, minDur, 250);
        if (stale()) {
          void api.ruleCancelProbe().catch(() => undefined);
          return;
        }
        if (batch.probeError) {
          this.setRuleNote(`Duration reading paused \u2014 ${batch.probeError}`);
          return;
        }
        pending = batch.remaining;
        offline = batch.remainingOffline;
        if (batch.newMembers.length > 0) {
          const have = new Set(items.map((i) => i.id));
          const freshItems = this.scopeRuleItems(
            batch.newMembers,
            scope,
          ).filter((i) => !have.has(i.id));
          if (freshItems.length > 0) {
            items = items.concat(freshItems);
            const badges = [
              ...(this.viewCache.get(key)?.badges ?? []),
              ...(batch.badges ?? []),
            ];
            const scroll = this.el.scrollTop;
            this.viewCache.set(key, {
              items,
              hits: null,
              displayNames: null,
              scrollTop: 0,
              badges,
            });
            this.renderItems(name, items, badges);
            this.el.scrollTop = scroll;
          }
        }
        if (batch.cancelled || batch.probed === 0) break;
      }
      if (pending === 0) {
        this.setRuleNote(offline > 0 ? offlineNote : null);
      }
    } catch {
      if (token !== this.ruleRunToken) return;
      this.renderStatus(`Could not gather \u201c${name}\u201d. Please try again.`);
    }
  }

  /** ROM collection (ROM Lists): membership = a DAT identification,
   * never an extension. Runs the identification pass first (cached
   * identifications make repeat opens instant), then renders the
   * collection with DAT names headlining each row. */
  /** Placeholder system view (Dickles, 2026-10-15): the shelf
   * exists, the system doesn't — yet. No gathering, no media bar,
   * no identification promises; just the placeholder card. */
  async runRomPlaceholder(name: string): Promise<void> {
    this.active = true;
    this.el.hidden = false;
    this.titleHint = name;
    this.lastRule = null;
    this.lastRom = null;
    this.lastPlaceholder = name;
    this.displayNames = null;
    this.hooks.onResults();
    this.currentViewKey = `rom-placeholder|${name}`;
    this.resultsEl.innerHTML = "";
    this.resultsEl.appendChild(
      this.resultsHeader(`“${name}” — a shelf in waiting`),
    );
    const p = document.createElement("p");
    p.className = "search-hint";
    p.textContent =
      "A placeholder for what's coming. There's no emulator core or verified game list for this system yet — when they arrive, this shelf wakes up.";
    this.resultsEl.appendChild(p);
  }

  async runRomCollection(
    name: string,
    systemKey: string,
    scope: RomScope,
  ): Promise<void> {
    this.active = true;
    this.el.hidden = false;
    this.titleHint = name;
    this.lastRule = null;
    this.lastPlaceholder = null;
    this.lastRom = { name, systemKey, scope };
    this.displayNames = null;
    this.hooks.onResults();
    // View cache (fix batch item 2): a re-visit paints the
    // collection instantly; identifications are cached backend-
    // side too, but the gather itself is not free.
    const key = `rom|${systemKey}|${this.scopeKey(scope)}`;
    this.saveScroll();
    const cached = this.viewCache.get(key);
    this.currentViewKey = key;
    if (cached?.items) {
      this.displayNames = cached.displayNames;
      this.renderItems(name, cached.items);
      this.mountRomMediaBar(name, systemKey);
      this.restoreScroll(key);
      return;
    }
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
      this.viewCache.set(key, {
        items,
        hits: null,
        displayNames: names,
        scrollTop: 0,
      });
      this.renderItems(name, items);
      this.mountRomMediaBar(name, systemKey);
      if (items.length === 0) {
        // An empty shelf should POINT somewhere (Dickles,
        // 2026-10-15): the honest membership line, then a real
        // link to the system's Internet Archive shelf, then the
        // standing legal routes in one breath. Warm and brief.
        const help = document.createElement("div");
        help.className = "rom-empty-help";
        const note = document.createElement("p");
        note.className = "search-hint";
        note.textContent =
          "No verified ROMs yet. Membership comes from the No-Intro / Redump / MAME lists — a file joins only when its contents match a known game exactly.";
        help.appendChild(note);
        const pointer = document.createElement("p");
        pointer.className = "search-hint";
        pointer.append("Looking to fill this shelf? ");
        const shelf = romArchiveLink(systemKey);
        const link = document.createElement("a");
        link.className = "guide-link";
        link.href = shelf.url;
        link.textContent = shelf.perSystem
          ? `The Internet Archive keeps a ${name} collection`
          : "The Internet Archive's Software Library";
        link.addEventListener("click", (e) => {
          e.preventDefault();
          openGuideLink(shelf.url);
        });
        pointer.appendChild(link);
        pointer.append(
          " — and the classic routes never close: dump cartridges you own, homebrew, and public-domain releases are all fair game.",
        );
        help.appendChild(pointer);
        this.resultsEl.appendChild(help);
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

  /** ROM media enrichment, in the collection view: a "get media"
   * bar under the results, and box art leading every row that has
   * it (art-forward collections) with a quiet glyph where none has
   * landed yet. The art pass is async and re-checks the view key so
   * a stale answer never paints into a different view. */
  private mountRomMediaBar(name: string, systemKey: string): void {
    const bar = document.createElement("div");
    bar.className = "rm-collection-bar";
    const get = document.createElement("button");
    get.className = "pill-btn primary";
    get.textContent = "Get missing media";
    get.addEventListener("click", () => {
      void startMediaDownload(systemKey, name);
    });
    const rep = document.createElement("button");
    rep.className = "pill-btn";
    rep.textContent = "Media report…";
    rep.addEventListener("click", () => openRomMediaReport(systemKey));
    bar.append(get, rep);
    this.resultsEl.appendChild(bar);
    const key = this.currentViewKey;
    const ids = this.hits.map((h) => h.item.id);
    if (ids.length === 0) return;
    void (async () => {
      let art: Record<string, string>;
      try {
        art = await api.romMediaArt(ids);
      } catch {
        return; // art is decoration; its absence stays silent
      }
      if (this.currentViewKey !== key || !this.active) return;
      const rows =
        this.resultsEl.querySelectorAll<HTMLElement>(".search-hit");
      rows.forEach((rowEl) => {
        const idx = Number(rowEl.dataset.index);
        const hit = this.hits[idx];
        if (!hit) return;
        const path = art[hit.item.id];
        if (path) {
          const img = document.createElement("img");
          img.className = "hit-art";
          img.src = fileSrc(path);
          img.alt = "";
          img.loading = "lazy";
          rowEl.prepend(img);
          rowEl.classList.add("has-art");
        } else {
          const glyph = document.createElement("span");
          glyph.className = "hit-art hit-art-glyph";
          glyph.textContent = "\u{1F3AE}";
          rowEl.prepend(glyph);
        }
      });
    })();
  }

  private renderStatus(text: string): void {
    this.resultsEl.innerHTML = "";
    const p = document.createElement("p");
    p.className = "search-status";
    p.textContent = text;
    this.resultsEl.appendChild(p);
  }

  /** Render a fixed item list (rule folders) with the results grammar. */
  private renderItems(
    heading: string,
    items: Item[],
    badges?: ItemVolumeBadge[],
  ): void {
    const badgeById = new Map((badges ?? []).map((b) => [b.item_id, b]));
    this.hits = items.map((item) => {
      const b = badgeById.get(item.id);
      return {
        item,
        rank: 0,
        volume_label: b?.volume_label ?? null,
        volume_offline: b?.offline ?? false,
        volume_last_seen: b?.last_seen ?? null,
      };
    });
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

  private renderResults(q: string, rawHits: SearchHit[]): void {
    // lastResults keeps the RAW hits — the size facet filters at
    // render time, so changing it re-renders without a new search.
    this.lastResults = { q, hits: rawHits };
    const rawCount = rawHits.length;
    const hits = this.sizeFacet
      ? rawHits.filter((h) => this.sizeMatches(h.item))
      : rawHits;
    this.hits = hits;
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
    if (rawCount !== hits.length) {
      const note = document.createElement("p");
      note.className = "search-hint";
      note.textContent = `Size filter on — ${hits.length} of ${rawCount} results shown.`;
      this.resultsEl.appendChild(note);
    }
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
      // Phase 2: starred hits carry their ★ (same accent as lists).
      if (hit.item.starred) {
        const star = document.createElement("span");
        star.className = "hit-star";
        star.textContent = "★";
        star.title = "Starred";
        name.prepend(star);
      }

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

      // Library Memory badge: this file lives on a drive that
      // isn't connected right now. It stays in the results — the
      // badge (an inline pill in the meta cell, family material)
      // says where it lives and when it was last here.
      if (hit.volume_offline) {
        row.classList.add("offline");
        const badge = document.createElement("span");
        badge.className = "hit-badge";
        badge.textContent = `On ${hit.volume_label ?? "a drive"} · last seen ${fmtDay(hit.volume_last_seen)}`;
        meta.appendChild(badge);
      }
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
          this.emitSelection();
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
      row.addEventListener("dblclick", () => {
        // Double-click law (fix batch item 7): a playable file
        // pops out into the player window, playing. Everything
        // else keeps activate() — folders navigate, files preview.
        // Offline hits never reach either path (activateHit).
        if (
          !hit.volume_offline &&
          hit.item.kind !== "folder" &&
          this.hooks.onPopOutItem &&
          isPlayableName(hit.item.name)
        ) {
          this.hooks.onPopOutItem(hit.item);
        } else {
          this.activateHit(hit);
        }
      });
      row.addEventListener("keydown", (e) => {
        if (e.key === "Enter") {
          this.activateHit(hit);
        } else if (e.key === " ") {
          e.preventDefault();
          if (hit.volume_offline) this.hooks.onOfflineItem?.(hit);
          else this.hooks.onPreviewItem(hit.item);
        } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
          e.preventDefault();
          this.focusHit(i + (e.key === "ArrowDown" ? 1 : -1));
        }
      });
      return row;
    }
}
