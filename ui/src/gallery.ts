/**
 * GalleryView: one big stage for the selected item over a filmstrip of
 * the directory. Built for photos — big preview, thumbnails below,
 * arrow-key browsing. The strip keeps the selected frame centered
 * (scroll-snap + scrollIntoView), flicks carry native momentum.
 */
import { api, formatBytes, formatDate, type Item } from "./api";
import { sortedItems, type Tab } from "./tabs";
import { ContextMenu } from "./contextmenu";
import { OpenWithDialog } from "./openwith";
import type { FileViewHooks } from "./fileview";

const IMAGE_EXTS = ["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "tif", "tiff", "ico"];

export class GalleryView {
  readonly el: HTMLElement;
  private stage: HTMLElement;
  private strip: HTMLElement;
  private selectedItem: Item | null = null;
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
    } else {
      const icon = document.createElement("div");
      icon.className = "gallery-bigicon";
      icon.textContent = item.kind === "folder" ? "📁" : "📄";
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
      cell.className = "gallery-cell" + (this.selectedItem?.id === item.id ? " selected" : "");
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
        icon.textContent = item.kind === "folder" ? "📁" : "📄";
        cell.appendChild(icon);
      }
      const label = document.createElement("span");
      label.className = "gallery-cell-name";
      label.textContent = item.name;
      cell.appendChild(label);
      cell.addEventListener("click", () => this.pick(item));
      cell.addEventListener("dblclick", () => this.hooks.onOpen(item));
      cell.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        e.stopPropagation();
        this.pick(item, true);
        this.openMenu(e.clientX, e.clientY, item);
      });
      this.strip.appendChild(cell);
      if (this.selectedItem?.id === item.id) {
        requestAnimationFrame(() =>
          cell.scrollIntoView({ block: "nearest", inline: "center", behavior: "smooth" })
        );
      }
    }
  }

  private pick(item: Item, quiet = false): void {
    this.selectedItem = item;
    const tab = this.tab();
    tab.selection.clear();
    tab.selection.add(item.id);
    if (!quiet) this.hooks.onSelect(item);
    const token = ++this.renderToken;
    this.renderStage(token);
    this.strip.querySelectorAll(".gallery-cell").forEach((c, i) => {
      const items = sortedItems(this.tab());
      c.classList.toggle("selected", items[i]?.id === item.id);
    });
    const idx = sortedItems(this.tab()).findIndex((i) => i.id === item.id);
    const cell = this.strip.children[idx];
    if (cell) {
      (cell as HTMLElement).scrollIntoView({ block: "nearest", inline: "center", behavior: "smooth" });
    }
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
