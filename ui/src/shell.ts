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
import { FileView } from "./fileview";
import { IndexTracker } from "./indexing";
import { reducedMotion, animateFromLive, liveOpacity } from "./motion";
import { QuickLook } from "./quicklook";
import { ContextMenu } from "./contextmenu";
import { Toast, confirmDialog } from "./toast";
import { SearchView } from "./search";
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
import { DEFAULT_THEME, ThemeManager } from "./themes";

/** Where a sidebar favorite navigates: a real location root or a
 * same-named folder inside the Home location. */
interface FavoriteTarget {
  locationId: string;
  dirId: string;
  display: string;
}

export class Shell {
  readonly el = document.createElement("div");
  private tabs: Tab[] = [];
  private activeTab: Tab | null = null;
  private locations: LocationSummary[] = [];

  private crumbBar!: HTMLElement;
  private backBtn!: HTMLButtonElement;
  private fwdBtn!: HTMLButtonElement;
  private upBtn!: HTMLButtonElement;
  private viewGridBtn!: HTMLButtonElement;
  private viewListBtn!: HTMLButtonElement;
  private tabBar!: HTMLElement;
  private sidebarList!: HTMLElement;
  private favList!: HTMLElement;
  private statusLeft!: HTMLElement;
  private statusRight!: HTMLElement;
  private viewport!: HTMLElement;
  private menuLayer!: HTMLElement;
  private searchWrap!: HTMLElement;

  private fileView: FileView;
  private quickLook: QuickLook;
  private searchView: SearchView;
  private themeManager: ThemeManager;
  private indexTracker: IndexTracker;
  private ctxMenu = new ContextMenu();
  private fontScale = 1.0;

  constructor() {
    this.el.id = "app";
    this.buildChrome();

    this.fileView = new FileView(
      () => this.requireTab(),
      {
        onOpen: (item) => this.openItem(item),
        onSelect: (item) => this.onSelect(item),
        onTogglePreview: () => this.togglePreview(),
        onMakeExecutable: (item) => {
          api.setExecutable(item.id, true).catch((err) =>
            console.error("chmod +x failed:", err)
          );
        },
      },
    );
    this.quickLook = new QuickLook(() => this.fileView.el.focus({ preventScroll: true }));
    this.searchView = new SearchView({
      onNavigateToItem: (item) => {
        this.searchView.clear(); // back to the file view before navigating
        this.openItem(item);
      },
      onPreviewItem: (item) => void this.quickLook.open(item),
      onResults: () => {
        this.fileView.el.hidden = true;
      },
      onClear: () => this.showFileView(),
    });
    this.viewport.append(this.fileView.el, this.searchView.el, this.quickLook.el);
    // The search field lives in the toolbar; its results panel is in the viewport.
    this.searchWrap.appendChild(this.searchView.field);

    this.themeManager = new ThemeManager({
      renderMenu: (themes, activeId, anchor) => this.renderThemeMenu(themes, activeId, anchor),
      closeMenu: () => this.closeMenu(),
    });
    this.indexTracker = new IndexTracker((text) => {
      this.statusRight.textContent = text ?? "";
      this.statusRight.hidden = !text;
    });

    document.addEventListener("keydown", (e) => this.onGlobalKey(e));
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

    const viewToggle = document.createElement("div");
    viewToggle.className = "segmented";
    viewToggle.setAttribute("role", "group");
    viewToggle.setAttribute("aria-label", "View mode");
    this.viewGridBtn = this.toggleButton("Grid view", "grid");
    this.viewListBtn = this.toggleButton("List view", "list");
    viewToggle.append(this.viewGridBtn, this.viewListBtn);

    const searchWrap = document.createElement("div");
    searchWrap.className = "search-wrap";
    const searchIcon = document.createElement("span");
    searchIcon.className = "search-icon";
    searchIcon.textContent = "🔍";
    searchWrap.appendChild(searchIcon);
    // The search field is mounted here in the constructor, once the real
    // SearchView exists (its results panel lives in the viewport).
    this.searchWrap = searchWrap;

    const themeBtn = document.createElement("button");
    themeBtn.className = "icon-btn";
    themeBtn.textContent = "Theme";
    themeBtn.setAttribute("aria-label", "Choose theme");
    themeBtn.setAttribute("aria-haspopup", "menu");
    themeBtn.addEventListener("click", () => this.themeManager.openMenu(themeBtn));

    toolbar.append(nav, this.crumbBar, this.searchWrap, viewToggle, themeBtn);
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
    sidebar.append(favHead, this.favList, locHead, this.sidebarList);

    const main = document.createElement("main");
    main.className = "main";
    this.tabBar = document.createElement("div");
    this.tabBar.className = "tabbar";
    this.tabBar.setAttribute("role", "tablist");
    this.viewport = document.createElement("div");
    this.viewport.className = "viewport";
    main.append(this.tabBar, this.viewport);

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

  private toggleButton(label: string, mode: ViewMode): HTMLButtonElement {
    const b = document.createElement("button");
    b.className = "seg-btn";
    b.textContent = label;
    b.setAttribute("aria-label", label);
    b.setAttribute("aria-pressed", "false");
    b.addEventListener("click", () => this.setView(mode));
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
    this.renderViewToggle();
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
      tab.items = await api.listDir(dir.locationId, dir.dirId);
    } catch {
      tab.items = [];
    }
    if (tab === this.activeTab) {
      this.fileView.render();
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
    if (item.kind === "folder") {
      this.navigateTo(descend(tab, item));
    } else {
      // Double-click / Enter opens with the OS default app; Spacebar is Quick Look.
      void api
        .openItem(item.id)
        .catch(() => void this.quickLook.open(item));
    }
  }

  private onSelect(item: Item | null): void {
    void item;
    this.renderStatus();
    this.renderCrumbs();
  }

  private togglePreview(): void {
    const sel = this.fileView.selected;
    this.quickLook.toggle(sel);
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
    // Copy path button (one-click copy)
    const copyBtn = document.createElement("button");
    copyBtn.className = "icon-btn crumb-copy";
    copyBtn.textContent = "\u29C9";  // copy square icon
    copyBtn.setAttribute("aria-label", "Copy current path");
    copyBtn.title = "Copy path";
    copyBtn.addEventListener("click", async (e) => {
      e.stopPropagation();
      try {
        const tab = this.activeTab;
        if (!tab) return;
        const dir = currentDir(tab);
        // Get real filesystem path from backend
        let path = await api.getItemPath(dir.dirId);
        // Append selected filename if one is selected
        const sel = this.fileView?.selected;
        if (sel) path = path + "/" + sel.name;
        await navigator.clipboard.writeText(path);
        copyBtn.textContent = "\u2713";
        setTimeout(() => { copyBtn.textContent = "\u29C9"; }, 1000);
        Toast.success("Path copied");
      } catch (err) {
        Toast.error(`Copy failed: ${err}`);
      }
    });
    this.crumbBar.appendChild(copyBtn);
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
    const sel = this.fileView?.selected;
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
    for (let i = 0; i < names.length; i++) {
      const name = names[i];
      const target = targets[i];
      const b = document.createElement("button");
      b.className = "side-item";
      b.textContent = name;
      b.disabled = !target;
      b.title = target ? target.display : `${name} is not available`;
      if (target) {
        b.addEventListener("click", () => {
          this.navigateTo({
            locationId: target.locationId,
            dirId: target.dirId,
            crumbs: [{ dirId: target.dirId, name: target.display }],
          });
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
                  const ok = await confirmDialog(
                    "Empty Trash?",
                    "This will permanently delete all items in the trash. This cannot be undone.",
                    "Empty Trash",
                    true
                  );
                  if (ok) {
                    try {
                      const r = await api.emptyTrash();
                      Toast.success(`Trash emptied — ${r.deleted} item${r.deleted === 1 ? "" : "s"} deleted${r.errors > 0 ? ` (${r.errors} errors)` : ""}.`);
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
                  const ok = await confirmDialog(
                    "Secure Delete?",
                    "Files will be overwritten with zeros before deletion. This cannot be undone.",
                    "Secure Delete",
                    true
                  );
                  if (ok) {
                    try {
                      const r = await api.secureEmptyTrash();
                      Toast.success(`Securely deleted ${r.deleted} items${r.errors > 0 ? ` (${r.errors} errors)` : ""}.`);
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
      this.sidebarList.appendChild(b);
    }
    if (this.locations.length === 0) {
      const p = document.createElement("p");
      p.className = "side-empty";
      p.textContent = "No locations yet.";
      this.sidebarList.appendChild(p);
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
  }

  // -------------------------------------------------------------- view mode

  private setView(mode: ViewMode): void {
    const tab = this.requireTab();
    this.fileView.switchView(mode);
    this.renderViewToggle();
    void tab;
  }

  private renderViewToggle(): void {
    const tab = this.activeTab;
    const mode = tab?.view ?? "grid";
    this.viewGridBtn.setAttribute("aria-pressed", String(mode === "grid"));
    this.viewListBtn.setAttribute("aria-pressed", String(mode === "list"));
    this.viewGridBtn.classList.toggle("active", mode === "grid");
    this.viewListBtn.classList.toggle("active", mode === "list");
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
    this.fileView.el.hidden = false;
  }

  // ------------------------------------------------------------- theme menu

  private renderThemeMenu(themes: ThemeInfo[], activeId: string, anchor: HTMLElement): void {
    this.closeMenu();
    const menu = document.createElement("div");
    menu.className = "theme-menu";
    menu.setAttribute("role", "menu");
    // Anchor to the trigger: transform-origin at the source element.
    const r = anchor.getBoundingClientRect();
    menu.style.transformOrigin = "top right";
    for (const t of themes) {
      const item = document.createElement("button");
      item.className = "theme-item";
      item.setAttribute("role", "menuitemradio");
      item.setAttribute("aria-checked", String(t.id === activeId));
      const name = document.createElement("span");
      name.textContent = t.name;
      const meta = document.createElement("span");
      meta.className = "theme-meta";
      meta.textContent = t.dark ? "Dark" : "Light";
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
      const sel = this.fileView.selected;
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
    if (inField) return;
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
    } else if (e.key === "Backspace" || (e.altKey && e.key === "ArrowUp")) {
      if (this.activeTab) {
        e.preventDefault();
        this.goUp();
      }
    }
  }
}

// Re-export for main.ts convenience.
export { DEFAULT_THEME };
