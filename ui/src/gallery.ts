/**
 * GalleryView: one big stage for the selected item over a filmstrip of
 * the directory. Built for photos — big preview, thumbnails below,
 * arrow-key browsing. The strip keeps the selected frame centered
 * (scroll-snap + scrollIntoView), flicks carry native momentum.
 */
import { api, formatBytes, formatDate, type Item } from "./api";
import { fileTypeGlyph } from "./sectionicons";
import { sortedItems, type Tab } from "./tabs";
import { ContextMenu } from "./contextmenu";
import { OpenWithDialog } from "./openwith";
import {
  alternativeToUrl,
  dragState,
  showItemMenu,
  type FileViewHooks,
} from "./fileview";

const IMAGE_EXTS = ["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "tif", "tiff", "ico"];
const TEXT_EXTS = ["txt", "md", "markdown", "csv", "tsv", "log", "srt", "vtt", "sub", "ssa", "ass", "nfo", "json", "xml", "yaml", "yml", "toml", "ini", "cfg", "sh", "py", "rs", "ts", "js"];

export class GalleryView {
  readonly el: HTMLElement;
  private stage: HTMLElement;
  private strip: HTMLElement;
  private selectedItem: Item | null = null;
  /** Shift-range anchor (index into sortedItems) for strip picks. */
  private anchorIndex = -1;
  private ctxMenu = new ContextMenu();
  private openWithDialog = new OpenWithDialog();
  private renderToken = 0;

  constructor(
    private tab: () => Tab,
    private hooks: FileViewHooks,
  ) {
    this.el = document.createElement("div");
    this.el.className = "gallery-view";
    this.el.hidden = true;
    this.el.tabIndex = 0;

    this.stage = document.createElement("div");
    this.stage.className = "gallery-stage";
    this.strip = document.createElement("div");
    this.strip.className = "gallery-strip";
    this.el.append(this.stage, this.strip);
    this.el.addEventListener("keydown", (e) => this.onKey(e));
  }

  get selected(): Item | null {
    return this.selectedItem;
  }

  render(): void {
    const tab = this.tab();
    const items = sortedItems(tab);
    const token = ++this.renderToken;

    // Keep the current selection if it survives; else first item.
    if (!this.selectedItem || !items.some((i) => i.id === this.selectedItem!.id)) {
      this.selectedItem = items[0] ?? null;
    }
    // The stage item is always part of the shared selection (a fresh
    // render after navigation starts from the cleared set).
    if (this.selectedItem && tab.selection.size === 0) {
      tab.selection.add(this.selectedItem.id);
    }
    this.renderStage(token);
    this.renderStrip(items);
  }

  private renderStage(token: number): void {
    this.stage.innerHTML = "";
    const item = this.selectedItem;
    if (!item) {
      const p = document.createElement("p");
      p.className = "gallery-empty";
      p.textContent = "This folder is empty.";
      this.stage.appendChild(p);
      return;
    }
    const ext = item.name.split(".").pop()?.toLowerCase() ?? "";
    if (IMAGE_EXTS.includes(ext) && item.kind !== "folder") {
      const img = document.createElement("img");
      img.className = "gallery-img";
      img.alt = item.name;
      api
        .getImagePreview(item.id, item.provider_key)
        .then((url) => {
          if (token === this.renderToken) img.src = url;
        })
        .catch(() => {});
      this.stage.appendChild(img);
    } else if (TEXT_EXTS.includes(ext) && item.kind !== "folder") {
      const pre = document.createElement("pre");
      pre.className = "gallery-text";
      pre.textContent = "Loading…";
      api
        .previewText(item.id, 60)
        .then((r) => {
          if (token === this.renderToken) {
            pre.textContent = r.lines.join("\n") + (r.truncated ? "\n… (truncated)" : "");
          }
        })
        .catch(() => {
          pre.textContent = "(couldn't read this file)";
        });
      this.stage.appendChild(pre);
    } else {
      const icon = document.createElement("div");
      icon.className = "gallery-bigicon";
      // Icon set v2: document families wear their own glyph
      // (16px-proofed shapes); everything else keeps the emoji.
      const glyph = item.kind === "folder" ? null : fileTypeGlyph(item.name);
      if (glyph) icon.appendChild(glyph);
      else icon.textContent = item.kind === "folder" ? "📁" : "📄";
      this.stage.appendChild(icon);
    }
    const caption = document.createElement("div");
    caption.className = "gallery-caption";
    const name = document.createElement("span");
    name.className = "gallery-name";
    name.textContent = item.name;
    const meta = document.createElement("span");
    meta.className = "gallery-meta";
    meta.textContent = `${item.kind === "folder" ? "Folder" : formatBytes(item.size_bytes)} · ${formatDate(item.modified)}`;
    caption.append(name, meta);
    this.stage.appendChild(caption);
  }

  private renderStrip(items: Item[]): void {
    this.strip.innerHTML = "";
    for (const item of items) {
      const cell = document.createElement("button");
      cell.className =
        "gallery-cell" + (this.tab().selection.has(item.id) ? " selected" : "");
      cell.title = item.name;
      const ext = item.name.split(".").pop()?.toLowerCase() ?? "";
      if (IMAGE_EXTS.includes(ext) && item.kind !== "folder") {
        const img = document.createElement("img");
        img.className = "gallery-thumb";
        img.alt = "";
        api
          .getThumbnail(item.id, item.provider_key, false)
          .then((url) => (img.src = url))
          .catch(() => {});
        cell.appendChild(img);
      } else {
        const icon = document.createElement("span");
        icon.className = "gallery-cell-icon";
        const glyph = item.kind === "folder" ? null : fileTypeGlyph(item.name);
        if (glyph) icon.appendChild(glyph);
        else icon.textContent = item.kind === "folder" ? "📁" : "📄";
        cell.appendChild(icon);
      }
      const label = document.createElement("span");
      label.className = "gallery-cell-name";
      label.textContent = item.name;
      cell.appendChild(label);
      cell.addEventListener("click", (e) => this.pick(item, false, e));
      cell.addEventListener("dblclick", () => this.hooks.onOpen(item));
      cell.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        e.stopPropagation();
        // The menu acts on the effective selection when the clicked
        // thumbnail belongs to one (multi-trash 2) — the same targets
        // rule FileView's menu has always used.
        const tab = this.tab();
        if (!tab.selection.has(item.id)) this.pick(item, true);
        const targets =
          tab.selection.has(item.id) && tab.selection.size > 1
            ? sortedItems(tab).filter((i) => tab.selection.has(i.id))
            : [item];
        this.openMenu(e.clientX, e.clientY, item, targets);
      });
      // Drag source, FileView's payload rule: the whole selection
      // when the grabbed thumbnail belongs to one.
      cell.draggable = true;
      cell.addEventListener("dragstart", (e) => {
        dragState.item = item;
        const tab = this.tab();
        dragState.items =
          tab.selection.has(item.id) && tab.selection.size > 1
            ? sortedItems(tab).filter((i) => tab.selection.has(i.id))
            : [item];
        if (e.dataTransfer) {
          e.dataTransfer.effectAllowed = "copyMove";
          e.dataTransfer.setData("text/plain", item.provider_key ?? item.name);
        }
      });
      cell.addEventListener("dragend", () => {
        dragState.item = null;
        dragState.items = null;
      });
      this.strip.appendChild(cell);
      if (this.selectedItem?.id === item.id) {
        requestAnimationFrame(() =>
          cell.scrollIntoView({ block: "nearest", inline: "center", behavior: "smooth" })
        );
      }
    }
  }

  /** Pick a thumbnail. The gallery shares the tab selection model
   * with every other view (multi-trash 2, 2026-10-16 — pick() used
   * to clear the selection to the one item unconditionally, so no
   * gesture here could ever act on more than one file): plain click
   * sole-selects and stages the item, Ctrl/Cmd+click toggles it in
   * the bunch, Shift+click extends a range from the anchor. The
   * STAGE follows the picked item whenever it ends up selected. */
  private pick(item: Item, quiet = false, e?: MouseEvent): void {
    const tab = this.tab();
    const items = sortedItems(tab);
    const idx = items.findIndex((i) => i.id === item.id);
    const toggle = !!(e && (e.ctrlKey || e.metaKey));
    const range = !!(e && e.shiftKey);
    if (range && this.anchorIndex >= 0 && idx >= 0) {
      const [a, b] =
        this.anchorIndex < idx
          ? [this.anchorIndex, idx]
          : [idx, this.anchorIndex];
      tab.selection = new Set(items.slice(a, b + 1).map((i) => i.id));
      this.selectedItem = item;
    } else if (toggle) {
      if (tab.selection.has(item.id)) tab.selection.delete(item.id);
      else tab.selection.add(item.id);
      // The stage item must stay selected — a gallery with nothing
      // staged has nothing to show.
      if (tab.selection.size === 0) tab.selection.add(item.id);
      this.anchorIndex = idx;
      this.selectedItem = tab.selection.has(item.id)
        ? item
        : (items.find((i) => tab.selection.has(i.id)) ?? item);
    } else {
      tab.selection = new Set([item.id]);
      this.anchorIndex = idx;
      this.selectedItem = item;
    }
    if (!quiet) this.hooks.onSelect(item);
    const token = ++this.renderToken;
    this.renderStage(token);
    this.syncStripSelection();
    const stageIdx = items.findIndex((i) => i.id === this.selectedItem?.id);
    const cell = this.strip.children[stageIdx >= 0 ? stageIdx : idx];
    if (cell) {
      (cell as HTMLElement).scrollIntoView({ block: "nearest", inline: "center", behavior: "smooth" });
    }
  }

  /** Highlight exactly the thumbnails whose ids are in the shared
   * selection — what you see selected is what the doors will act on. */
  private syncStripSelection(): void {
    const tab = this.tab();
    const items = sortedItems(tab);
    this.strip.querySelectorAll(".gallery-cell").forEach((c, i) => {
      c.classList.toggle("selected", !!items[i] && tab.selection.has(items[i].id));
    });
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
    const items = sortedItems(this.tab());
    const idx = items.findIndex((i) => i.id === this.selectedItem?.id);
    if (e.key === "ArrowRight" || e.key === "ArrowLeft") {
      e.preventDefault();
      const next =
        e.key === "ArrowRight"
          ? Math.min(items.length - 1, idx + 1)
          : Math.max(0, idx - 1);
      if (items[next]) this.pick(items[next]);
    } else if (e.key === "Enter") {
      if (this.selectedItem) {
        e.preventDefault();
        this.hooks.onOpen(this.selectedItem);
      }
    }
  }
}
