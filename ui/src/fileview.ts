/**
 * File views: grid (large icons + name) and list (name/size/modified/kind).
 * Selection (click, ctrl/shift-click), sort, and rubber-band overscroll.
 * View toggle uses an interruptible WAAPI crossfade from live opacity.
 */
import { formatBytes, formatDate, type Item } from "./api";
import { animateFromLive, liveOpacity, reducedMotion, fadeTo, rubberBand, springTo, cancelSpring } from "./motion";
import { sortedItems, type SortDir, type SortKey, type Tab, type ViewMode } from "./tabs";
import { ContextMenu } from "./contextmenu";
import { OpenWithDialog } from "./openwith";

export interface FileViewHooks {
  onOpen(item: Item): void; // Enter / double-click on a folder (or file)
  onSelect(item: Item | null): void; // selection changed
  onTogglePreview(): void; // Space handled by shell
  onMakeExecutable(item: Item): void;
  onDelete(item: Item): void; // move to trash
  onProperties(item: Item): void;
}

/** The item currently being dragged out of a file view, if any.
 * HTML5 drag events can't read dataTransfer payloads during dragover,
 * so drop targets consult this shared slot instead. */
export const dragState: { item: Item | null } = { item: null };

const KIND_ICON: Record<string, string> = {
  folder: "M3 5a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z",
  file: "M6 2h8l4 4v14a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V3a1 1 0 0 1 1-1z",
};

function icon(kind: string, cls: string): string {
  const d = KIND_ICON[kind] ?? KIND_ICON.file;
  return `<svg class="${cls}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true"><path d="${d}"/></svg>`;
}

export class FileView {
  readonly el: HTMLElement;
  private scroller: HTMLElement;
  private overscroll = 0;
  private wheelEndTimer = 0;
  private lastSelected: Item | null = null;
  private colWidths: Map<string, number> = new Map();
  private ctxMenu = new ContextMenu();
  private openWithDialog = new OpenWithDialog();

  constructor(
    private tab: () => Tab,
    private hooks: FileViewHooks,
  ) {
    this.el = document.createElement("div");
    this.el.className = "fileview";
    this.el.setAttribute("role", "grid");
    this.el.tabIndex = 0;
    this.scroller = document.createElement("div");
    this.scroller.className = "fileview-scroller";
    this.el.appendChild(this.scroller);
    this.el.addEventListener("wheel", (e) => this.onWheel(e), { passive: false });
    this.el.addEventListener("keydown", (e) => this.onKey(e));
  }

  setTab(t: Tab): void {
    // Rebind is done via the tab() closure; kept for symmetry.
    void t;
  }

  /** Called when the active tab changes: drop transient focus, re-render. */
  onTabSwitch(): void {
    this.lastSelected = null;
    this.overscroll = 0;
    this.scroller.style.transform = "";
    this.render();
  }

  /** Render items for the current tab state (interruptible). */
  render(): void {
    // The empty state (no locations) replaces the view's children; if the
    // scroller was detached, re-attach it before rendering into it.
    if (!this.scroller.isConnected) {
      this.el.innerHTML = "";
      this.el.appendChild(this.scroller);
    }
    const tab = this.tab();
    this.el.dataset.view = tab.view;
    if (tab.view === "grid") this.renderGrid(tab);
    else this.renderList(tab);
    this.updateSelection();
    // Scroll back to top on navigation; keep position on re-render of sorts.
    this.scroller.scrollTop = 0;
  }

  /** Swap grid<->list with an interruptible crossfade from live opacity. */
  switchView(mode: ViewMode): void {
    const tab = this.tab();
    if (tab.view === mode) return;
    tab.view = mode;
    const apply = () => this.render();
    if (reducedMotion()) {
      fadeTo(this.el, 0, () => {
        apply();
        fadeTo(this.el, 1);
      });
      return;
    }
    const from = liveOpacity(this.el);
    const out = animateFromLive(
      this.el,
      [{ opacity: from }, { opacity: 0 }],
      { duration: 110, easing: "ease-in", fill: "forwards" },
    );
    out.onfinish = () => {
      apply();
      animateFromLive(
        this.el,
        [{ opacity: 0 }, { opacity: 1 }],
        { duration: 160, easing: "ease-out", fill: "forwards" },
      );
    };
  }

  /** Sort by key; clicking the active key flips direction. */
  sortBy(key: SortKey): void {
    const tab = this.tab();
    if (tab.sort === key) tab.sortDir = (tab.sortDir * -1) as SortDir;
    else {
      tab.sort = key;
      tab.sortDir = 1;
    }
    this.render();
  }

  // ------------------------------------------------------------- rendering

  private renderGrid(tab: Tab): void {
    this.scroller.className = "fileview-scroller grid";
    this.scroller.innerHTML = "";
    const frag = document.createDocumentFragment();
    sortedItems(tab).forEach((item, i) => {
      const cell = document.createElement("div");
      cell.className = "cell";
      cell.dataset.id = item.id;
      cell.dataset.index = String(i);
      cell.setAttribute("role", "gridcell");
      cell.innerHTML = `${icon(item.kind, "cell-icon")}<span class="cell-name"></span>`;
      cell.querySelector(".cell-name")!.textContent = item.name;
      cell.title = item.name;
      this.bindCell(cell, item, i);
      frag.appendChild(cell);
    });
    this.scroller.appendChild(frag);
  }

  private renderList(tab: Tab): void {
    this.scroller.className = "fileview-scroller list";
    this.scroller.innerHTML = "";
    const table = document.createElement("table");
    table.className = "list-table";
    const thead = document.createElement("thead");
    thead.innerHTML =
      `<tr>` +
      (["name", "size", "modified"] as SortKey[])
        .map(
          (k) =>
            `<th data-sort="${k}" tabindex="0" role="columnheader" aria-sort="${
              tab.sort === k ? (tab.sortDir === 1 ? "ascending" : "descending") : "none"
            }">${k === "name" ? "Name" : k === "size" ? "Size" : "Modified"}${
              tab.sort === k ? (tab.sortDir === 1 ? " ▲" : " ▼") : ""
            }</th>`,
        )
        .join("") +
      `<th>Kind</th></tr>`;
    thead.querySelectorAll<HTMLElement>("th[data-sort]").forEach((th) => {
      const key = th.dataset.sort as SortKey;
      th.addEventListener("click", () => this.sortBy(key));
      th.addEventListener("keydown", (e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          this.sortBy(key);
        }
      });
    });
    // Add resize handles to all headers
    thead.querySelectorAll<HTMLElement>("th").forEach((th) => {
      // Apply stored width
      const colKey = th.dataset.sort || "kind";
      const saved = this.colWidths.get(colKey);
      if (saved) th.style.width = `${saved}px`;

      const handle = document.createElement("div");
      handle.className = "col-resize-handle";
      th.appendChild(handle);
      th.style.position = "relative";

      handle.addEventListener("mousedown", (e) => {
        e.preventDefault();
        e.stopPropagation();
        const startX = e.clientX;
        const startWidth = th.offsetWidth;
        const onMove = (ev: MouseEvent) => {
          const newWidth = Math.max(60, startWidth + (ev.clientX - startX));
          th.style.width = `${newWidth}px`;
          this.colWidths.set(colKey, newWidth);
        };
        const onUp = () => {
          document.removeEventListener("mousemove", onMove);
          document.removeEventListener("mouseup", onUp);
        };
        document.addEventListener("mousemove", onMove);
        document.addEventListener("mouseup", onUp);
      });
    });
    table.appendChild(thead);
    const tbody = document.createElement("tbody");
    sortedItems(tab).forEach((item, i) => {
      const tr = document.createElement("tr");
      tr.className = "row";
      tr.dataset.id = item.id;
      tr.dataset.index = String(i);
      const nameTd = document.createElement("td");
      nameTd.className = "row-name";
      nameTd.innerHTML = icon(item.kind, "row-icon");
      const label = document.createElement("span");
      label.textContent = item.name;
      nameTd.appendChild(label);
      if (item.starred) {
        const star = document.createElement("span");
        star.className = "star";
        star.textContent = "★";
        star.setAttribute("aria-label", "starred");
        nameTd.appendChild(star);
      }
      tr.appendChild(nameTd);
      const sizeTd = document.createElement("td");
      sizeTd.className = "num";
      sizeTd.textContent = item.kind === "folder" ? "—" : formatBytes(item.size_bytes);
      tr.appendChild(sizeTd);
      const modTd = document.createElement("td");
      modTd.textContent = formatDate(item.modified);
      tr.appendChild(modTd);
      const kindTd = document.createElement("td");
      kindTd.className = "muted";
      kindTd.textContent = item.kind === "folder" ? "Folder" : "File";
      tr.appendChild(kindTd);
      this.bindCell(tr, item, i);
      tbody.appendChild(tr);
    });
    table.appendChild(tbody);
    this.scroller.appendChild(table);
  }

  private bindCell(cell: HTMLElement, item: Item, index: number): void {
    cell.addEventListener("click", (e) => {
      e.stopPropagation();
      this.select(item, index, e);
    });
    cell.addEventListener("dblclick", (e) => {
      e.stopPropagation();
      this.hooks.onOpen(item);
    });
    cell.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      e.stopPropagation();
      // Select the item first if not already selected
      const tab = this.tab();
      if (!tab.selection.has(item.id)) {
        this.select(item, index);
      }
      this.ctxMenu.open(e.clientX, e.clientY, [
        {
          label: "Open",
          action: () => this.hooks.onOpen(item),
        },
        {
          label: "Open With...",
          action: () => this.openWithDialog.open(item),
        },
        {
          label: "Quick Look",
          shortcut: "Space",
          action: () => this.hooks.onTogglePreview(),
        },
        {
          label: "Make Executable",
          shortcut: "Ctrl+E",
          action: () => this.hooks.onMakeExecutable(item),
        },
        {
          label: "Properties",
          action: () => this.hooks.onProperties(item),
        },
        {
          label: "Delete",
          shortcut: "Del",
          danger: true,
          action: () => this.hooks.onDelete(item),
        },
      ]);
    });
    // Drag source: files and folders can be dragged to sidebar favorites
    // (Trash deletes; a folder favorite files the item into it).
    cell.draggable = true;
    cell.addEventListener("dragstart", (e) => {
      dragState.item = item;
      if (e.dataTransfer) {
        e.dataTransfer.effectAllowed = "move";
        e.dataTransfer.setData("text/plain", item.provider_key ?? item.name);
      }
      cell.classList.add("dragging");
    });
    cell.addEventListener("dragend", () => {
      dragState.item = null;
      cell.classList.remove("dragging");
    });
  }

  // ------------------------------------------------------------- selection

  select(item: Item, index: number, e?: MouseEvent): void {
    const tab = this.tab();
    const items = sortedItems(tab);
    const multi = !!(e && (e.ctrlKey || e.metaKey));
    const range = !!(e && e.shiftKey);
    if (range && tab.anchorIndex >= 0) {
      const [a, b] =
        tab.anchorIndex < index
          ? [tab.anchorIndex, index]
          : [index, tab.anchorIndex];
      tab.selection.clear();
      for (let i = a; i <= b; i++) tab.selection.add(items[i].id);
    } else if (multi) {
      if (tab.selection.has(item.id)) tab.selection.delete(item.id);
      else tab.selection.add(item.id);
      tab.anchorIndex = index;
    } else {
      tab.selection.clear();
      tab.selection.add(item.id);
      tab.anchorIndex = index;
    }
    this.lastSelected = item;
    this.updateSelection();
    this.hooks.onSelect(item);
    this.el.focus({ preventScroll: true });
  }

  selectIndex(index: number, extend: boolean): void {
    const tab = this.tab();
    const items = sortedItems(tab);
    if (index < 0 || index >= items.length) return;
    const item = items[index];
    if (extend && tab.anchorIndex >= 0) {
      const [a, b] =
        tab.anchorIndex < index
          ? [tab.anchorIndex, index]
          : [index, tab.anchorIndex];
      tab.selection.clear();
      for (let i = a; i <= b; i++) tab.selection.add(items[i].id);
    } else {
      tab.selection.clear();
      tab.selection.add(item.id);
      tab.anchorIndex = index;
    }
    this.lastSelected = item;
    this.updateSelection();
    this.scrollIntoView(index);
    this.hooks.onSelect(item);
  }

  get selected(): Item | null {
    const tab = this.tab();
    if (!this.lastSelected || !tab.selection.has(this.lastSelected.id))
      return null;
    return this.lastSelected;
  }

  clearSelection(): void {
    const tab = this.tab();
    tab.selection.clear();
    tab.anchorIndex = -1;
    this.lastSelected = null;
    this.updateSelection();
    this.hooks.onSelect(null);
  }

  private updateSelection(): void {
    const tab = this.tab();
    this.scroller
      .querySelectorAll<HTMLElement>("[data-id]")
      .forEach((el) => {
        const on = tab.selection.has(el.dataset.id!);
        el.setAttribute("aria-selected", String(on));
        el.classList.toggle("selected", on);
      });
  }

  private scrollIntoView(index: number): void {
    const el = this.scroller.querySelector<HTMLElement>(
      `[data-index="${index}"]`,
    );
    el?.scrollIntoView({ block: "nearest" });
  }

  private onKey(e: KeyboardEvent): void {
    const tab = this.tab();
    const items = sortedItems(tab);
    if (items.length === 0) return;
    const cur =
      tab.anchorIndex >= 0 ? tab.anchorIndex : 0;
    let next = cur;
    if (e.key === "ArrowDown" || e.key === "ArrowRight") next = Math.min(items.length - 1, cur + 1);
    else if (e.key === "ArrowUp" || e.key === "ArrowLeft") next = Math.max(0, cur - 1);
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = items.length - 1;
    else if (e.key === "Enter") {
      const sel = this.selected ?? items[cur];
      if (sel) this.hooks.onOpen(sel);
      e.preventDefault();
      return;
    } else return;
    e.preventDefault();
    this.selectIndex(next, e.shiftKey);
  }

  // ------------------------------------------------- rubber-band overscroll

  private onWheel(e: WheelEvent): void {
    const el = this.scroller;
    const atTop = el.scrollTop <= 0;
    const atBottom = el.scrollTop + el.clientHeight >= el.scrollHeight - 1;
    const dy = e.deltaY;
    const pushingPast = (atTop && dy < 0) || (atBottom && dy > 0);
    if (!pushingPast) {
      if (this.overscroll !== 0) this.releaseRubber();
      return;
    }
    e.preventDefault();
    this.overscroll += dy * 0.55;
    const bent = rubberBand(this.overscroll, 72);
    if (reducedMotion()) {
      el.style.transform = `translateY(${Math.sign(bent) * 8}px)`;
    } else {
      el.style.transform = `translateY(${bent}px)`;
    }
    window.clearTimeout(this.wheelEndTimer);
    this.wheelEndTimer = window.setTimeout(() => this.releaseRubber(), 90);
  }

  private releaseRubber(): void {
    window.clearTimeout(this.wheelEndTimer);
    const el = this.scroller;
    this.overscroll = 0;
    const from = parseFloat(
      (el.style.transform.match(/translateY\((-?[\d.]+)px\)/) ?? [])[1] ?? "0",
    );
    if (reducedMotion()) {
      fadeTo(el, 1, () => {
        el.style.transform = "";
        el.style.opacity = "";
      });
      return;
    }
    cancelSpring("fileview-rubber");
    springTo(
      "fileview-rubber",
      0,
      (v) => {
        el.style.transform = v === 0 ? "" : `translateY(${v}px)`;
      },
      { from, stiffness: 420, damping: 2 * Math.sqrt(420) * 0.85 },
    );
  }
}
