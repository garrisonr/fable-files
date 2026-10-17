/**
 * ColumnsView: Miller columns. Each column is one directory; selecting a
 * folder opens its contents in the next column, selecting a file shows a
 * preview card in a final pane. Columns scroll horizontally; the newest
 * column is always scrolled into view.
 */
import { api, formatBytes, formatDate, type Item } from "./api";
import { sortedItems, type Tab } from "./tabs";
import { ContextMenu } from "./contextmenu";
import { OpenWithDialog } from "./openwith";
import {
  alternativeToUrl,
  showItemMenu,
  type FileViewHooks,
} from "./fileview";

interface Column {
  locationId: string;
  dirId: string;
  items: Item[];
  selectedId: string | null;
  /** The column's multi-selection (multi-trash 2): a plain pick
   * holds exactly the picked item; Ctrl/Cmd+click toggles members
   * without navigating. selectedId remains the focus row (preview
   * + keyboard). */
  selectedIds: Set<string>;
}

const IMAGE_EXTS = ["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "tif", "tiff", "ico"];
const TEXT_EXTS = ["txt", "md", "markdown", "csv", "tsv", "log", "srt", "vtt", "sub", "ssa", "ass", "nfo", "json", "xml", "yaml", "yml", "toml", "ini", "cfg", "sh", "py", "rs", "ts", "js"];

export class ColumnsView {
  readonly el: HTMLElement;
  private strip: HTMLElement;
  private cols: Column[] = [];
  private selectedItem: Item | null = null;
  private ctxMenu = new ContextMenu();
  private openWithDialog = new OpenWithDialog();

  constructor(
    private tab: () => Tab,
    private hooks: FileViewHooks,
  ) {
    this.el = document.createElement("div");
    this.el.className = "columns-view";
    this.el.hidden = true;
    this.el.tabIndex = 0;
    this.strip = document.createElement("div");
    this.strip.className = "columns-strip";
    this.el.appendChild(this.strip);
    this.el.addEventListener("keydown", (e) => this.onKey(e));
  }

  get selected(): Item | null {
    return this.selectedItem;
  }

  /** Start (or restart) the column stack at the tab's current directory. */
  reset(tab: Tab): void {
    const dir = tab.history[tab.hIndex];
    this.cols = [
      {
        locationId: dir.locationId,
        dirId: dir.dirId,
        items: sortedItems(tab),
        selectedId: null,
        selectedIds: new Set(),
      },
    ];
    this.selectedItem = null;
    this.render();
  }

  private render(): void {
    this.strip.innerHTML = "";
    this.cols.forEach((col, ci) => {
      const colEl = document.createElement("div");
      colEl.className = "col";
      colEl.setAttribute("role", "listbox");
      for (const item of col.items) {
        const row = document.createElement("button");
        row.className =
          "col-row" +
          (item.kind === "folder" ? " is-folder" : "") +
          (col.selectedIds.has(item.id) ? " selected" : "");
        const name = document.createElement("span");
        name.className = "col-name";
        name.textContent = item.name;
        row.appendChild(name);
        if (item.kind === "folder") {
          const chev = document.createElement("span");
          chev.className = "col-chev";
          chev.textContent = "›";
          row.appendChild(chev);
        }
        row.addEventListener("click", (e) => {
          // Modifier clicks build the column's multi-selection
          // WITHOUT navigating (multi-trash 2); a plain click keeps
          // the Miller behavior — select, and folders open a column.
          if (e.ctrlKey || e.metaKey) {
            this.togglePick(ci, item);
            return;
          }
          if (e.shiftKey) {
            this.rangePick(ci, item);
            return;
          }
          void this.pick(ci, item);
        });
        row.addEventListener("dblclick", () => {
          if (item.kind !== "folder") this.hooks.onOpen(item);
        });
        row.addEventListener("contextmenu", (e) => {
          e.preventDefault();
          e.stopPropagation();
          // Effective targets, FileView's rule: the column's bunch
          // when the clicked row belongs to it, else the row alone
          // (picked first, as before).
          const colNow = this.cols[ci];
          if (!colNow || !colNow.selectedIds.has(item.id)) {
            void this.pick(ci, item, true);
            this.openMenu(e.clientX, e.clientY, item, [item]);
            return;
          }
          const targets =
            colNow.selectedIds.size > 1
              ? colNow.items.filter((i) => colNow.selectedIds.has(i.id))
              : [item];
          this.openMenu(e.clientX, e.clientY, item, targets);
        });
        colEl.appendChild(row);
      }
      if (col.items.length === 0) {
        const empty = document.createElement("p");
        empty.className = "col-empty";
        empty.textContent = "Empty folder";
        colEl.appendChild(empty);
      }
      this.strip.appendChild(colEl);
    });

    // Preview pane for a selected file.
    if (this.selectedItem && this.selectedItem.kind !== "folder") {
      this.strip.appendChild(this.buildPreview(this.selectedItem));
    }
    // Keep the newest column visible.
    this.strip.scrollLeft = this.strip.scrollWidth;
  }

  private buildPreview(item: Item): HTMLElement {
    const pane = document.createElement("div");
    pane.className = "col-preview";
    const ext = item.name.split(".").pop()?.toLowerCase() ?? "";
    if (IMAGE_EXTS.includes(ext)) {
      const img = document.createElement("img");
      img.className = "col-preview-img";
      img.alt = item.name;
      api
        .getThumbnail(item.id, item.provider_key, true)
        .then((url) => (img.src = url))
        .catch(() =>
          api.getImagePreview(item.id, item.provider_key).then((url) => (img.src = url)).catch(() => {})
        );
      pane.appendChild(img);
    }
    if (TEXT_EXTS.includes(ext)) {
      const pre = document.createElement("pre");
      pre.className = "col-preview-text";
      pre.textContent = "Loading…";
      api
        .previewText(item.id, 30)
        .then((r) => {
          pre.textContent = r.lines.join("\n") + (r.truncated ? "\n… (truncated)" : "");
        })
        .catch(() => {
          pre.textContent = "(couldn't read this file)";
        });
      pane.appendChild(pre);
    }
    const name = document.createElement("h3");
    name.className = "col-preview-name";
    name.textContent = item.name;
    const meta = document.createElement("p");
    meta.className = "col-preview-meta";
    meta.textContent = `${item.kind === "folder" ? "Folder" : formatBytes(item.size_bytes)} · ${formatDate(item.modified)}`;
    const openBtn = document.createElement("button");
    openBtn.className = "col-preview-open";
    openBtn.textContent = "Open";
    openBtn.addEventListener("click", () => this.hooks.onOpen(item));
    pane.append(name, meta, openBtn);
    return pane;
  }

  private async pick(ci: number, item: Item, quiet = false): Promise<void> {
    const col = this.cols[ci];
    col.selectedId = item.id;
    col.selectedIds = new Set([item.id]);
    this.selectedItem = item;
    const tab = this.tab();
    tab.selection.clear();
    tab.selection.add(item.id);
    if (!quiet) this.hooks.onSelect(item);
    // Anything to the right of this column is now stale.
    this.cols = this.cols.slice(0, ci + 1);
    if (item.kind === "folder") {
      this.render();
      try {
        const items = await api.listDir(col.locationId, item.id);
        this.cols.push({
          locationId: col.locationId,
          dirId: item.id,
          items: [...items].sort((a, b) => a.name.localeCompare(b.name)),
          selectedId: null,
          selectedIds: new Set(),
        });
      } catch {
        this.cols.push({
          locationId: col.locationId,
          dirId: item.id,
          items: [],
          selectedId: null,
          selectedIds: new Set(),
        });
      }
    }
    this.render();
  }

  /** Ctrl/Cmd+click: toggle a row in the column's multi-selection.
   * Never navigates — a folder toggled here is selected, not opened
   * (multi-trash 2). The tab selection mirrors the column's bunch
   * so every shared reader sees the same truth. */
  private togglePick(ci: number, item: Item): void {
    const col = this.cols[ci];
    if (!col) return;
    if (col.selectedIds.has(item.id)) col.selectedIds.delete(item.id);
    else col.selectedIds.add(item.id);
    col.selectedId = item.id;
    this.selectedItem = item;
    const tab = this.tab();
    tab.selection = new Set(col.selectedIds);
    this.hooks.onSelect(item);
    this.render();
  }

  /** Shift+click: extend the column's selection over the span from
   * the focus row to the clicked row (union with what stands). */
  private rangePick(ci: number, item: Item): void {
    const col = this.cols[ci];
    if (!col) return;
    const anchor = col.items.findIndex((i) => i.id === col.selectedId);
    const idx = col.items.findIndex((i) => i.id === item.id);
    if (anchor < 0 || idx < 0) {
      void this.pick(ci, item);
      return;
    }
    const [a, b] = anchor < idx ? [anchor, idx] : [idx, anchor];
    for (let i = a; i <= b; i++) col.selectedIds.add(col.items[i].id);
    col.selectedId = item.id;
    this.selectedItem = item;
    const tab = this.tab();
    tab.selection = new Set(col.selectedIds);
    this.hooks.onSelect(item);
    this.render();
  }

  /** The effective selection of the ACTIVE (last) column — what
   * the shell's Delete key acts on. Falls back to the focus item. */
  selectionItems(): Item[] {
    const col = this.cols[this.cols.length - 1];
    if (!col) return [];
    const items = col.items.filter((i) => col.selectedIds.has(i.id));
    if (items.length > 0) return items;
    return this.selectedItem ? [this.selectedItem] : [];
  }

  private openMenu(x: number, y: number, item: Item, targets: Item[] = [item]): void {
    void showItemMenu(
      this.ctxMenu,
      x,
      y,
      item,
      targets,
      this.hooks,
      (i) => this.openWithDialog.open(i),
    );
  }

  private onKey(e: KeyboardEvent): void {
    const active = this.cols.length - 1;
    const col = this.cols[active];
    if (!col) return;
    const idx = col.items.findIndex((i) => i.id === col.selectedId);
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const next =
        e.key === "ArrowDown"
          ? Math.min(col.items.length - 1, idx + 1)
          : Math.max(0, idx - 1);
      if (col.items[next]) void this.pick(active, col.items[next]);
    } else if (e.key === "ArrowRight") {
      const sel = col.items[idx];
      if (sel?.kind === "folder") {
        e.preventDefault();
        void this.pick(active, sel);
      }
    } else if (e.key === "ArrowLeft" && this.cols.length > 1) {
      e.preventDefault();
      this.cols = this.cols.slice(0, -1);
      const prev = this.cols[this.cols.length - 1];
      this.selectedItem = prev.items.find((i) => i.id === prev.selectedId) ?? null;
      this.render();
    } else if (e.key === "Enter") {
      const sel = col.items[idx];
      if (sel) {
        e.preventDefault();
        this.hooks.onOpen(sel);
      }
    }
  }
}
