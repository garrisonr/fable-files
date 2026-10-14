/**
 * File views: grid (large icons + name) and list (name/size/modified/kind).
 * Selection (click, ctrl/shift-click), sort, and rubber-band overscroll.
 * View toggle uses an interruptible WAAPI crossfade from live opacity.
 */
import { api, formatBytes, formatDate, type Item } from "./api";
import { animateFromLive, liveOpacity, reducedMotion, fadeTo, rubberBand, springTo, cancelSpring } from "./motion";
import { sortedItems, type SortDir, type SortKey, type Tab, type ViewMode } from "./tabs";
import { ContextMenu, type MenuItem } from "./contextmenu";
import { OpenWithDialog } from "./openwith";
import { labelColor, loadLabelDefs } from "./labels";
import { isVideoName } from "./video";
import { isAudioName } from "./music";

export interface FileViewHooks {
  onOpen(item: Item): void; // Enter / double-click on a folder (or file)
  onOpenInNewTab?(item: Item): void; // right-click: open in a new tab
  onSelect(item: Item | null): void; // selection changed
  onTogglePreview(): void; // Space handled by shell
  onMakeExecutable(item: Item): void;
  onDelete(item: Item): void; // move to trash
  onTrash?(items: Item[]): void; // move a whole selection to trash
  onMediaRename?(items: Item[]): void; // Plex/Kodi/Emby rename
  onPlayMusic?(items: Item[]): void; // music player
  onRenameInline?(item: Item, newName: string): void;
  onProperties(item: Item): void;
  // Round 4 (Nautilus + Finder parity) — optional so every view can
  // share one menu builder.
  onCut?(items: Item[]): void;
  onCopy?(items: Item[]): void;
  onMoveTo?(items: Item[]): void;
  onCopyTo?(items: Item[]): void;
  onRename?(item: Item): void;
  onCompress?(items: Item[]): void;
  onEmail?(items: Item[]): void;
  onSendLocalSend?(item: Item): void;
  onDuplicate?(items: Item[]): void;
  onMakeAlias?(item: Item): void;
  onConvertImage?(item: Item, format: string): void;
  onCreatePdf?(items: Item[]): void;
  onRotateImage?(item: Item): void;
  onSetWallpaper?(item: Item): void;
  onToggleLabel?(items: Item[], label: string, on: boolean): void;
  onManageLabels?(): void;
  onExtractArchive?(item: Item, toDir: boolean): void;
  onTranscode?(item: Item, preset: string): void;
  onMetadata?(item: Item): void;
  onMountIso?(item: Item): void;
  onCast?(item: Item): void;
  onChecksums?(item: Item): void;
  onSplitFile?(item: Item): void;
  onJoinFiles?(item: Item): void;
  onBurnAdd?(item: Item): void;
  onPrint?(item: Item): void;
  onCreateTorrent?(item: Item): void;
  onNewFolder?(): void;
  onNewFile?(kind: string): void;
  onPasteHere?(): void;
  onTerminalHere?(): void;
}

const AUDIO_EXTS = new Set([
  "mp3", "flac", "ogg", "oga", "wav", "m4a", "m4b", "opus", "aac", "aif", "aiff", "wma",
]);

const ARCHIVE_EXTS = new Set([
  "zip", "tar", "tgz", "gz", "bz2", "xz", "7z", "rar",
]);

export function isArchiveItem(item: Item): boolean {
  if (item.kind === "folder") return false;
  const ext = item.name.split(".").pop()?.toLowerCase() ?? "";
  return ARCHIVE_EXTS.has(ext);
}

/** Color-grade an item element from its first label + render dots
 * for up to three labels. Shared by grid cells and list rows. */
export function applyLabelGrade(el: HTMLElement, item: Item): void {
  if (!item.labels || item.labels.length === 0) return;
  const defs = loadLabelDefs();
  const color = labelColor(defs, item.labels[0]);
  if (color) {
    el.style.setProperty("--label-color", color);
    el.classList.add("labeled");
  }
  const dots = document.createElement("span");
  dots.className = "label-dots";
  for (const name of item.labels.slice(0, 3)) {
    const d = document.createElement("span");
    d.className = "label-dot";
    d.style.background = labelColor(defs, name) ?? "#9ca3af";
    d.title = name;
    dots.appendChild(d);
  }
  el.appendChild(dots);
}

const IMAGE_EXTS = new Set([
  "png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "svg", "ico", "avif",
]);

export function isImageItem(item: Item): boolean {
  const ext = item.name.split(".").pop()?.toLowerCase() ?? "";
  return IMAGE_EXTS.has(ext);
}

const defaultAppCache = new Map<string, { id: string; name: string } | null>();
let localsendCache: boolean | null = null;

/**
 * The one right-click menu for file items — Nautilus + Finder parity.
 * `targets` is the effective selection (the right-clicked item plus any
 * multi-selection it belongs to).
 */
/** Media files the Plex/Kodi/Emby renamer handles. */
function isMediaName(name: string): boolean {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  return [
    "mkv", "mp4", "avi", "mov", "m4v", "webm", "mpg", "mpeg", "wmv",
    "mp3", "flac", "m4a", "m4b", "aac", "ogg", "opus", "wav",
  ].includes(ext);
}

export async function showItemMenu(
  menu: ContextMenu,
  x: number,
  y: number,
  item: Item,
  targets: Item[],
  hooks: FileViewHooks,
  openWith: (item: Item) => void,
): Promise<void> {
  const ext = item.name.split(".").pop()?.toLowerCase() ?? "";
  if (!defaultAppCache.has(ext)) {
    try {
      const info = await api.openWithApps(item.id);
      if (info.default) {
        const app = info.apps.find((a) => a.id === info.default);
        defaultAppCache.set(ext, {
          id: info.default,
          name: app ? app.name : info.default.replace(/\.desktop$/, ""),
        });
      } else {
        defaultAppCache.set(ext, null);
      }
    } catch {
      defaultAppCache.set(ext, null);
    }
  }
  if (localsendCache === null) {
    localsendCache = await api.localsendAvailable().catch(() => false);
  }
  const def = defaultAppCache.get(ext);
  const items: MenuItem[] = [];
  const sep: MenuItem = { label: "", separator: true, action: () => undefined };

  // Items inside an archive: only Open / Quick Look make sense.
  if (item.id.startsWith("arc:")) {
    items.push({ label: "Open", action: () => hooks.onOpen(item) });
    items.push({
      label: "Quick Look",
      shortcut: "Space",
      action: () => hooks.onTogglePreview(),
    });
    menu.open(x, y, items);
    return;
  }

  if (def) {
    items.push({
      label: `Open With ${def.name}`,
      shortcut: "Return",
      action: () => void api.openWithApp(item.id, def.id),
    });
  }
  items.push({ label: "Open", action: () => hooks.onOpen(item) });
  if (hooks.onOpenInNewTab)
    items.push({
      label: "Open in New Tab",
      action: () => hooks.onOpenInNewTab!(item),
    });
  items.push({ label: "Open With...", action: () => openWith(item) });
  if (isArchiveItem(item) && hooks.onExtractArchive) {
    items.push({
      label: "Browse Archive",
      action: () => hooks.onOpen(item),
    });
    items.push({
      label: "Extract Here",
      action: () => hooks.onExtractArchive!(item, false),
    });
    items.push({
      label: "Extract to...",
      action: () => hooks.onExtractArchive!(item, true),
    });
  }
  items.push({
    label: "Quick Look",
    shortcut: "Space",
    action: () => hooks.onTogglePreview(),
  });
  items.push(sep);
  if (hooks.onCut)
    items.push({
      label: "Cut",
      shortcut: "Ctrl+X",
      action: () => hooks.onCut!(targets),
    });
  if (hooks.onCopy)
    items.push({
      label: "Copy",
      shortcut: "Ctrl+C",
      action: () => hooks.onCopy!(targets),
    });
  if (hooks.onMoveTo)
    items.push({ label: "Move to...", action: () => hooks.onMoveTo!(targets) });
  if (hooks.onCopyTo)
    items.push({ label: "Copy to...", action: () => hooks.onCopyTo!(targets) });
  if (hooks.onTrash || hooks.onDelete)
    items.push({
      label: targets.length > 1 ? `Move ${targets.length} Items to Trash` : "Move to Trash",
      shortcut: "Del",
      danger: true,
      action: () =>
        hooks.onTrash ? hooks.onTrash(targets) : hooks.onDelete(item),
    });
  items.push(sep);
  if (hooks.onRename && targets.length === 1)
    items.push({
      label: "Rename...",
      shortcut: "F2",
      action: () => hooks.onRename!(item),
    });
  if (hooks.onDuplicate)
    items.push({ label: "Duplicate", action: () => hooks.onDuplicate!(targets) });
  if (hooks.onMakeAlias && targets.length === 1)
    items.push({ label: "Make Alias", action: () => hooks.onMakeAlias!(item) });
  if (
    hooks.onPlayMusic &&
    targets.length >= 1 &&
    targets.every((t) => t.kind !== "folder" && isAudioName(t.name))
  )
    items.push({
      label: targets.length === 1 ? "Play" : `Play ${targets.length} Tracks`,
      action: () => hooks.onPlayMusic!(targets),
    });
  if (
    hooks.onMediaRename &&
    targets.length >= 1 &&
    targets.every((t) => t.kind !== "folder" && isMediaName(t.name))
  )
    items.push({
      label:
        targets.length === 1
          ? "Rename for Media Player..."
          : `Rename ${targets.length} for Media Player...`,
      action: () => hooks.onMediaRename!(targets),
    });
  if (hooks.onCompress)
    items.push({
      label: targets.length === 1 ? `Compress "${item.name}"` : "Compress...",
      action: () => hooks.onCompress!(targets),
    });
  if (hooks.onEmail)
    items.push({ label: "Email...", action: () => hooks.onEmail!(targets) });
  if (localsendCache && hooks.onSendLocalSend)
    items.push({
      label: "Send via LocalSend",
      action: () => hooks.onSendLocalSend!(item),
    });
  if (isImageItem(item)) {
    items.push(sep);
    if (hooks.onConvertImage)
      items.push({
        label: "Convert Image...",
        action: () => {
          const formats: MenuItem[] = ["png", "jpg", "webp", "bmp"].map(
            (f) => ({
              label: `Convert to ${f.toUpperCase()}`,
              action: () => hooks.onConvertImage!(item, f),
            }),
          );
          menu.open(x, y, formats);
        },
      });
    if (hooks.onCreatePdf)
      items.push({
        label: "Create PDF",
        action: () => hooks.onCreatePdf!(targets.filter(isImageItem)),
      });
    if (hooks.onRotateImage)
      items.push({
        label: "Rotate Left",
        action: () => hooks.onRotateImage!(item),
      });
    if (hooks.onSetWallpaper)
      items.push({
        label: "Set Desktop Picture",
        action: () => hooks.onSetWallpaper!(item),
      });
  }
  if ((isVideoName(item.name) || AUDIO_EXTS.has(ext)) && hooks.onTranscode) {
    items.push(sep);
    if (isVideoName(item.name)) {
      items.push({
        label: "Transcode Video…",
        action: () => {
          menu.open(x, y, [
            { label: "MP4 · H.264 — plays everywhere", action: () => hooks.onTranscode!(item, "v_mp4_264") },
            { label: "MP4 · H.265 — smaller, modern", action: () => hooks.onTranscode!(item, "v_mp4_265") },
            { label: "WebM · VP9 — for the web", action: () => hooks.onTranscode!(item, "v_webm") },
            { label: "MKV · H.265 — archive quality", action: () => hooks.onTranscode!(item, "v_mkv_265") },
            { label: "GIF — short clips", action: () => hooks.onTranscode!(item, "v_gif") },
            { label: "Extract Audio · MP3", action: () => hooks.onTranscode!(item, "a_mp3") },
          ]);
        },
      });
    }
    if (AUDIO_EXTS.has(ext)) {
      items.push({
        label: "Transcode Audio…",
        action: () => {
          menu.open(x, y, [
            { label: "MP3 · 320 kbps", action: () => hooks.onTranscode!(item, "a_mp3") },
            { label: "FLAC · lossless", action: () => hooks.onTranscode!(item, "a_flac") },
            { label: "M4A · AAC 256", action: () => hooks.onTranscode!(item, "a_m4a") },
            { label: "Opus · 160 kbps", action: () => hooks.onTranscode!(item, "a_opus") },
            { label: "WAV · uncompressed", action: () => hooks.onTranscode!(item, "a_wav") },
          ]);
        },
      });
    }
  }
  {
    // ——— Power tools: metadata, casting, ISO, checksums, split/join, burn
    const isFile = item.kind !== "folder";
    const ISO_EXTS = ["iso", "img", "dmg", "cdi", "nrg", "bin"];
    const MEDIA_EXTS = [
      "mp4", "mkv", "webm", "mov", "avi", "m4v", "mp3", "flac", "ogg",
      "wav", "m4a", "opus",
    ];
    const tools: MenuItem[] = [];
    if (!item.id.startsWith("arc:")) {
      if (hooks.onMetadata) {
        tools.push({
          label: "Metadata…",
          action: () => hooks.onMetadata!(item),
        });
      }
      if (MEDIA_EXTS.includes(ext) && hooks.onCast) {
        tools.push({
          label: "Cast to…",
          action: () => hooks.onCast!(item),
        });
      }
      if (hooks.onCreateTorrent) {
        tools.push({
          label: "Create Torrent…",
          action: () => hooks.onCreateTorrent!(item),
        });
      }
      const PRINTABLE = [
        "pdf", "ps", "txt", "jpg", "jpeg", "png", "gif", "tif", "tiff",
        "doc", "docx", "odt", "rtf", "xls", "xlsx", "ods", "ppt", "pptx",
        "odp", "html",
      ];
      if (PRINTABLE.includes(ext) && hooks.onPrint) {
        tools.push({
          label: "Print…",
          action: () => hooks.onPrint!(item),
        });
      }
      if (ISO_EXTS.includes(ext) && hooks.onMountIso) {
        tools.push({
          label: "Mount Image",
          action: () => hooks.onMountIso!(item),
        });
      }
      if (isFile && hooks.onChecksums) {
        tools.push({
          label: "Checksums…",
          action: () => hooks.onChecksums!(item),
        });
      }
      if (isFile && hooks.onSplitFile) {
        tools.push({
          label: "Split File…",
          action: () => hooks.onSplitFile!(item),
        });
      }
      if (item.name.toLowerCase().endsWith(".001") && hooks.onJoinFiles) {
        tools.push({
          label: "Combine Parts…",
          action: () => hooks.onJoinFiles!(item),
        });
      }
      if (hooks.onBurnAdd) {
        tools.push({
          label: "Add to Burn Project",
          action: () => hooks.onBurnAdd!(item),
        });
      }
    }
    if (tools.length > 0) {
      items.push(sep, ...tools);
    }
  }
  if (hooks.onToggleLabel || hooks.onManageLabels) {
    items.push(sep);
    const defs = loadLabelDefs();
    for (const def of defs) {
      const applied = targets.every((t) => t.labels.includes(def.name));
      items.push({
        label: def.name,
        dot: def.color,
        checked: applied,
        action: () => hooks.onToggleLabel?.(targets, def.name, !applied),
      });
    }
    if (hooks.onManageLabels)
      items.push({
        label: "Manage Labels…",
        action: () => hooks.onManageLabels!(),
      });
  }
  items.push(sep);
  items.push({
    label: "Make Executable",
    shortcut: "Ctrl+E",
    action: () => hooks.onMakeExecutable(item),
  });
  items.push({
    label: "Find More Apps Online…",
    action: () => void api.openUrl(alternativeToUrl(item)),
  });
  items.push(sep);
  items.push({ label: "Properties", action: () => hooks.onProperties(item) });
  menu.open(x, y, items);
}

/** The item currently being dragged out of a file view, if any.
 * HTML5 drag events can't read dataTransfer payloads during dragover,
 * so drop targets consult this shared slot instead. */
export const dragState: { item: Item | null; favName: string | null } = {
  item: null,
  favName: null,
};

const KIND_ICON: Record<string, string> = {
  folder: "M3 5a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z",
  file: "M6 2h8l4 4v14a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V3a1 1 0 0 1 1-1z",
};

function icon(kind: string, cls: string): string {
  const d = KIND_ICON[kind] ?? KIND_ICON.file;
  return `<svg class="${cls}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true"><path d="${d}"/></svg>`;
}

/** AlternativeTo search URL for programs that can open this file type. */
export function alternativeToUrl(item: Item): string {
  const ext = item.name.includes(".")
    ? item.name.split(".").pop()!.toLowerCase()
    : item.name;
  return `https://alternativeto.net/browse/search/?q=${encodeURIComponent(ext)}`;
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
    // Background (empty-space) menu: cells/rows stopPropagation on
    // their own contextmenu, so anything reaching here is background.
    this.el.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      this.showBackgroundMenu(e.clientX, e.clientY);
    });
  }

  /** Right-click on empty space: New Folder / New File / Paste / Terminal. */
  private showBackgroundMenu(x: number, y: number): void {
    const hooks = this.hooks;
    const items: MenuItem[] = [];
    if (hooks.onNewFolder) {
      items.push({
        label: "New Folder",
        action: () => hooks.onNewFolder!(),
      });
    }
    if (hooks.onNewFile) {
      const kinds: Array<[string, string]> = [
        ["Text File", "txt"],
        ["Markdown", "md"],
        ["Word Document", "docx"],
        ["Spreadsheet", "xlsx"],
        ["Presentation", "pptx"],
        ["OpenDocument Text", "odt"],
        ["OpenDocument Spreadsheet", "ods"],
        ["OpenDocument Presentation", "odp"],
        ["CSV", "csv"],
        ["HTML Page", "html"],
        ["JSON", "json"],
        ["Shell Script", "sh"],
        ["Python Script", "py"],
      ];
      items.push({
        label: "New File",
        action: () => {
          this.ctxMenu.open(
            x,
            y,
            kinds.map(([label, kind]) => ({
              label: `${label} (.${kind})`,
              action: () => hooks.onNewFile!(kind),
            })),
          );
        },
      });
    }
    if (hooks.onPasteHere) {
      items.push({
        label: "Paste",
        action: () => hooks.onPasteHere!(),
      });
    }
    if (hooks.onTerminalHere) {
      items.push({
        label: "Open Terminal Here",
        action: () => hooks.onTerminalHere!(),
      });
    }
    if (items.length > 0) this.ctxMenu.open(x, y, items);
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
      applyLabelGrade(cell, item);
      this.bindCell(cell, item, i);
      frag.appendChild(cell);
      // Photos show real thumbnails (freedesktop cache) instead of an icon.
      const ext = item.name.split(".").pop()?.toLowerCase() ?? "";
      if (
        item.kind !== "folder" &&
        ["png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "ico"].includes(ext)
      ) {
        api
          .getThumbnail(item.id, item.provider_key, false)
          .then((url) => {
            const img = document.createElement("img");
            img.className = "cell-thumb";
            img.alt = "";
            img.src = url;
            const iconEl = cell.querySelector(".cell-icon");
            if (iconEl && cell.isConnected) iconEl.replaceWith(img);
          })
          .catch(() => {
            // No thumbnail — the kind icon stays.
          });
      }
    });
    this.scroller.appendChild(frag);
  }

  private renderList(tab: Tab): void {
    this.scroller.className = "fileview-scroller list";
    this.scroller.innerHTML = "";
    const table = document.createElement("table");
    table.className = "list-table";
    const colgroup = document.createElement("colgroup");
    const colEls: HTMLTableColElement[] = [];
    for (const [k, defW] of [["name", 0], ["size", 96], ["modified", 184], ["kind", 96]] as Array<[string, number]>) {
      const col = document.createElement("col");
      const w = this.colWidths.get(k) ?? defW;
      if (w > 0) col.style.width = `${w}px`;
      colgroup.appendChild(col);
      colEls.push(col);
    }
    table.appendChild(colgroup);
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
    thead.querySelectorAll<HTMLElement>("th").forEach((th, thi) => {
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
          if (colEls[thi]) colEls[thi].style.width = `${newWidth}px`;
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
      applyLabelGrade(tr, item);
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

  private namePress: { id: string; wasOnly: boolean; t: number } | null = null;
  private renameTimer: number | null = null;

  private bindCell(cell: HTMLElement, item: Item, index: number): void {
    cell.addEventListener("mousedown", (e) => {
      if (e.button !== 0) return;
      e.stopPropagation();
      const tab = this.tab();
      this.namePress = {
        id: item.id,
        wasOnly: tab.selection.size === 1 && tab.selection.has(item.id),
        t: Date.now(),
      };
      this.select(item, index, e);
    });
    cell.addEventListener("click", (e) => {
      e.stopPropagation();
      // Finder-style rename: a deliberate click on the NAME of the item
      // that was already the sole selection when the press began arms
      // an inline rename; a double-click cancels it and opens instead.
      const np = this.namePress;
      this.namePress = null;
      if (!np || np.id !== item.id || !np.wasOnly) return;
      if (!(e.target as HTMLElement).closest(".row-name, .cell-name")) return;
      if (this.renameTimer !== null) window.clearTimeout(this.renameTimer);
      this.renameTimer = window.setTimeout(() => {
        this.renameTimer = null;
        this.startInlineRename(cell, item);
      }, 550);
    });
    cell.addEventListener("dblclick", (e) => {
      e.stopPropagation();
      if (this.renameTimer !== null) {
        window.clearTimeout(this.renameTimer);
        this.renameTimer = null;
      }
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
      const targets = tab.selection.has(item.id) && tab.selection.size > 1
        ? sortedItems(tab).filter((i) => tab.selection.has(i.id))
        : [item];
      void showItemMenu(
        this.ctxMenu,
        e.clientX,
        e.clientY,
        item,
        targets,
        this.hooks,
        (it) => this.openWithDialog.open(it),
      );
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

  /** Rename in place: swap the name label for an input, Enter/blur
   * commits through the hook, Esc cancels. Stem pre-selected. */
  private startInlineRename(cell: HTMLElement, item: Item): void {
    if (!this.hooks.onRenameInline) return;
    const tab = this.tab();
    if (!(tab.selection.size === 1 && tab.selection.has(item.id))) return;
    const label = cell.querySelector<HTMLElement>(".row-name span:not(.star), .cell-name");
    if (!label || label.querySelector("input")) return;
    const original = item.name;
    const input = document.createElement("input");
    input.className = "rename-inline";
    input.value = original;
    input.setAttribute("aria-label", "Rename");
    label.textContent = "";
    label.appendChild(input);
    input.focus();
    const dot = original.lastIndexOf(".");
    if (item.kind !== "folder" && dot > 0) input.setSelectionRange(0, dot);
    else input.select();
    let done = false;
    const commit = (save: boolean) => {
      if (done) return;
      done = true;
      const next = input.value.trim();
      label.textContent = original;
      if (save && next && next !== original) this.hooks.onRenameInline!(item, next);
    };
    input.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Enter") {
        e.preventDefault();
        commit(true);
      } else if (e.key === "Escape") {
        e.preventDefault();
        commit(false);
      }
    });
    input.addEventListener("blur", () => commit(true));
    input.addEventListener("mousedown", (e) => e.stopPropagation());
    input.addEventListener("click", (e) => e.stopPropagation());
    input.addEventListener("dblclick", (e) => e.stopPropagation());
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

  /** Mark commands (Total Commander style): select all / invert /
   * same extension as the current item. */
  selectAll(): void {
    const tab = this.tab();
    tab.selection = new Set(tab.items.map((i) => i.id));
    this.rerenderSelection();
  }

  invertSelection(): void {
    const tab = this.tab();
    const next = new Set<string>();
    for (const i of tab.items) {
      if (!tab.selection.has(i.id)) next.add(i.id);
    }
    tab.selection = next;
    this.rerenderSelection();
  }

  selectSameExtension(): void {
    const tab = this.tab();
    const anchor = this.selected;
    if (!anchor) return;
    const ext = anchor.name.split(".").pop()?.toLowerCase() ?? "";
    tab.selection = new Set(
      tab.items
        .filter((i) => (i.name.split(".").pop()?.toLowerCase() ?? "") === ext)
        .map((i) => i.id),
    );
    this.rerenderSelection();
  }

  private rerenderSelection(): void {
    this.render();
    const first = this.selectionItems()[0];
    if (first) this.hooks.onSelect(first);
  }

  /** The effective selection: multi-selection if active, else the
   * last selected item. */
  selectionItems(): Item[] {
    const tab = this.tab();
    if (tab.selection.size > 0) {
      const items = sortedItems(tab).filter((i) => tab.selection.has(i.id));
      if (items.length > 0) return items;
    }
    return this.selected ? [this.selected] : [];
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
