/**
 * Shell assembly: toolbar (nav buttons, breadcrumb path bar, view toggle,
 * search, theme menu), translucent sidebar (Favorites + Locations), tab bar,
 * viewport (file view / search results / Quick Look), and status bar.
 * All styling comes from --ufm-* tokens; this file creates no colors.
 */
import {
  api,
  type Item,
  type LocationSummary,
  type ThemeInfo,
} from "./api";
import { FileView, dragState, type FileViewHooks } from "./fileview";
import { ColumnsView } from "./columns";
import { GalleryView } from "./gallery";
import { AboutDialog } from "./about";
import { CommandPalette, type PaletteCommand } from "./palette";
import { IndexTracker } from "./indexing";
import { reducedMotion, animateFromLive, liveOpacity } from "./motion";
import { QuickLook } from "./quicklook";
import { ContextMenu } from "./contextmenu";
import { Toast, confirmDialog, promptDialog } from "./toast";
import { SearchView } from "./search";
import { PropertiesDialog } from "./properties";
import { FolderPicker, type PickerShortcut } from "./picker";
import { RenameDialog } from "./rename";
import { LabelManager } from "./labels";
import { loadLabelDefs, migrateLabelAssignments } from "./labels";
import type { ArchiveEntry } from "./api";
import { TerminalPanel } from "./terminal";
import { MetadataDialog } from "./metadata";
import { BurnStudio } from "./burn";
import { CloudMount } from "./cloud";
import { CastDialog } from "./cast";
import { AppDeleter } from "./deleter";
import {
  ChecksumDialog,
  DuplicatesDialog,
  SplitDialog,
  SyncDialog,
} from "./tools";
import { DiskUtility } from "./disks";
import { TorrentDialog } from "./torrent";
import type { MenuItem } from "./contextmenu";
import { PrintDialog } from "./print";
import { JobsPanel } from "./jobs";
import {
  canGoBack,
  canGoForward,
  currentDir,
  descend,
  goBack,
  goForward,
  goToCrumb,
  goUp,
  makeTab,
  navigate,
  type Tab,
  type ViewMode,
} from "./tabs";
import { DEFAULT_THEME, ThemeManager, type MenuThemeItem } from "./themes";

/** Where a sidebar favorite navigates: a real location root or a
 * same-named folder inside the Home location. */
interface FavoriteTarget {
  locationId: string;
  dirId: string;
  display: string;
}

/** A saved search pinned in the sidebar. */
interface SmartFolder {
  name: string;
  query: string;
  type: string | null;
}

export class Shell {
  readonly el = document.createElement("div");
  private tabs: Tab[] = [];
  private activeTab: Tab | null = null;
  private locations: LocationSummary[] = [];

  private crumbBar!: HTMLElement;
  private crumbsWrap!: HTMLElement;
  private backBtn!: HTMLButtonElement;
  private fwdBtn!: HTMLButtonElement;
  private upBtn!: HTMLButtonElement;
  private tabBar!: HTMLElement;
  private sidebarList!: HTMLElement;
  private favList!: HTMLElement;
  private statusLeft!: HTMLElement;
  private statusRight!: HTMLElement;
  private viewport!: HTMLElement;
  private menuLayer!: HTMLElement;
  private searchWrap!: HTMLElement;

  private fileView: FileView;
  private columnsView: ColumnsView;
  private galleryView: GalleryView;
  private aboutDialog = new AboutDialog();
  private palette = new CommandPalette();
  private menuBtn!: HTMLButtonElement;
  /** Undoable file operations (trash / drag-move), newest last. */
  private undoStack: Array<
    | { kind: "trash"; trashName: string; label: string }
    | { kind: "move"; newPath: string; fromDir: string; label: string }
  > = [];
  private quickLook: QuickLook;
  private searchView: SearchView;
  private themeManager: ThemeManager;
  private indexTracker: IndexTracker;
  private ctxMenu = new ContextMenu();
  private propertiesDialog = new PropertiesDialog();
  private fontScale = 1.0;
  private picker = new FolderPicker();
  private renameDialog = new RenameDialog();
  private labelManager = new LabelManager();
  private labelList!: HTMLElement;
  private smartList!: HTMLElement;
  /** Archive browsing state: archive item id -> its catalog Item and
   * cached listing. */
  private archiveMeta = new Map<string, Item>();
  private archiveCache = new Map<string, ArchiveEntry[]>();
  private termPanel = new TerminalPanel();
  private jobsPanel = new JobsPanel();
  private metadataDialog = new MetadataDialog();
  private castDialog = new CastDialog();
  private appDeleter = new AppDeleter();
  private checksumDialog = new ChecksumDialog();
  private syncDialog = new SyncDialog();
  private splitDialog = new SplitDialog();
  private dupDialog = new DuplicatesDialog();
  private burnStudio!: BurnStudio;
  private cloudMount!: CloudMount;
  private diskDialog = new DiskUtility();
  private torrentDialog = new TorrentDialog();
  private printDialog = new PrintDialog();

  constructor() {
    this.el.id = "app";
    this.buildChrome();

    const viewHooks: FileViewHooks = {
      onOpen: (item) => this.openItem(item),
      onSelect: (item) => this.onSelect(item),
      onTogglePreview: () => this.togglePreview(),
      onMakeExecutable: (item) => {
        api.setExecutable(item.id, true).catch((err) =>
          console.error("chmod +x failed:", err)
        );
      },
      onDelete: (item) => void this.deleteItem(item),
      onTrash: (items) => void this.trashItems(items),
      onRenameInline: (item, name) => void this.renameInline(item, name),
      onProperties: (item) => this.propertiesDialog.open(item),
      onCut: (items) => void this.clipboardSet(items, true),
      onCopy: (items) => void this.clipboardSet(items, false),
      onMoveTo: (items) => void this.moveOrCopyTo(items, "move"),
      onCopyTo: (items) => void this.moveOrCopyTo(items, "copy"),
      onRename: (item) =>
        this.renameDialog.open(item, () => {
          const t = this.activeTab;
          if (t) void this.loadDir(t);
          this.refreshQuickLook(item.provider_key);
        }),
      onCompress: (items) => {
        Toast.show(
          items.length === 1
            ? `Compressing “${items[0].name}”…`
            : `Compressing ${items.length} items…`,
          "info",
          8000,
        );
        void api
          .compressItems(items.map((i) => i.id))
          .then(async (p) => {
            Toast.success(`Created ${p}`);
            const t = this.activeTab;
            if (t) {
              await this.loadDir(t);
              const made = t.items.find((i) => i.provider_key === p);
              if (made) {
                t.selection.clear();
                t.selection.add(made.id);
                this.renderActiveView();
              }
            }
          })
          .catch((err) => Toast.error(`Compress failed: ${err}`));
      },
      onEmail: (items) => {
        void api
          .emailItems(items.map((i) => i.id))
          .catch((err) => Toast.error(`Email failed: ${err}`));
      },
      onSendLocalSend: (item) => {
        void api
          .sendViaLocalsend(item.id)
          .then(() => Toast.success("Sent to LocalSend"))
          .catch((err) => Toast.error(`LocalSend failed: ${err}`));
      },
      onDuplicate: (items) => void this.duplicateItems(items),
      onMakeAlias: (item) => {
        void api
          .makeAlias(item.id)
          .then((p) => {
            Toast.success(`Created alias ${p.split("/").pop()}`);
            const t = this.activeTab;
            if (t) void this.loadDir(t);
          })
          .catch((err) => Toast.error(`Alias failed: ${err}`));
      },
      onConvertImage: (item, format) => {
        void api
          .convertImage(item.id, format)
          .then((p) => {
            Toast.success(`Converted to ${p.split("/").pop()}`);
            const t = this.activeTab;
            if (t) void this.loadDir(t);
          })
          .catch((err) => Toast.error(`Convert failed: ${err}`));
      },
      onCreatePdf: (items) => {
        if (items.length === 0) return;
        void api
          .createPdf(items.map((i) => i.id))
          .then((p) => {
            Toast.success(`Created ${p.split("/").pop()}`);
            const t = this.activeTab;
            if (t) void this.loadDir(t);
          })
          .catch((err) => Toast.error(`PDF failed: ${err}`));
      },
      onRotateImage: (item) => {
        void api
          .rotateImageLeft(item.id)
          .then(async () => {
            Toast.success(`Rotated ${item.name}`);
            const t = this.activeTab;
            if (t) await this.loadDir(t);
            this.refreshQuickLook(item.provider_key);
          })
          .catch((err) => Toast.error(`Rotate failed: ${err}`));
      },
      onSetWallpaper: (item) => {
        void api
          .setWallpaper(item.id)
          .then(() => Toast.success("Desktop picture set"))
          .catch((err) => Toast.error(`Wallpaper failed: ${err}`));
      },
      onToggleLabel: (items, label, on) =>
        void this.toggleLabel(items, label, on),
      onManageLabels: () =>
        this.labelManager.open(() => {
          void this.buildSidebar();
          const t = this.activeTab;
          if (t) void this.loadDir(t);
        }),
      onExtractArchive: (item, toDir) => void this.extractArchive(item, toDir),
      onTranscode: (item, preset) => {
        void api
          .transcode(item.id, preset)
          .then((id) => this.jobsPanel.trackTranscode(id, item.name))
          .catch((err) => Toast.error(`Transcode failed: ${err}`));
      },
      onMetadata: (item) =>
        void this.metadataDialog.open(item, () => this.refreshActiveDir()),
      onMountIso: (item) => void this.mountIsoItem(item),
      onCast: (item) => void this.castDialog.open(item),
      onChecksums: (item) => void this.checksumDialog.open(item),
      onSplitFile: (item) => this.splitDialog.open(item),
      onJoinFiles: (item) => {
        void api
          .joinFiles(item.id)
          .then((out) => {
            Toast.success(`Combined: ${out.split("/").pop()}`);
            this.refreshActiveDir();
          })
          .catch((err) => Toast.error(`Combine failed: ${err}`));
      },
      onPrint: (item) => void this.printDialog.open(item),
      onCreateTorrent: (item) => void this.torrentDialog.open(item),
      onNewFolder: () => void this.createFolderHere(),
      onNewFile: (kind) => void this.createFileHere(kind),
      onPasteHere: () => void this.pasteIntoCurrentDir(),
      onTerminalHere: () => void this.toggleTerminal(),
      onBurnAdd: (item) => {
        const path = item.provider_key;
        if (!path) {
          Toast.error("That item has no local path to burn.");
          return;
        }
        void this.burnStudio.openWith([
          {
            path,
            size: item.kind === "folder" ? null : (item.size_bytes ?? null),
          },
        ]);
      },
    };
    this.fileView = new FileView(() => this.requireTab(), viewHooks);
    this.columnsView = new ColumnsView(() => this.requireTab(), viewHooks);
    this.galleryView = new GalleryView(() => this.requireTab(), viewHooks);
    this.quickLook = new QuickLook(() => this.fileView.el.focus({ preventScroll: true }));
    this.quickLook.onCast = (item) => void this.castDialog.open(item);
    this.burnStudio = new BurnStudio({
      getSelectionPaths: async () =>
        this.activeSelectionItems()
          .filter((i) => !!i.provider_key)
          .map((i) => ({
            path: i.provider_key as string,
            size: i.kind === "folder" ? null : (i.size_bytes ?? null),
          })),
      openPathLocation: async (isoPath) => {
        const mountPath = await api.mountIsoPath(isoPath);
        await this.addAndOpenLocation(
          isoPath.split("/").pop()?.replace(/\.iso$/i, "") || "Disc Image",
          mountPath,
        );
      },
      refresh: () => this.refreshActiveDir(),
    });
    this.cloudMount = new CloudMount({
      runInTerminal: async (command) => {
        await this.termPanel.runInFolder(command, await this.homePath());
      },
      addLocation: async (path, label) => {
        await api.addPathLocation(label, path);
        await this.refreshLocations();
      },
      removeLocation: async (path) => {
        await this.refreshLocations();
        for (const loc of this.locations) {
          const rootPath = await api.getItemPath(loc.root_item).catch(() => "");
          if (rootPath === path) {
            await api.removePathLocation(loc.id);
          }
        }
        await this.refreshLocations();
      },
      refreshSidebar: async () => {
        await this.buildSidebar();
      },
      homePath: () => this.homePath(),
    });
    this.splitDialog.onDone = () => this.refreshActiveDir();
    this.searchView = new SearchView({
      onNavigateToItem: (item) => {
        this.searchView.clear(); // back to the file view before navigating
        this.openItem(item);
      },
      onPreviewItem: (item) => void this.quickLook.open(item),
      onResults: () => {
        this.fileView.el.hidden = true;
        this.columnsView.el.hidden = true;
        this.galleryView.el.hidden = true;
      },
      onClear: () => this.showFileView(),
      onSaveSmartFolder: () => void this.saveSmartFolder(),
    });
    this.viewport.append(
      this.fileView.el,
      this.columnsView.el,
      this.galleryView.el,
      this.searchView.el,
      this.quickLook.el
    );
    // The search field lives in the toolbar; its results panel is in the viewport.
    this.searchWrap.appendChild(this.searchView.field);
    this.el.append(
      this.picker.element,
      this.renameDialog.element,
      this.labelManager.element,
      this.jobsPanel.el,
      this.metadataDialog.element,
      this.castDialog.element,
      this.appDeleter.element,
      this.checksumDialog.element,
      this.syncDialog.element,
      this.splitDialog.element,
      this.dupDialog.element,
      this.burnStudio.element,
      this.cloudMount.element,
      this.diskDialog.element,
      this.printDialog.element,
      this.torrentDialog.element,
    );
    this.torrentDialog.onCreated = () => this.refreshActiveDir();
    this.diskDialog.onChanged = () => void this.refreshLocations();
    this.jobsPanel.onJobDone = (output) => {
      Toast.success(`Created ${output.split("/").pop()}`);
      this.refreshActiveDir();
    };
    this.jobsPanel.onJobError = (error) => {
      Toast.error(`Job failed: ${error}`);
    };

    this.themeManager = new ThemeManager({
      renderMenu: (themes, activeId, anchor) => this.renderThemeMenu(themes, activeId, anchor),
      closeMenu: () => this.closeMenu(),
    });
    this.indexTracker = new IndexTracker((text) => {
      this.statusRight.textContent = text ?? "";
      this.statusRight.hidden = !text;
    });

    document.addEventListener("keydown", (e) => this.onGlobalKey(e));
    // The native webview context menu (Back / Forward / Stop / Reload)
    // never appears: our own menus preventDefault at their targets, and
    // anything unhandled gets suppressed here. Text fields keep the
    // editing menu (cut/copy/paste).
    document.addEventListener("contextmenu", (e) => {
      if (e.defaultPrevented) return;
      const t = e.target as HTMLElement | null;
      if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA")) return;
      e.preventDefault();
    });
  }

  // ------------------------------------------------------------------ boot

  async boot(): Promise<void> {
    await this.themeManager.init();
    try {
      this.locations = await api.listLocations();
    } catch {
      this.locations = [];
    }
    await this.buildSidebar();
    // Label splits (Music/Media, Payroll/Money): assignments follow
    // the renamed defs so nothing loses its filing.
    void migrateLabelAssignments(loadLabelDefs(), api).then((did) => {
      if (did) void this.buildSidebar();
    });
    const first = this.locations[0];
    if (first) {
      this.newTab(first);
    } else {
      this.renderEmptyState();
    }
    void this.indexTracker.boot(first ? first.id : null);
  }

  private requireTab(): Tab {
    if (!this.activeTab) throw new Error("no active tab");
    return this.activeTab;
  }

  // ---------------------------------------------------------------- chrome

  private buildChrome(): void {
    // Toolbar — translucent, content scrolls beneath it.
    const toolbar = document.createElement("header");
    toolbar.className = "toolbar";

    this.backBtn = this.navButton("Back", "←", () => this.goBack());
    this.fwdBtn = this.navButton("Forward", "→", () => this.goForward());
    this.upBtn = this.navButton("Up one level", "↑", () => this.goUp());
    const nav = document.createElement("div");
    nav.className = "nav-buttons";
    nav.append(this.backBtn, this.fwdBtn, this.upBtn);

    this.crumbBar = document.createElement("nav");
    this.crumbBar.className = "crumbs";
    this.crumbBar.setAttribute("aria-label", "Location");
    // Right-click the path: Copy Path / Edit Path (never the webview menu).
    this.crumbBar.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      e.stopPropagation();
      this.ctxMenu.open(e.clientX, e.clientY, [
        { label: "Copy Path", action: () => void this.copyCurrentPath() },
        { label: "Edit Path", action: () => this.editPath() },
      ]);
    });
    // The copy button lives OUTSIDE the scrolling crumbs so a long path
    // can never push it off-screen.
    const crumbsWrap = document.createElement("div");
    crumbsWrap.className = "crumbs-wrap";
    const copyBtn = document.createElement("button");
    copyBtn.className = "icon-btn crumb-copy";
    copyBtn.textContent = "\u29C9";
    copyBtn.title = "Copy path";
    copyBtn.setAttribute("aria-label", "Copy current path");
    copyBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      void this.copyCurrentPath(copyBtn);
    });
    crumbsWrap.append(this.crumbBar, copyBtn);
    this.crumbsWrap = crumbsWrap;

    const searchWrap = document.createElement("div");
    searchWrap.className = "search-wrap";
    // The search field is mounted here in the constructor, once the real
    // SearchView exists (its results panel lives in the viewport).
    this.searchWrap = searchWrap;

    const menuBtn = document.createElement("button");
    menuBtn.className = "icon-btn menu-btn";
    menuBtn.textContent = "☰";
    menuBtn.setAttribute("aria-label", "Menu");
    menuBtn.setAttribute("aria-haspopup", "menu");
    menuBtn.addEventListener("click", () => this.openMainMenu());
    this.menuBtn = menuBtn;

    toolbar.append(nav, this.crumbsWrap, this.searchWrap, menuBtn);
    this.el.append(toolbar);

    // Menu layer for anchored popovers (theme menu).
    this.menuLayer = document.createElement("div");
    this.menuLayer.className = "menu-layer";
    this.menuLayer.hidden = true;
    this.menuLayer.addEventListener("click", (e) => {
      if (e.target === this.menuLayer) this.closeMenu();
    });

    const workarea = document.createElement("div");
    workarea.className = "workarea";

    const sidebar = document.createElement("aside");
    sidebar.className = "sidebar";
    sidebar.setAttribute("aria-label", "Sidebar");
    const favHead = document.createElement("h2");
    favHead.className = "side-head";
    favHead.textContent = "Favorites";
    this.favList = document.createElement("div");
    this.favList.className = "side-list";
    const locHead = document.createElement("h2");
    locHead.className = "side-head";
    locHead.textContent = "Locations";
    this.sidebarList = document.createElement("div");
    this.sidebarList.className = "side-list";
    const labelHead = document.createElement("button");
    labelHead.className = "side-head side-collapse";
    labelHead.setAttribute("aria-label", "Toggle Labels section");
    const labelHeadText = document.createElement("span");
    labelHeadText.textContent = "Labels";
    const labelChev = document.createElement("span");
    labelChev.className = "side-chev";
    labelChev.textContent = "▾";
    labelHead.append(labelHeadText, labelChev);
    this.labelList = document.createElement("div");
    this.labelList.className = "side-list";
    const labelsCollapsed = (() => {
      try {
        return localStorage.getItem("ff-labels-collapsed") === "1";
      } catch {
        return false;
      }
    })();
    this.labelList.hidden = labelsCollapsed;
    labelChev.style.transform = labelsCollapsed ? "rotate(-90deg)" : "";
    labelHead.addEventListener("click", () => {
      const nowHidden = !this.labelList.hidden;
      this.labelList.hidden = nowHidden;
      labelChev.style.transform = nowHidden ? "rotate(-90deg)" : "";
      try {
        localStorage.setItem("ff-labels-collapsed", nowHidden ? "1" : "0");
      } catch {
        // persistence is a nicety, not a requirement
      }
    });
    const smartHead = document.createElement("h2");
    smartHead.className = "side-head";
    smartHead.textContent = "Smart Folders";
    this.smartList = document.createElement("div");
    this.smartList.className = "side-list";
    sidebar.append(
      favHead,
      this.favList,
      labelHead,
      this.labelList,
      smartHead,
      this.smartList,
      locHead,
      this.sidebarList,
    );

    const main = document.createElement("main");
    main.className = "main";
    this.tabBar = document.createElement("div");
    this.tabBar.className = "tabbar";
    this.tabBar.setAttribute("role", "tablist");
    this.viewport = document.createElement("div");
    this.viewport.className = "viewport";
    main.append(this.tabBar, this.viewport, this.termPanel.el);

    workarea.append(sidebar, main);
    this.el.append(workarea);

    const status = document.createElement("footer");
    status.className = "statusbar";
    this.statusLeft = document.createElement("span");
    this.statusLeft.className = "status-left";
    this.statusRight = document.createElement("span");
    this.statusRight.className = "status-right";
    this.statusRight.hidden = true;
    status.append(this.statusLeft, this.statusRight);
    this.el.append(status);

    this.el.append(this.menuLayer);
  }

  private navButton(label: string, glyph: string, onClick: () => void): HTMLButtonElement {
    const b = document.createElement("button");
    b.className = "icon-btn";
    b.textContent = glyph;
    b.setAttribute("aria-label", label);
    b.title = label;
    b.addEventListener("click", onClick);
    return b;
  }

  // ---------------------------------------------------------------- tabs

  newTab(loc: LocationSummary): Tab {
    const tab = makeTab(loc);
    this.tabs.push(tab);
    this.activateTab(tab);
    void this.loadDir(tab);
    return tab;
  }

  private activateTab(tab: Tab): void {
    this.activeTab = tab;
    this.quickLook.close();
    if (this.searchView.active) this.searchView.clear();
    this.fileView.onTabSwitch();
    this.renderTabBar();
    this.renderCrumbs();
    void this.loadDir(tab);
  }

  private closeTab(tab: Tab): void {
    const i = this.tabs.indexOf(tab);
    if (i < 0) return;
    this.tabs.splice(i, 1);
    if (this.tabs.length === 0) {
      const first = this.locations[0];
      if (first) this.newTab(first);
      else {
        this.activeTab = null;
        this.renderEmptyState();
      }
      return;
    }
    if (this.activeTab === tab) {
      this.activateTab(this.tabs[Math.max(0, i - 1)]);
    } else {
      this.renderTabBar();
    }
  }

  private renderTabBar(): void {
    this.tabBar.innerHTML = "";
    for (const tab of this.tabs) {
      const t = document.createElement("div");
      t.className = "tab";
      t.setAttribute("role", "tab");
      t.setAttribute("aria-selected", String(tab === this.activeTab));
      const label = document.createElement("button");
      label.className = "tab-label";
      const span = document.createElement("span");
      span.textContent = tab.title;
      label.appendChild(span);
      label.addEventListener("click", () => this.activateTab(tab));
      const close = document.createElement("button");
      close.className = "tab-close";
      close.textContent = "✕";
      close.setAttribute("aria-label", `Close tab ${tab.title}`);
      close.addEventListener("click", (e) => {
        e.stopPropagation();
        this.closeTab(tab);
      });
      t.append(label, close);
      this.tabBar.appendChild(t);
    }
    const add = document.createElement("button");
    add.className = "icon-btn tab-add";
    add.textContent = "+";
    add.setAttribute("aria-label", "New tab");
    add.title = "New tab (Ctrl+T)";
    add.addEventListener("click", () => {
      const loc = this.activeTab
        ? this.locations.find((l) => l.id === currentDir(this.activeTab!).locationId)
        : this.locations[0];
      if (loc) this.newTab(loc);
    });
    this.tabBar.appendChild(add);
  }

  // ------------------------------------------------------------ navigation

  private async loadDir(tab: Tab): Promise<void> {
    const dir = currentDir(tab);
    try {
      if (dir.dirId.startsWith("label:")) {
        tab.items = await api.listItemsByLabel(dir.dirId.slice("label:".length));
      } else if (dir.dirId.startsWith("archive:")) {
        tab.items = await this.archiveItems(dir.dirId);
      } else {
        tab.items = await api.listDir(dir.locationId, dir.dirId);
      }
    } catch {
      tab.items = [];
    }
    if (tab === this.activeTab) {
      this.renderActiveView();
      this.renderCrumbs();
      this.renderNavButtons();
      this.renderStatus();
    }
  }

  private navigateTo(dir: { locationId: string; dirId: string; crumbs: { dirId: string; name: string }[] }): void {
    const tab = this.requireTab();
    this.quickLook.close();
    navigate(tab, dir);
    void this.loadDir(tab);
  }

  private goBack(): void {
    const tab = this.requireTab();
    if (goBack(tab)) {
      this.quickLook.close();
      void this.loadDir(tab);
    }
  }

  private goForward(): void {
    const tab = this.requireTab();
    if (goForward(tab)) {
      this.quickLook.close();
      void this.loadDir(tab);
    }
  }

  private goUp(): void {
    const tab = this.requireTab();
    const next = goUp(tab);
    if (next) {
      this.quickLook.close();
      void this.loadDir(tab);
    }
  }

  private openItem(item: Item): void {
    const tab = this.requireTab();
    const dir = currentDir(tab);
    // Inside an archive: folders descend virtually; files extract+open.
    if (dir.dirId.startsWith("archive:")) {
      if (item.kind === "folder") {
        const entryPath = item.id.split(":").slice(2).join(":");
        const archiveId = dir.dirId.slice("archive:".length).split(":")[0];
        const newDirId = `archive:${archiveId}:${entryPath}`;
        this.navigateTo({
          locationId: dir.locationId,
          dirId: newDirId,
          crumbs: [
            ...dir.crumbs,
            { dirId: newDirId, name: item.name },
          ],
        });
      } else {
        void this.openArchiveEntry(item);
      }
      return;
    }
    if (item.kind === "folder") {
      if (dir.dirId.startsWith("label:")) {
        // A labeled folder opens at its real location.
        this.navigateTo({
          locationId: item.location,
          dirId: item.id,
          crumbs: [{ dirId: item.id, name: item.name }],
        });
      } else {
        this.navigateTo(descend(tab, item));
      }
    } else if (Shell.isArchiveName(item.name)) {
      this.openArchive(item);
    } else {
      // Double-click / Enter opens with the OS default app; Spacebar is Quick Look.
      void api
        .openItem(item.id)
        .catch(() => void this.quickLook.open(item));
    }
  }

  private static isArchiveName(name: string): boolean {
    const ext = name.split(".").pop()?.toLowerCase() ?? "";
    return ["zip", "tar", "tgz", "gz", "bz2", "xz", "7z", "rar"].includes(ext);
  }

  /** Enter archive-browsing mode for an archive file. */
  private openArchive(item: Item): void {
    this.archiveMeta.set(item.id, item);
    const dirId = `archive:${item.id}`;
    this.navigateTo({
      locationId: item.location,
      dirId,
      crumbs: [{ dirId, name: item.name }],
    });
  }

  /** Immediate children of a prefix inside a cached archive listing. */
  private async archiveItems(dirId: string): Promise<Item[]> {
    const rest = dirId.slice("archive:".length);
    const sep = rest.indexOf(":");
    const archiveId = sep >= 0 ? rest.slice(0, sep) : rest;
    const sub = sep >= 0 ? rest.slice(sep + 1) : "";
    const meta = this.archiveMeta.get(archiveId);
    if (!meta) return [];
    let entries = this.archiveCache.get(archiveId);
    if (!entries) {
      entries = await api.listArchive(archiveId);
      this.archiveCache.set(archiveId, entries);
    }
    const prefix = sub ? `${sub}/` : "";
    const byName = new Map<string, Item>();
    for (const e of entries) {
      if (prefix && !e.path.startsWith(prefix)) continue;
      const rel = e.path.slice(prefix.length);
      if (!rel) continue;
      const seg = rel.split("/")[0];
      if (byName.has(seg)) continue;
      const isFolder = rel.includes("/") || (e.is_dir && e.path === prefix + seg);
      byName.set(seg, {
        id: `arc:${archiveId}:${prefix}${seg}`,
        location: meta.location,
        name: seg,
        kind: isFolder ? "folder" : "file",
        size_bytes: isFolder ? null : e.size,
        modified: meta.modified,
        provider_key: null,
        labels: [],
        starred: false,
      });
    }
    return [...byName.values()].sort((a, b) => {
      if ((a.kind === "folder") !== (b.kind === "folder")) {
        return a.kind === "folder" ? -1 : 1;
      }
      return a.name.localeCompare(b.name);
    });
  }

  /** Extract one archive entry to temp and open it. */
  private async openArchiveEntry(item: Item): Promise<void> {
    try {
      const tab = this.requireTab();
      const dir = currentDir(tab);
      const archiveId = dir.dirId.slice("archive:".length).split(":")[0];
      const entryPath = item.id.split(":").slice(2).join(":");
      const archivePath = await api.getItemPath(archiveId);
      const temp = await api.extractArchiveEntry(archivePath, entryPath);
      await api.openPath(temp);
    } catch (err) {
      Toast.error(`Could not open ${item.name}: ${err}`);
    }
  }

  private onSelect(item: Item | null): void {
    void item;
    this.renderStatus();
    this.renderCrumbs();
  }

  private togglePreview(): void {
    this.quickLook.toggle(this.activeSelected());
  }

  /** The selection of whichever view is on screen. */
  private activeSelected(): Item | null {
    const tab = this.activeTab;
    if (!tab) return null;
    if (tab.view === "columns") return this.columnsView.selected;
    if (tab.view === "gallery") return this.galleryView.selected;
    return this.fileView.selected;
  }

  /** Move an item to the Trash, refresh the listing, and say so. */
  private async deleteItem(item: Item): Promise<void> {
    try {
      const trashName = await api.trashItem(item.id);
      this.undoStack.push({ kind: "trash", trashName, label: item.name });
      Toast.success(`Moved “${item.name}” to Trash. Super+Z to undo.`);
      this.quickLook.close();
      const tab = this.activeTab;
      if (tab) await this.loadDir(tab);
    } catch (err) {
      Toast.error(`Could not delete “${item.name}”: ${err}`);
    }
  }

  /** Move a whole selection to the Trash: one toast, one refresh. */
  private async trashItems(items: Item[]): Promise<void> {
    let ok = 0;
    for (const it of items) {
      try {
        const trashName = await api.trashItem(it.id);
        this.undoStack.push({ kind: "trash", trashName, label: it.name });
        ok++;
      } catch {
        // counted below
      }
    }
    if (ok === 1)
      Toast.success(`Moved “${items[0].name}” to Trash. Super+Z to undo.`);
    else if (ok > 1)
      Toast.success(`Moved ${ok} items to Trash. Super+Z to undo.`);
    if (ok < items.length)
      Toast.error(`Could not trash ${items.length - ok} item(s).`);
    this.quickLook.close();
    const t = this.activeTab;
    if (t) await this.loadDir(t);
  }

  /** If Quick Look is showing this path, re-open it on the fresh Item
   * (post rotate/rename) so the pane never displays a stale photo. */
  private refreshQuickLook(path: string | null | undefined): void {
    if (!path || !this.quickLook.isOpen) return;
    if (this.quickLook.item?.provider_key !== path) return;
    const fresh = this.activeTab?.items.find((i) => i.provider_key === path);
    if (fresh) void this.quickLook.open(fresh);
  }

  private async renameInline(item: Item, newName: string): Promise<void> {
    try {
      await api.renameItem(item.id, newName);
      const t = this.activeTab;
      if (t) await this.loadDir(t);
      this.refreshQuickLook(item.provider_key);
    } catch (err) {
      Toast.error(`Rename failed: ${err}`);
      const t = this.activeTab;
      if (t) await this.loadDir(t);
    }
  }

  // ------------------------------------------------------- file actions

  /** Selection of the active view as a list (multi when available). */
  private activeSelectionItems(): Item[] {
    const tab = this.activeTab;
    if (!tab) return [];
    if (tab.view === "grid" || tab.view === "list") {
      return this.fileView.selectionItems();
    }
    const sel = this.activeSelected();
    return sel ? [sel] : [];
  }

  private refreshActiveDir(): void {
    const t = this.activeTab;
    if (t) void this.loadDir(t);
  }

  private async clipboardSet(items: Item[], cut: boolean): Promise<void> {
    const paths = items
      .map((i) => i.provider_key)
      .filter((p): p is string => !!p);
    if (paths.length === 0) {
      Toast.error("Nothing to copy — these items have no local path.");
      return;
    }
    try {
      await api.clipboardWrite(paths, cut);
      Toast.success(
        `${cut ? "Cut" : "Copied"} ${paths.length} item${paths.length === 1 ? "" : "s"}`,
      );
    } catch (err) {
      Toast.error(`Clipboard failed: ${err}`);
    }
  }

  private async pasteIntoCurrentDir(): Promise<void> {
    const tab = this.activeTab;
    if (!tab) return;
    const dir = currentDir(tab);
    if (dir.dirId.startsWith("label:") || dir.dirId.startsWith("archive:")) {
      Toast.error("Can't paste into this view.");
      return;
    }
    try {
      const destDir = await api.getItemPath(dir.dirId);
      const clip = await api.clipboardRead();
      if (clip.paths.length === 0) {
        Toast.error("Clipboard has no files.");
        return;
      }
      let done = 0;
      for (const src of clip.paths) {
        const parent = src.replace(/\/[^/]*$/, "");
        if (parent === destDir) {
          if (clip.cut) continue; // cutting into the same folder: no-op
          // copy into the same folder = duplicate with a suffix (backend)
        }
        if (clip.cut) await api.movePathTo(src, destDir);
        else await api.copyPathTo(src, destDir);
        done++;
      }
      if (done > 0) {
        Toast.success(
          `Pasted ${done} item${done === 1 ? "" : "s"}${clip.cut ? " (moved)" : ""}`,
        );
        this.refreshActiveDir();
      }
    } catch (err) {
      Toast.error(`Paste failed: ${err}`);
    }
  }

  private async duplicateItems(items: Item[]): Promise<void> {
    let done = 0;
    for (const item of items) {
      if (!item.provider_key) continue;
      const parent = item.provider_key.replace(/\/[^/]*$/, "");
      try {
        await api.copyPathTo(item.provider_key, parent);
        done++;
      } catch (err) {
        Toast.error(`Could not duplicate ${item.name}: ${err}`);
      }
    }
    if (done > 0) {
      Toast.success(`Duplicated ${done} item${done === 1 ? "" : "s"}`);
      this.refreshActiveDir();
    }
  }

  /** Shared destination-picker flow for Move to… / Copy to…. */
  private async moveOrCopyTo(items: Item[], mode: "move" | "copy"): Promise<void> {
    const names = ["Home", "Documents", "Downloads", "Videos", "Pictures", "Music", "Trash"];
    const targets = await this.resolveFavoriteTargets();
    const shortcuts: PickerShortcut[] = [];
    names.forEach((n, i) => {
      const t = targets[i];
      if (t && n !== "Trash") {
        shortcuts.push({ label: n, locationId: t.locationId, dirId: t.dirId });
      }
    });
    const tab = this.activeTab;
    const dir = tab ? currentDir(tab) : null;
    const start =
      dir && !dir.dirId.startsWith("label:") && !dir.dirId.startsWith("archive:")
        ? { locationId: dir.locationId, dirId: dir.dirId }
        : shortcuts[0];
    if (!start) {
      Toast.error("No destination available.");
      return;
    }
    const dest = await this.picker.pick(
      mode === "move" ? "Move to…" : "Copy to…",
      shortcuts,
      start,
    );
    if (!dest) return;
    let done = 0;
    for (const item of items) {
      try {
        if (mode === "move") {
          const newPath = await api.moveItemTo(item.id, dest);
          const fromDir = item.provider_key
            ? item.provider_key.replace(/\/[^/]*$/, "")
            : "";
          if (fromDir && fromDir !== dest) {
            this.undoStack.push({ kind: "move", newPath, fromDir, label: item.name });
          }
        } else {
          if (!item.provider_key) continue;
          await api.copyPathTo(item.provider_key, dest);
        }
        done++;
      } catch (err) {
        Toast.error(`Could not ${mode} ${item.name}: ${err}`);
      }
    }
    if (done > 0) {
      Toast.success(
        `${mode === "move" ? "Moved" : "Copied"} ${done} item${done === 1 ? "" : "s"}${mode === "move" ? ". Super+Z to undo." : ""}`,
      );
      this.refreshActiveDir();
    }
  }

  private async toggleLabel(items: Item[], label: string, on: boolean): Promise<void> {
    let done = 0;
    for (const item of items) {
      if (!item.provider_key) continue;
      try {
        await api.setPathLabel(item.provider_key, label, on);
        done++;
      } catch (err) {
        Toast.error(`Label failed for ${item.name}: ${err}`);
      }
    }
    if (done > 0) {
      Toast.success(
        `${on ? "Labeled" : "Unlabeled"} ${done} item${done === 1 ? "" : "s"}: ${label}`,
      );
      this.refreshActiveDir();
    }
  }

  private async extractArchive(item: Item, toDir: boolean): Promise<void> {
    try {
      let dest: string | null = null;
      if (toDir) {
        const names = ["Home", "Documents", "Downloads", "Videos", "Pictures", "Music", "Trash"];
        const targets = await this.resolveFavoriteTargets();
        const shortcuts: PickerShortcut[] = [];
        names.forEach((n, i) => {
          const t = targets[i];
          if (t && n !== "Trash") {
            shortcuts.push({ label: n, locationId: t.locationId, dirId: t.dirId });
          }
        });
        const tab = this.activeTab;
        const dir = tab ? currentDir(tab) : null;
        const start = dir
          ? { locationId: dir.locationId, dirId: dir.dirId }
          : shortcuts[0];
        if (!start) return;
        dest = await this.picker.pick("Extract to…", shortcuts, start);
        if (!dest) return;
      }
      const out = await api.extractArchive(item.id, dest ?? undefined);
      Toast.success(`Extracted to ${out}`);
      this.refreshActiveDir();
    } catch (err) {
      Toast.error(`Extract failed: ${err}`);
    }
  }

  private openLabel(name: string): void {
    const dirId = `label:${name}`;
    this.navigateTo({
      locationId: "",
      dirId,
      crumbs: [{ dirId, name: `🏷 ${name}` }],
    });
  }

  private static loadSmartFolders(): SmartFolder[] {
    try {
      const raw = localStorage.getItem("ff-smart-folders");
      if (raw) {
        const parsed = JSON.parse(raw) as SmartFolder[];
        if (Array.isArray(parsed)) return parsed;
      }
    } catch {
      // no stored folders
    }
    return [];
  }

  private static saveSmartFolders(list: SmartFolder[]): void {
    try {
      localStorage.setItem("ff-smart-folders", JSON.stringify(list));
    } catch {
      // storage unavailable
    }
  }

  private async saveSmartFolder(): Promise<void> {
    const query = this.searchView.currentQuery;
    const type = this.searchView.currentTypeFilter;
    if (!query && !type) {
      Toast.error("Type a search or pick a type filter first.");
      return;
    }
    const name = await promptDialog(
      "Name this Smart Folder",
      "e.g. Movie files",
      query || type || "",
    );
    if (!name) return;
    const list = Shell.loadSmartFolders();
    list.push({ name, query, type });
    Shell.saveSmartFolders(list);
    void this.buildSidebar();
    Toast.success(`Smart Folder “${name}” saved`);
  }

  private static loadFavOrder(): string[] {
    try {
      const raw = localStorage.getItem("ff-fav-order");
      if (raw) {
        const parsed = JSON.parse(raw) as string[];
        if (Array.isArray(parsed)) return parsed;
      }
    } catch {
      // no stored order
    }
    return [];
  }

  private reorderFavorite(from: string, to: string): void {
    const base = ["Home", "Documents", "Downloads", "Videos", "Pictures", "Music", "Trash"];
    const current = Shell.loadFavOrder();
    const order = current.length > 0 ? [...current] : [...base];
    for (const n of [from, to]) {
      if (!order.includes(n)) order.push(n);
    }
    order.splice(order.indexOf(from), 1);
    order.splice(order.indexOf(to), 0, from);
    try {
      localStorage.setItem("ff-fav-order", JSON.stringify(order));
    } catch {
      // storage unavailable
    }
    void this.buildSidebar();
  }

  /** The user's home directory path (via the Home location). */
  private async homePath(): Promise<string> {
    const home =
      this.locations.find((l) =>
        l.display_name.toLowerCase().includes("home"),
      ) ?? this.locations[0];
    if (!home) return "";
    return api.getItemPath(home.root_item).catch(() => "");
  }

  /** New Folder in the folder being browsed (background menu). */
  private async createFolderHere(): Promise<void> {
    const tab = this.activeTab;
    if (!tab) return;
    const dir = currentDir(tab);
    if (dir.dirId.startsWith("label:") || dir.dirId.startsWith("archive:")) {
      Toast.error("Can't create a folder here — this is a virtual view.");
      return;
    }
    try {
      const path = await api.createFolderIn(dir.dirId);
      Toast.success(`Created ${path.split("/").pop()}`);
      this.refreshActiveDir();
    } catch (err) {
      Toast.error(`Create folder failed: ${err}`);
    }
  }

  /** New blank file (background menu → New File). */
  private async createFileHere(kind: string): Promise<void> {
    const tab = this.activeTab;
    if (!tab) return;
    const dir = currentDir(tab);
    if (dir.dirId.startsWith("label:") || dir.dirId.startsWith("archive:")) {
      Toast.error("Can't create a file here — this is a virtual view.");
      return;
    }
    try {
      const path = await api.createNewFile(dir.dirId, kind);
      Toast.success(`Created ${path.split("/").pop()}`);
      this.refreshActiveDir();
    } catch (err) {
      Toast.error(`Create file failed: ${err}`);
    }
  }

  /** Absolute path of the folder the active tab is browsing. */
  private async currentDirPath(): Promise<string> {
    const tab = this.activeTab;
    if (!tab) return "";
    const dir = currentDir(tab);
    if (dir.dirId.startsWith("label:") || dir.dirId.startsWith("archive:")) {
      return "";
    }
    return api.getItemPath(dir.dirId).catch(() => "");
  }

  private async refreshLocations(): Promise<void> {
    this.locations = await api.listLocations().catch(() => this.locations);
    await this.buildSidebar();
  }

  /** Mount an ISO item and open it as a sidebar location. */
  private async mountIsoItem(item: Item): Promise<void> {
    try {
      const mountPath = await api.mountIso(item.id);
      await this.addAndOpenLocation(
        item.name.replace(/\.(iso|img|dmg|cdi|nrg|bin)$/i, ""),
        mountPath,
      );
      Toast.success(`Mounted “${item.name}”.`);
    } catch (err) {
      Toast.error(`Mount failed: ${err}`);
    }
  }

  /** Register a path as a sidebar location and navigate into it. */
  private async addAndOpenLocation(
    name: string,
    path: string,
  ): Promise<void> {
    const summary = await api.addPathLocation(name, path);
    await this.refreshLocations();
    this.navigateTo({
      locationId: summary.id,
      dirId: summary.root_item,
      crumbs: [{ dirId: summary.root_item, name: summary.display_name }],
    });
  }

  private async openSyncHere(): Promise<void> {
    const path = (await this.currentDirPath()) || (await this.homePath());
    this.syncDialog.onDone = () => this.refreshActiveDir();
    this.syncDialog.open(path);
  }

  private async openDupHere(): Promise<void> {
    const path = (await this.currentDirPath()) || (await this.homePath());
    if (!path) {
      Toast.error("Open a folder first, then hunt duplicates in it.");
      return;
    }
    this.dupDialog.onDone = () => this.refreshActiveDir();
    this.dupDialog.open(path);
  }

  /** Built-in terminal: opens in the folder being browsed. */
  private async toggleTerminal(): Promise<void> {
    let cwd = "";
    const tab = this.activeTab;
    if (tab) {
      const dir = currentDir(tab);
      if (!dir.dirId.startsWith("label:") && !dir.dirId.startsWith("archive:")) {
        cwd = await api.getItemPath(dir.dirId).catch(() => "");
      }
    }
    if (!cwd) {
      const home = this.locations.find((l) =>
        l.display_name.toLowerCase().includes("home"),
      );
      if (home) cwd = await api.getItemPath(home.root_item).catch(() => "");
    }
    await this.termPanel.toggle(cwd || "/");
  }

  /** Connect to Server (Finder): gio-mounts sftp/smb/ftp/dav and adds
   * the mount as a browsable location. */
  private async connectToServer(): Promise<void> {
    const url = await promptDialog(
      "Connect to Server",
      "sftp://user@server/share · smb://server/share · ftp://… · dav://…",
    );
    if (!url) return;
    try {
      Toast.success("Mounting… (a sign-in prompt may appear)");
      const mountPath = await api.mountServer(url);
      const name = url
        .replace(/^[a-z]+:\/\//, "")
        .split("/")[0] || "Server";
      const summary = await api.addPathLocation(name, mountPath);
      this.locations = await api.listLocations().catch(() => this.locations);
      void summary;
      void this.buildSidebar();
      this.navigateTo({
        locationId: summary.id,
        dirId: summary.root_item,
        crumbs: [{ dirId: summary.root_item, name: summary.display_name }],
      });
      Toast.success(`Connected: ${summary.display_name}`);
    } catch (err) {
      Toast.error(`Connect failed: ${err}`);
    }
  }

  // ------------------------------------------------------------- breadcrumbs

  private renderCrumbs(): void {
    this.crumbBar.innerHTML = "";
    const tab = this.activeTab;
    if (!tab) return;
    const crumbs = currentDir(tab).crumbs;
    crumbs.forEach((c, i) => {
      if (i > 0) {
        const sep = document.createElement("span");
        sep.className = "crumb-sep";
        sep.textContent = "/";
        sep.setAttribute("aria-hidden", "true");
        this.crumbBar.appendChild(sep);
      }
      const b = document.createElement("button");
      b.className = "crumb" + (i === crumbs.length - 1 ? " current" : "");
      b.textContent = c.name;
      b.setAttribute("aria-label", `Go to ${c.name}`);
      b.addEventListener("click", () => {
        const next = goToCrumb(tab, i);
        if (next) {
          this.quickLook.close();
          void this.loadDir(tab);
        }
      });
      this.crumbBar.appendChild(b);
    });
    // The selected file appears as the final, current breadcrumb element.
    const selForCrumb = this.activeSelected();
    if (selForCrumb) {
      const sep = document.createElement("span");
      sep.className = "crumb-sep";
      sep.textContent = "/";
      sep.setAttribute("aria-hidden", "true");
      this.crumbBar.appendChild(sep);
      const cur = document.createElement("span");
      cur.className = "crumb crumb-file current";
      cur.textContent = selForCrumb.name;
      cur.title = selForCrumb.provider_key ?? selForCrumb.name;
      this.crumbBar.appendChild(cur);
      // The last folder crumb is no longer the "current" one.
      this.crumbBar.querySelectorAll(".crumb.current").forEach((el) => {
        if (el !== cur) el.classList.remove("current");
      });
    }
    // Clicking anywhere on the crumb bar edits the path
    this.crumbBar.addEventListener("click", (e) => {
      // Don't trigger if clicking the copy button or a crumb button
      const t = e.target as HTMLElement;
      if (t.closest(".crumb-copy") || t.closest("button")) return;
      this.editPath();
    });
  }

  private displayPath(): string {
    const tab = this.activeTab;
    if (!tab) return "";
    const crumbs = currentDir(tab).crumbs.map((c) => c.name).join(" / ");
    // Append selected file name if one is selected
    const sel = this.activeSelected();
    if (sel) return crumbs + " / " + sel.name;
    return crumbs;
  }

  private editPath(): void {
    const tab = this.activeTab;
    if (!tab) return;
    const input = document.createElement("input");
    input.className = "crumb-input";
    input.value = this.displayPath();
    input.setAttribute("aria-label", "Type a path and press Enter");
    const commit = async () => {
      const value = input.value;
      this.renderCrumbs();
      await this.jumpToPath(value);
    };
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter") void commit();
      else if (e.key === "Escape") this.renderCrumbs();
      e.stopPropagation();
    });
    input.addEventListener("blur", () => this.renderCrumbs());
    this.crumbBar.innerHTML = "";
    this.crumbBar.appendChild(input);
    input.focus();
    input.select();
  }

  /**
   * Jump to a typed path: match a location by display name, then walk path
   * segments with list_dir, descending into same-named folders. Best-effort —
   * the backend has no resolve-path command yet.
   */
  private async jumpToPath(raw: string): Promise<void> {
    const cleaned = raw.replace(/^\/+|\/+$/g, "");
    if (!cleaned) return;
    const parts = cleaned.split("/").map((p) => p.trim()).filter(Boolean);
    if (parts.length === 0) return;
    for (const loc of this.locations) {
      const head = parts[0].toLocaleLowerCase();
      const name = loc.display_name.toLocaleLowerCase();
      const matches = name === head || name.startsWith(head);
      if (!matches) continue;
      const crumbs = [{ dirId: loc.root_item, name: loc.display_name }];
      let dirId = loc.root_item;
      let ok = true;
      for (const seg of parts.slice(1)) {
        let items: Item[] = [];
        try {
          items = await api.listDir(loc.id, dirId);
        } catch {
          ok = false;
          break;
        }
        const hit = items.find(
          (it) => it.kind === "folder" && it.name.toLocaleLowerCase() === seg.toLocaleLowerCase(),
        );
        if (!hit) {
          ok = false;
          break;
        }
        dirId = hit.id;
        crumbs.push({ dirId: hit.id, name: hit.name });
      }
      if (ok) {
        this.navigateTo({ locationId: loc.id, dirId, crumbs });
        return;
      }
    }
    this.flashStatus(`No match for “${raw}”.`);
  }

  // ---------------------------------------------------------------- sidebar

  private async buildSidebar(): Promise<void> {
    this.favList.innerHTML = "";
    this.sidebarList.innerHTML = "";
    const targets = await this.resolveFavoriteTargets();
    const names = ["Home", "Documents", "Downloads", "Videos", "Pictures", "Music", "Trash"];
    // Saved drag order first; favorites whose folder doesn't exist are
    // OMITTED, never shown disabled (Dickles' rule).
    const order = Shell.loadFavOrder();
    const orderIdx = (n: string) => {
      const i = order.indexOf(n);
      return i < 0 ? 999 : i;
    };
    const pairs = names
      .map((n, i) => ({ name: n, target: targets[i] }))
      .filter((p) => p.target)
      .sort((a, b) => orderIdx(a.name) - orderIdx(b.name));
    for (const pair of pairs) {
      const name = pair.name;
      const target = pair.target!;
      const b = document.createElement("button");
      b.className = "side-item";
      b.textContent = name;
      b.title = target.display;
      {
        // Drag to rearrange the sidebar (persisted order).
        b.draggable = true;
        b.addEventListener("dragstart", (e) => {
          dragState.favName = name;
          if (e.dataTransfer) e.dataTransfer.setData("text/plain", name);
        });
        b.addEventListener("dragend", () => {
          dragState.favName = null;
          b.classList.remove("drop-hover");
        });
        b.addEventListener("click", () => {
          this.navigateTo({
            locationId: target.locationId,
            dirId: target.dirId,
            crumbs: [{ dirId: target.dirId, name: target.display }],
          });
        });
        // Drop target: drag a file here to file it (or trash it).
        b.addEventListener("dragover", (e) => {
          if (dragState.favName) {
            if (dragState.favName !== name) {
              e.preventDefault();
              b.classList.add("drop-hover");
            }
            return;
          }
          if (!dragState.item) return;
          e.preventDefault();
          if (e.dataTransfer) e.dataTransfer.dropEffect = "move";
          b.classList.add("drop-hover");
        });
        b.addEventListener("dragleave", () => b.classList.remove("drop-hover"));
        b.addEventListener("drop", async (e) => {
          e.preventDefault();
          b.classList.remove("drop-hover");
          if (dragState.favName) {
            this.reorderFavorite(dragState.favName, name);
            dragState.favName = null;
            return;
          }
          const dragged = dragState.item;
          if (!dragged) return;
          try {
            if (name === "Trash") {
              const trashName = await api.trashItem(dragged.id);
              this.undoStack.push({ kind: "trash", trashName, label: dragged.name });
              Toast.success(`Moved “${dragged.name}” to Trash. Super+Z to undo.`);
            } else {
              const dest = await api.getItemPath(target.dirId);
              if (dragged.provider_key && dragged.provider_key.replace(/\/[^/]*$/, "") === dest) {
                return; // already in this folder
              }
              const newPath = await api.moveItemTo(dragged.id, dest);
              const fromDir = dragged.provider_key
                ? dragged.provider_key.replace(/\/[^/]*$/, "")
                : "";
              if (fromDir) {
                this.undoStack.push({ kind: "move", newPath, fromDir, label: dragged.name });
              }
              Toast.success(`Moved “${dragged.name}” to ${name}. Super+Z to undo.`);
            }
            this.quickLook.close();
            const tab = this.activeTab;
            if (tab) await this.loadDir(tab);
          } catch (err) {
            Toast.error(`Could not move “${dragged.name}”: ${err}`);
          }
        });
        // Trash gets a right-click menu: Empty / Secure Delete
        if (name === "Trash") {
          b.addEventListener("contextmenu", (e) => {
            e.preventDefault();
            e.stopPropagation();
            this.ctxMenu.open(e.clientX, e.clientY, [
              {
                label: "Empty Trash",
                action: async () => {
                  const sum = await api.trashSummary().catch(() => null);
                  const ok = await confirmDialog(
                    "Empty Trash?",
                    sum
                      ? `This will permanently delete ${sum.items} item${sum.items === 1 ? "" : "s"} (${sum.files} file${sum.files === 1 ? "" : "s"} including folder contents). This cannot be undone.`
                      : "This will permanently delete all items in the trash. This cannot be undone.",
                    "Empty Trash",
                    true
                  );
                  if (ok) {
                    try {
                      const r = await api.emptyTrash();
                      Toast.success(`Trash emptied — ${r.items} item${r.items === 1 ? "" : "s"} (${r.files} file${r.files === 1 ? "" : "s"}) deleted${r.errors > 0 ? ` (${r.errors} errors)` : ""}.`);
                      // Refresh the view if we're looking at trash
                      this.renderCrumbs();
                    } catch (err) {
                      Toast.error(`Failed to empty trash: ${err}`);
                    }
                  }
                },
              },
              {
                label: "Secure Delete (overwrite)",
                action: async () => {
                  const sum = await api.trashSummary().catch(() => null);
                  const ok = await confirmDialog(
                    "Secure Delete?",
                    sum
                      ? `${sum.items} item${sum.items === 1 ? "" : "s"} (${sum.files} file${sum.files === 1 ? "" : "s"} including folder contents) will be overwritten with zeros before deletion. This cannot be undone.`
                      : "Files will be overwritten with zeros before deletion. This cannot be undone.",
                    "Secure Delete",
                    true
                  );
                  if (ok) {
                    try {
                      const r = await api.secureEmptyTrash();
                      Toast.success(`Securely deleted ${r.items} item${r.items === 1 ? "" : "s"} (${r.files} file${r.files === 1 ? "" : "s"} overwritten)${r.errors > 0 ? ` (${r.errors} errors)` : ""}.`);
                    } catch (err) {
                      alert(`Failed to secure-delete: ${err}`);
                    }
                  }
                },
              },
            ]);
          });
        }
      }
      this.favList.appendChild(b);
    }
    for (const loc of this.locations) {
      const b = document.createElement("button");
      b.className = "side-item";
      const name = document.createElement("span");
      name.textContent = loc.display_name;
      const prov = document.createElement("span");
      prov.className = "side-provider";
      prov.textContent = loc.provider;
      b.append(name, prov);
      b.addEventListener("click", () => {
        this.navigateTo({
          locationId: loc.id,
          dirId: loc.root_item,
          crumbs: [{ dirId: loc.root_item, name: loc.display_name }],
        });
      });
      b.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        e.stopPropagation();
        void (async () => {
          const rootPath = await api
            .getItemPath(loc.root_item)
            .catch(() => "");
          const menuItems: MenuItem[] = [
            {
              label: "Disk Utility…",
              action: () =>
                void this.diskDialog.open(rootPath || undefined),
            },
          ];
          const home = await this.homePath().catch(() => "");
          if (rootPath && rootPath !== "/" && rootPath !== home) {
            menuItems.push({
              label: "Unmount / Eject",
              action: () => {
                void api
                  .unmountPath(rootPath)
                  .then(async () => {
                    Toast.success(`Unmounted ${loc.display_name}.`);
                    await this.refreshLocations();
                  })
                  .catch((err) => Toast.error(`Unmount failed: ${err}`));
              },
            });
          }
          this.ctxMenu.open(e.clientX, e.clientY, menuItems);
        })();
      });
      this.sidebarList.appendChild(b);
    }
    if (this.locations.length === 0) {
      const p = document.createElement("p");
      p.className = "side-empty";
      p.textContent = "No locations yet.";
      this.sidebarList.appendChild(p);
    }
    // Labels: click to browse everything carrying the label.
    this.labelList.innerHTML = "";
    for (const def of loadLabelDefs()) {
      const b = document.createElement("button");
      b.className = "side-item";
      const dot = document.createElement("span");
      dot.className = "side-dot";
      dot.style.background = def.color;
      const label = document.createElement("span");
      label.textContent = def.name;
      b.append(dot, label);
      b.addEventListener("click", () => this.openLabel(def.name));
      b.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        e.stopPropagation();
        this.ctxMenu.open(e.clientX, e.clientY, [
          {
            label: "Manage Labels…",
            action: () =>
              this.labelManager.open(() => {
                void this.buildSidebar();
                this.refreshActiveDir();
              }),
          },
        ]);
      });
      this.labelList.appendChild(b);
    }
    // Smart folders: saved searches.
    this.smartList.innerHTML = "";
    const folders = Shell.loadSmartFolders();
    if (folders.length === 0) {
      const p = document.createElement("p");
      p.className = "side-empty";
      p.textContent = "Run a search and save it here.";
      this.smartList.appendChild(p);
    }
    for (const sf of folders) {
      const b = document.createElement("button");
      b.className = "side-item";
      b.textContent = `☆ ${sf.name}`;
      b.title = sf.query || sf.type || "";
      b.addEventListener("click", () => {
        void this.searchView.runExternal(sf.query, sf.type);
      });
      b.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        e.stopPropagation();
        this.ctxMenu.open(e.clientX, e.clientY, [
          {
            label: `Remove "${sf.name}"`,
            danger: true,
            action: () => {
              Shell.saveSmartFolders(
                Shell.loadSmartFolders().filter((f) => f !== sf),
              );
              void this.buildSidebar();
            },
          },
        ]);
      });
      this.smartList.appendChild(b);
    }
  }

  /**
   * Favorites resolve to real locations first; Documents/Downloads fall back
   * to same-named folders inside the Home location (the common
   * single-location setup), so they stay clickable instead of grayed out.
   */
  /**
   * Fuzzy folder name candidates — not everyone names them the same.
   * First existing match wins.
   */
  private static readonly FUZZY_FAVORITES: Record<string, string[]> = {
    Videos: ["Videos", "Movies", "Video", "Films"],
    Pictures: ["Pictures", "Photos", "Images", "Pics"],
    Music: ["Music", "Audio", "Songs"],
  };

  private async resolveFavoriteTargets(): Promise<(FavoriteTarget | null)[]> {
    const homeLoc =
      this.locations.find((l) => l.display_name.toLowerCase().includes("home")) ??
      this.locations.find((l) => l.provider === "local") ??
      this.locations[0] ??
      null;
    const out: (FavoriteTarget | null)[] = [];
    for (const name of ["Home", "Documents", "Downloads", "Videos", "Pictures", "Music", "Trash"]) {
      if (name == "Trash") {
        out.push(await this.resolveTrashTarget(homeLoc));
        continue;
      }
      const direct = this.findFavorite(name);
      if (direct) {
        out.push({
          locationId: direct.id,
          dirId: direct.root_item,
          display: direct.display_name,
        });
        continue;
      }
      let sub: FavoriteTarget | null = null;
      if (homeLoc && name !== "Home") {
        try {
          const kids = await api.listDir(homeLoc.id, homeLoc.root_item);
          const candidates = Shell.FUZZY_FAVORITES[name] ?? [name];
          const kid = kids.find(
            (k) =>
              k.kind === "folder" &&
              candidates.some((c) => k.name.toLowerCase() === c.toLowerCase()),
          );
          if (kid) {
            sub = { locationId: homeLoc.id, dirId: kid.id, display: kid.name };
          }
        } catch {
          sub = null;
        }
      }
      out.push(sub);
    }
    return out;
  }

  /**
   * Resolve the OS trash folder by traversing from Home:
   * ~/.local/share/Trash/files (Linux) — no backend changes needed.
   */
  private async resolveTrashTarget(homeLoc: LocationSummary | null): Promise<FavoriteTarget | null> {
    if (!homeLoc) return null;
    try {
      const segments = [".local", "share", "Trash", "files"];
      let dirId = homeLoc.root_item;
      let display = "Trash";
      for (const seg of segments) {
        const kids = await api.listDir(homeLoc.id, dirId);
        const kid = kids.find((k) => k.kind === "folder" && k.name === seg);
        if (!kid) return null;
        dirId = kid.id;
        if (seg === "files") display = kid.name;
      }
      return { locationId: homeLoc.id, dirId, display: "Trash" };
    } catch {
      return null;
    }
  }

  /** Favorites resolve to real locations via list_locations. */
  private findFavorite(name: string): LocationSummary | null {
    const want = name.toLocaleLowerCase();
    return (
      this.locations.find((l) =>
        l.display_name.toLocaleLowerCase().includes(want),
      ) ??
      this.locations.find((l) => l.provider.toLocaleLowerCase().includes(want)) ??
      null
    );
  }

  /** Adjust UI font scale (0.7 to 2.0). Persists for the session. */
  private setFontScale(scale: number): void {
    this.fontScale = Math.min(2.0, Math.max(0.7, Math.round(scale * 10) / 10));
    document.documentElement.style.setProperty("--ufm-font-scale", String(this.fontScale));
    // Drive the scale from the root font size so ALL rem-sized text
    // follows — sidebar, dialogs, and every file/folder name in every
    // view. (The old approach scaled only #app + two selectors, which
    // is why file labels ignored Ctrl +/-.)
    document.documentElement.style.fontSize = `${16 * this.fontScale}px`;
  }

  // ------------------------------------------------------------ palette

  /** Build the command list and open the palette (spec: launcher sheet). */
  private async openPalette(): Promise<void> {
    const commands: PaletteCommand[] = [];
    // Places: favorites first, then locations.
    const names = ["Home", "Documents", "Downloads", "Videos", "Pictures", "Music", "Trash"];
    const targets = await this.resolveFavoriteTargets();
    names.forEach((name, i) => {
      const target = targets[i];
      if (!target) return;
      commands.push({
        title: `Go to ${name}`,
        hint: target.display,
        run: () =>
          this.navigateTo({
            locationId: target.locationId,
            dirId: target.dirId,
            crumbs: [{ dirId: target.dirId, name: target.display }],
          }),
      });
    });
    for (const loc of this.locations) {
      commands.push({
        title: `Go to ${loc.display_name}`,
        hint: loc.provider,
        run: () =>
          this.navigateTo({
            locationId: loc.id,
            dirId: loc.root_item,
            crumbs: [{ dirId: loc.root_item, name: loc.display_name }],
          }),
      });
    }
    // Views.
    const viewCmd = (label: string, m: ViewMode): PaletteCommand => ({
      title: `${label} view`,
      run: () => this.setView(m),
    });
    commands.push(
      viewCmd("Grid", "grid"),
      viewCmd("List", "list"),
      viewCmd("Columns", "columns"),
      viewCmd("Gallery", "gallery")
    );
    // Actions.
    commands.push(
      { title: "Copy path", run: () => void this.copyCurrentPath() },
      { title: "Undo last delete or move", hint: "Super+Z", run: () => void this.undoLast() },
      { title: "Larger text", run: () => this.setFontScale(this.fontScale + 0.1) },
      { title: "Smaller text", run: () => this.setFontScale(this.fontScale - 0.1) },
      { title: "About Fable Files", run: () => this.aboutDialog.open() },
      {
        title: "Report a bug…",
        run: () =>
          void api.openUrl(
            "https://github.com/garrisonr/fable-files/issues/new?title=" +
              encodeURIComponent("[Bug] ") +
              "&labels=bug"
          ),
      },
      {
        title: "Request a feature…",
        run: () =>
          void api.openUrl(
            "https://github.com/garrisonr/fable-files/issues/new?title=" +
              encodeURIComponent("[Feature] ") +
              "&labels=enhancement"
          ),
      }
    );
    this.palette.open(commands);
  }

  // -------------------------------------------------------------- view mode

  private setView(mode: ViewMode): void {
    const tab = this.requireTab();
    if (mode === "grid" || mode === "list") {
      // FileView owns the grid<->list crossfade and stamps tab.view itself.
      tab.view = mode === "grid" ? "list" : "grid"; // force the switch path
      this.columnsView.el.hidden = true;
      this.galleryView.el.hidden = true;
      this.fileView.el.hidden = false;
      this.fileView.switchView(mode);
    } else {
      tab.view = mode;
      this.fileView.el.hidden = true;
      this.columnsView.el.hidden = mode !== "columns";
      this.galleryView.el.hidden = mode !== "gallery";
      if (mode === "columns") this.columnsView.reset(tab);
      else this.galleryView.render();
    }
    this.renderStatus();
  }

  /** Show + render whichever view the active tab is using. */
  private renderActiveView(): void {
    const tab = this.activeTab;
    if (!tab) return;
    const mode = tab.view;
    this.fileView.el.hidden = mode === "columns" || mode === "gallery";
    this.columnsView.el.hidden = mode !== "columns";
    this.galleryView.el.hidden = mode !== "gallery";
    if (mode === "columns") this.columnsView.reset(tab);
    else if (mode === "gallery") this.galleryView.render();
    else this.fileView.render();
  }

  /** Copy the current path (selected file's real path when one is picked). */
  private async copyCurrentPath(btn?: HTMLButtonElement): Promise<void> {
    try {
      const sel = this.activeSelected();
      let path: string;
      if (sel?.provider_key) {
        path = sel.provider_key;
      } else {
        const tab = this.activeTab;
        if (!tab) return;
        const dir = currentDir(tab);
        path = await api.getItemPath(dir.dirId);
        if (sel) path = path.replace(/\/$/, "") + "/" + sel.name;
      }
      await navigator.clipboard.writeText(path);
      if (btn) {
        btn.textContent = "\u2713";
        setTimeout(() => {
          btn.textContent = "\u29C9";
        }, 1000);
      }
      Toast.success("Path copied");
    } catch (err) {
      Toast.error(`Copy failed: ${err}`);
    }
  }

  /** The hamburger menu: views, sort, text size, theme, help. */
  private openMainMenu(): void {
    const tab = this.activeTab;
    const mode = tab?.view ?? "grid";
    const sort = tab?.sort ?? "name";
    const rect = this.menuBtn.getBoundingClientRect();
    const check = (on: boolean) => (on ? "✓ " : "   ");
    const viewItem = (label: string, m: ViewMode) => ({
      label: check(mode === m) + label,
      action: () => this.setView(m),
    });
    const sortItem = (label: string, key: "name" | "size" | "modified") => ({
      label: check(sort === key) + `Sort by ${label}`,
      action: () => {
        this.fileView.sortBy(key);
        this.renderActiveView();
      },
    });
    this.ctxMenu.open(rect.left - 140, rect.bottom + 6, [
      viewItem("Grid view", "grid"),
      viewItem("List view", "list"),
      viewItem("Columns view", "columns"),
      viewItem("Gallery view", "gallery"),
      sortItem("Name", "name"),
      sortItem("Size", "size"),
      sortItem("Date", "modified"),
      { label: "Larger text", shortcut: "Ctrl +", action: () => this.setFontScale(this.fontScale + 0.1) },
      { label: "Smaller text", shortcut: "Ctrl −", action: () => this.setFontScale(this.fontScale - 0.1) },
      { label: "Terminal", shortcut: "Ctrl `", action: () => void this.toggleTerminal() },
      { label: "Connect to Server…", action: () => void this.connectToServer() },
      { label: "Disk Utility…", action: () => void this.diskDialog.open() },
      { label: "Burn Studio…", action: () => this.burnStudio.open() },
      { label: "Mount Cloud Drive…", action: () => void this.cloudMount.open() },
      { label: "App Deleter…", action: () => void this.appDeleter.open(() => this.refreshActiveDir()) },
      {
        label: "Synchronize Folders…",
        action: () => void this.openSyncHere(),
      },
      {
        label: "Duplicate Finder…",
        action: () => void this.openDupHere(),
      },
      {
        label: "Manage Labels…",
        action: () =>
          this.labelManager.open(() => {
            void this.buildSidebar();
            this.refreshActiveDir();
          }),
      },
      { label: "Theme…", action: () => this.themeManager.openMenu(this.menuBtn) },
      { label: "About Fable Files", action: () => this.aboutDialog.open() },
      {
        label: "Report a Bug…",
        action: () =>
          void api.openUrl(
            "https://github.com/garrisonr/fable-files/issues/new?title=" +
              encodeURIComponent("[Bug] ") +
              "&labels=bug"
          ),
      },
      {
        label: "Request a Feature…",
        action: () =>
          void api.openUrl(
            "https://github.com/garrisonr/fable-files/issues/new?title=" +
              encodeURIComponent("[Feature] ") +
              "&labels=enhancement"
          ),
      },
    ]);
  }

  /** Undo the last trash / drag-move (Ctrl+Z or Super+Z). */
  private async undoLast(): Promise<void> {
    const entry = this.undoStack.pop();
    if (!entry) {
      Toast.error("Nothing to undo.");
      return;
    }
    try {
      if (entry.kind === "trash") {
        const restored = await api.untrashItem(entry.trashName);
        Toast.success(`Restored “${entry.label}”.`);
        void restored;
      } else {
        await api.movePathTo(entry.newPath, entry.fromDir);
        Toast.success(`Moved “${entry.label}” back.`);
      }
      this.quickLook.close();
      const tab = this.activeTab;
      if (tab) await this.loadDir(tab);
    } catch (err) {
      Toast.error(`Undo failed: ${err}`);
    }
  }

  private renderNavButtons(): void {
    const tab = this.activeTab;
    this.backBtn.disabled = !(tab && canGoBack(tab));
    this.fwdBtn.disabled = !(tab && canGoForward(tab));
    this.upBtn.disabled = !(tab && currentDir(tab).crumbs.length > 1);
  }

  // ----------------------------------------------------------------- search

  private showFileView(): void {
    this.searchView.el.hidden = true;
    this.renderActiveView();
  }

  // ------------------------------------------------------------- theme menu

  private renderThemeMenu(themes: MenuThemeItem[], activeId: string, anchor: HTMLElement): void {
    this.closeMenu();
    const menu = document.createElement("div");
    menu.className = "theme-menu";
    menu.setAttribute("role", "menu");
    // Anchor to the trigger: transform-origin at the source element.
    const r = anchor.getBoundingClientRect();
    menu.style.transformOrigin = "top right";
    for (const t of themes) {
      if (t.separator) {
        const sep = document.createElement("div");
        sep.className = "theme-sep";
        menu.appendChild(sep);
        continue;
      }
      const item = document.createElement("button");
      item.className = "theme-item";
      item.setAttribute("role", "menuitemradio");
      item.setAttribute("aria-checked", String(t.id === activeId));
      if (t.preview) {
        const sw = document.createElement("span");
        sw.className = "theme-swatch";
        for (const c of t.preview) {
          const dot = document.createElement("i");
          dot.style.background = c;
          sw.appendChild(dot);
        }
        item.appendChild(sw);
      }
      const name = document.createElement("span");
      name.textContent = t.name;
      const meta = document.createElement("span");
      meta.className = "theme-meta";
      meta.textContent = t.metaLabel ?? (t.dark ? "Dark" : "Light");
      item.append(name, meta);
      if (t.id === activeId) item.classList.add("active");
      item.addEventListener("click", () => {
        void this.themeManager.apply(t.id).then(() => this.closeMenu());
      });
      menu.appendChild(item);
    }
    this.menuLayer.hidden = false;
    this.menuLayer.appendChild(menu);
    // Position under the trigger, inside the viewport.
    const layer = this.menuLayer.getBoundingClientRect();
    menu.style.top = `${r.bottom - layer.top + 6}px`;
    menu.style.right = `${Math.max(8, layer.right - r.right)}px`;
    if (!reducedMotion()) {
      const from = { opacity: liveOpacity(menu), transform: "scale(0.96) translateY(-4px)" };
      animateFromLive(menu, [from, { opacity: 1, transform: "scale(1) translateY(0)" }], {
        duration: 140,
        easing: "ease-out",
        fill: "forwards",
      });
    }
    menu.querySelector("button")?.focus();
  }

  private closeMenu(): void {
    this.menuLayer.hidden = true;
    this.menuLayer.querySelectorAll(".theme-menu").forEach((m) => m.remove());
  }

  // ---------------------------------------------------------------- status

  private renderStatus(): void {
    const tab = this.activeTab;
    if (!tab) {
      this.statusLeft.textContent = "No tabs open";
      return;
    }
    const n = tab.items.length;
    const sel = tab.selection.size;
    this.statusLeft.textContent =
      `${this.displayPath()} · ${n} item${n === 1 ? "" : "s"}` +
      (sel > 0 ? ` · ${sel} selected` : "");
  }

  private flashStatus(text: string): void {
    this.statusLeft.textContent = text;
    window.setTimeout(() => {
      if (this.statusLeft.textContent === text) this.renderStatus();
    }, 2600);
  }

  private renderEmptyState(): void {
    this.fileView.el.innerHTML = "";
    const p = document.createElement("p");
    p.className = "empty-state";
    p.textContent = "No locations yet. Add one to start browsing.";
    this.fileView.el.appendChild(p);
    this.statusLeft.textContent = "No locations";
    this.renderTabBar();
    this.renderCrumbs();
    this.renderNavButtons();
  }

  // --------------------------------------------------------------- keyboard

  private onGlobalKey(e: KeyboardEvent): void {
    const inField =
      e.target instanceof HTMLInputElement ||
      e.target instanceof HTMLTextAreaElement;
    if ((e.ctrlKey || e.metaKey) && e.key.toLocaleLowerCase() === "k") {
      e.preventDefault();
      if (this.palette.isOpen) this.palette.close();
      else void this.openPalette();
      return;
    }
    if ((e.ctrlKey || e.metaKey) && e.key.toLocaleLowerCase() === "t") {
      e.preventDefault();
      const loc = this.activeTab
        ? this.locations.find((l) => l.id === currentDir(this.activeTab!).locationId)
        : this.locations[0];
      if (loc) this.newTab(loc);
      return;
    }
    if ((e.ctrlKey || e.metaKey) && e.key.toLocaleLowerCase() === "w") {
      e.preventDefault();
      if (this.activeTab) this.closeTab(this.activeTab);
      return;
    }
    if ((e.ctrlKey || e.metaKey) && e.key.toLocaleLowerCase() === "e") {
      // Ctrl+E: make the selected file executable (chmod +x).
      e.preventDefault();
      const sel = this.activeSelected();
      if (sel) {
        api.setExecutable(sel.id, true).catch((err) => console.error("chmod +x failed:", err));
      }
      return;
    }
    if ((e.ctrlKey || e.metaKey) && (e.key === "=" || e.key === "+")) {
      // Ctrl+=: increase font size
      e.preventDefault();
      this.setFontScale(this.fontScale + 0.1);
      return;
    }
    if ((e.ctrlKey || e.metaKey) && e.key === "-") {
      // Ctrl+-: decrease font size
      e.preventDefault();
      this.setFontScale(this.fontScale - 0.1);
      return;
    }
    if ((e.ctrlKey || e.metaKey) && e.key === "0") {
      // Ctrl+0: reset font size
      e.preventDefault();
      this.setFontScale(1.0);
      return;
    }
    if ((e.ctrlKey || e.metaKey) && e.key.toLocaleLowerCase() === "z" && !e.shiftKey) {
      // Undo the last trash / move. In text fields, leave text undo alone.
      if (!inField) {
        e.preventDefault();
        void this.undoLast();
      }
      return;
    }
    if ((e.ctrlKey || e.metaKey) && !inField) {
      const k = e.key.toLocaleLowerCase();
      if (k === "x" || k === "c") {
        const items = this.activeSelectionItems();
        if (items.length > 0) {
          e.preventDefault();
          void this.clipboardSet(items, k === "x");
          return;
        }
      } else if (k === "v") {
        e.preventDefault();
        void this.pasteIntoCurrentDir();
        return;
      } else if (k === "a") {
        e.preventDefault();
        this.fileView.selectAll();
        return;
      } else if (k === "`") {
        e.preventDefault();
        void this.toggleTerminal();
        return;
      }
    }
    if (e.key === "F2" && !inField) {
      const sel = this.activeSelected();
      if (sel) {
        e.preventDefault();
        this.renameDialog.open(sel, () => this.refreshActiveDir());
        return;
      }
    }
    if (inField) return;
    if (e.key === "Delete" || e.key === "Backspace") {
      // Del AND Backspace move the selection to the Trash. (Dickles'
      // law, 2026-10-13: Backspace deletes — it NEVER navigates back.)
      if (!this.searchView.field.contains(document.activeElement)) {
        const items = this.activeSelectionItems();
        if (items.length > 0) {
          e.preventDefault();
          void this.trashItems(items);
        }
      }
      return;
    }
    if (e.key === " " && this.activeTab && !this.searchView.active) {
      // Space toggles Quick Look — never when typing.
      const tag = (e.target as HTMLElement).tagName;
      if (tag !== "BUTTON" && tag !== "INPUT") {
        e.preventDefault();
        this.togglePreview();
      }
    } else if (e.key === "Escape") {
      if (!this.menuLayer.hidden) this.closeMenu();
      else if (this.quickLook.isOpen) this.quickLook.close();
    } else if (e.altKey && e.key === "ArrowUp") {
      if (this.activeTab) {
        e.preventDefault();
        this.goUp();
      }
    }
  }
}

// Re-export for main.ts convenience.
export { DEFAULT_THEME };
