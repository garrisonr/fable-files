/**
 * Instant search: debounced ~120ms, results replace the file view.
 * Shows name / path / rank; activating a hit navigates into it (folders)
 * or opens Quick Look (files). SearchHit carries no parent-dir field, so
 * "reveal in folder" is impossible until the backend adds one — click opens
 * the item itself, which is the honest subset for this milestone.
 */
import { api, formatBytes, type Item, type SearchHit } from "./api";

export interface SearchHooks {
  onNavigateToItem(item: Item): void;
  onPreviewItem(item: Item): void;
  /** Results are showing: the shell should hide the file view. */
  onResults(): void;
  onClear(): void;
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
  private timer = 0;
  private lastQuery = "";
  active = false;

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
    this.el.appendChild(this.chipsEl);

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
    this.fieldEl.appendChild(this.input);
    // Clicking anywhere on the pill focuses the input.
    this.fieldEl.addEventListener("mousedown", (e) => {
      if (e.target !== this.input) {
        e.preventDefault();
        this.input.focus();
      }
    });
  }

  get field(): HTMLElement {
    return this.fieldEl;
  }

  focus(): void {
    this.input.focus();
    this.input.select();
  }

  clear(): void {
    window.clearTimeout(this.timer);
    this.input.value = "";
    this.lastQuery = "";
    this.active = false;
    this.el.hidden = true;
    this.resultsEl.innerHTML = "";
    this.hooks.onClear();
  }

  private onInput(): void {
    window.clearTimeout(this.timer);
    this.timer = window.setTimeout(() => void this.run(), DEBOUNCE_MS);
  }

  private async run(): Promise<void> {
    const q = this.input.value.trim();
    if (q === this.lastQuery) return;
    this.lastQuery = q;
    if (!q && !this.typeFilter) {
      this.clear();
      return;
    }
    this.active = true;
    this.el.hidden = false;
    this.hooks.onResults();
    this.renderLoading(q);
    try {
      const hits = await api.search(q, null, 50, this.typeFilter);
      if (this.lastQuery !== q) return; // superseded
      this.renderResults(q, hits);
    } catch {
      if (this.lastQuery !== q) return;
      this.renderError(q);
    }
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
    this.resultsEl.innerHTML = "";
    const head = document.createElement("p");
    head.className = "search-status";
    const typeLabel = TYPE_FILTERS.find((f) => f.id === this.typeFilter)?.label;
    head.textContent =
      hits.length === 0
        ? q
          ? `No results for “${q}”.`
          : `No ${typeLabel ?? "files"} found.`
        : q
          ? `${hits.length} result${hits.length === 1 ? "" : "s"} for “${q}”.`
          : `${hits.length} ${typeLabel ?? "files"}${hits.length === 50 ? " (first 50)" : ""}.`;
    this.resultsEl.appendChild(head);
    const list = document.createElement("div");
    list.className = "search-results";
    list.setAttribute("role", "listbox");
    hits.forEach((hit, i) => {
      const row = document.createElement("button");
      row.className = "search-hit";
      row.setAttribute("role", "option");
      row.dataset.index = String(i);

      const name = document.createElement("span");
      name.className = "hit-name";
      name.textContent = hit.item.name;

      const path = document.createElement("span");
      path.className = "hit-path";
      path.textContent = hit.item.provider_key ?? hit.item.id;

      const meta = document.createElement("span");
      meta.className = "hit-meta";
      meta.textContent = `rank ${hit.rank.toFixed(2)} · ${
        hit.item.kind === "folder" ? "Folder" : formatBytes(hit.item.size_bytes)
      }`;

      row.append(name, path, meta);
      row.addEventListener("click", () => {
        if (hit.item.kind === "folder") this.hooks.onNavigateToItem(hit.item);
        else this.hooks.onPreviewItem(hit.item);
      });
      row.addEventListener("keydown", (e) => {
        if (e.key === "Enter") {
          if (hit.item.kind === "folder") this.hooks.onNavigateToItem(hit.item);
          else this.hooks.onPreviewItem(hit.item);
        }
      });
      list.appendChild(row);
    });
    this.resultsEl.appendChild(list);
  }
}
