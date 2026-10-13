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
import type { FileViewHooks } from "./fileview";

interface Column {
  locationId: string;
  dirId: string;
  items: Item[];
  selectedId: string | null;
}

const IMAGE_EXTS = ["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "tif", "tiff", "ico"];

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
          (col.selectedId === item.id ? " selected" : "");
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
        row.addEventListener("click", () => void this.pick(ci, item));
        row.addEventListener("dblclick", () => {
          if (item.kind !== "folder") this.hooks.onOpen(item);
        });
        row.addEventListener("contextmenu", (e) => {
          e.preventDefault();
          e.stopPropagation();
          void this.pick(ci, item, true);
          this.openMenu(e.clientX, e.clientY, item);
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
        });
      } catch {
        this.cols.push({
          locationId: col.locationId,
          dirId: item.id,
          items: [],
          selectedId: null,
        });
      }
    }
    this.render();
  }

  private openMenu(x: number, y: number, item: Item): void {
    this.ctxMenu.open(x, y, [
      { label: "Open", action: () => this.hooks.onOpen(item) },
      { label: "Open With...", action: () => this.openWithDialog.open(item) },
      { label: "Quick Look", shortcut: "Space", action: () => this.hooks.onTogglePreview() },
      {
        label: "Make Executable",
        shortcut: "Ctrl+E",
        action: () => this.hooks.onMakeExecutable(item),
      },
      { label: "Properties", action: () => this.hooks.onProperties(item) },
      { label: "Delete", shortcut: "Del", danger: true, action: () => this.hooks.onDelete(item) },
    ]);
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
