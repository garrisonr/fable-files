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

export class SearchView {
  readonly el: HTMLElement;
  private input: HTMLInputElement;
  private timer = 0;
  private lastQuery = "";
  active = false;

  constructor(private hooks: SearchHooks) {
    this.el = document.createElement("div");
    this.el.className = "searchview";
    this.el.hidden = true;

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
  }

  get field(): HTMLInputElement {
    return this.input;
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
    this.el.innerHTML = "";
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
    if (!q) {
      this.clear();
      return;
    }
    this.active = true;
    this.el.hidden = false;
    this.hooks.onResults();
    this.renderLoading(q);
    try {
      const hits = await api.search(q, null, 50);
      if (this.lastQuery !== q) return; // superseded
      this.renderResults(q, hits);
    } catch {
      if (this.lastQuery !== q) return;
      this.renderError(q);
    }
  }

  private renderLoading(q: string): void {
    this.el.innerHTML = "";
    const p = document.createElement("p");
    p.className = "search-status";
    p.textContent = `Searching for “${q}”…`;
    this.el.appendChild(p);
  }

  private renderError(q: string): void {
    this.el.innerHTML = "";
    const p = document.createElement("p");
    p.className = "search-status";
    p.textContent = `Search failed for “${q}”. The search backend is not wired yet.`;
    this.el.appendChild(p);
  }

  private renderResults(q: string, hits: SearchHit[]): void {
    this.el.innerHTML = "";
    const head = document.createElement("p");
    head.className = "search-status";
    head.textContent =
      hits.length === 0
        ? `No results for “${q}”.`
        : `${hits.length} result${hits.length === 1 ? "" : "s"} for “${q}”.`;
    this.el.appendChild(head);
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
    this.el.appendChild(list);
  }
}
