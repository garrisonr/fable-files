/**
 * ROM Home (Dickles, 2026-10-15 — SPEC.md in ~/workspace/fable-romhome):
 * the ROM collection's own room. Shelves, not a spreadsheet: a rail of
 * library shelves + systems by generation, an art-first shelf, and an
 * inspector that springs in on selection. The room adds no engines —
 * it stages Fable's existing powers (DAT identity, Get missing media,
 * ROM Lists, stars/tags, Library Memory) on the catalog alone, so
 * offline drives browse fully and honestly.
 *
 * HARD CONSTRAINT (SPEC §3): no hardcoded tile backgrounds anywhere —
 * icons and art sit directly on the theme's own surfaces; separation
 * comes only from the theme's elevation shadow + border hairline.
 */
import {
  api,
  fileSrc,
  formatBytes,
  type CollectionInfo,
  type Item,
  type RomGameState,
  type RomHomeGame,
  type RomSmartList,
} from "./api";
import {
  loadRomSystems,
  romSystemKey,
  romScopeOf,
  isRomPlaceholder,
  romDivisions,
  romSystemMeta,
  romIconSlug,
  loadRomIconSet,
  romArchiveLink,
  OWNED_HARDWARE,
  type RomSystem,
  type OwnedHardware,
  type RomGrouping,
} from "./roms";
import { ROM_FLAT_ICONS } from "./romicons_flat";
import { ContextMenu, type MenuItem } from "./contextmenu";
import { Toast, promptDialog, errText } from "./toast";
import { openGuideLink } from "./guidelinks";
import { fmtDay } from "./search";
import { springTo, reducedMotion } from "./motion";
import { loadLabelDefs } from "./labels";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface RomHomeHooks {
  /** Leave the room; the shell restores the Files view as it was. */
  onExit(): void;
  /** Play: the shell's existing launch path (openItem). */
  onPlayItem(item: Item): void;
  /** Space: Quick Look peek. */
  onPreviewItem(item: Item): void;
  /** "Show in Files": navigate the Files view to the item. */
  onRevealItem(item: Item): void;
  /** "Play with…" — the app's Open With dialog. */
  onOpenWith(item: Item): void;
  /** "See in File Auditor". */
  onOpenAuditor(): void;
  /** "Add a ROM folder…" — the shell's add-location flow. */
  onAddRomFolder(): void;
  /** Right-click on a rail system: the shell's Manage/Delete menu
   * (the retired tree's doors, preserved in the room). */
  onManageSystem(sys: RomSystem, x: number, y: number): void;
  /** "Also search all files…" — the room searches ROMs; Fable's
   * global search stays one honest hop away (§8). */
  onSearchAllFiles(query: string): void;
}

type ShelfRef =
  | { kind: "all" }
  | { kind: "favorites" }
  | { kind: "recent" }
  | { kind: "missing" }
  | { kind: "system"; name: string }
  | { kind: "hardware"; name: string }
  | { kind: "list"; id: string }
  | { kind: "smart"; id: string }
  | { kind: "search"; query: string };

type SortKey = "title" | "year" | "recent-played" | "most-played" | "recent-added";
type ViewMode = "grid" | "list";

interface RoomFilters {
  systems: string[];
  genres: string[];
  players: string[];
  media: string[];
  flags: string[];
}

const EMPTY_FILTERS: RoomFilters = {
  systems: [],
  genres: [],
  players: [],
  media: [],
  flags: [],
};

export interface RoomGame {
  key: string;
  item: Item;
  /** Display title: the user's override, else the DAT title, else
   * the file's clean name. */
  title: string;
  datTitle: string;
  /** Dataset key ("nes"), or null for extension-rule games. */
  platform: string | null;
  alsoPlatforms: string[];
  systemName: string;
  year: number | null;
  region: string | null;
  sha1: string | null;
  /** media type -> local path when the cascade landed a file. */
  media: Map<string, string>;
  state: RomGameState;
  starred: boolean;
  labels: string[];
  auditorFlagged: boolean;
  offline: boolean;
  volumeLabel: string | null;
  volumeLastSeen: string | null;
  source: "dat" | "ext";
  /** Personal state read from the catalog (DAT games arrive with
   * it; extension games hydrate once on join). */
  stateHydrated: boolean;
}

interface RoomSystem {
  sys: RomSystem;
  name: string;
  key: string | null;
  placeholder: boolean;
  count: number | null; // null = not counted yet (extension systems)
  missing: number;
  sizeBytes: number;
}

const STATE_KEY = "ff-romhome-state-v1";
const MEDIA_TYPES = ["boxart", "screenshot", "titleshot", "trailer"] as const;
const MEDIA_LABEL: Record<string, string> = {
  boxart: "Box art",
  screenshot: "Screenshot",
  titleshot: "Title shot",
  trailer: "Video",
};
const SORT_LABEL: Record<SortKey, string> = {
  title: "Title",
  year: "Year",
  "recent-played": "Recently played",
  "most-played": "Most played",
  "recent-added": "Recently added",
};

function ordinal(n: number): string {
  return n === 1 ? "1st" : n === 2 ? "2nd" : n === 3 ? "3rd" : `${n}th`;
}

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  e.className = className;
  if (text !== undefined) e.textContent = text;
  return e;
}

/** A stable cheerful hue per system for the typographic placeholder
 * tint (mixed at low alpha over the theme surface — never a tile). */
function systemHue(seed: string): number {
  let h = 0;
  for (let i = 0; i < seed.length; i++) h = (h * 31 + seed.charCodeAt(i)) % 360;
  return h;
}

/** Players bucket from a user-supplied players string ("2", "1-4",
 * "2+"): the bucket names the most players it seats. */
function playersBucket(players: string | null): string | null {
  if (!players) return null;
  const nums = players.match(/\d+/g)?.map((n) => Number(n)) ?? [];
  if (nums.length === 0) return null;
  const max = Math.max(...nums);
  if (max <= 1) return "1";
  if (max === 2) return "2";
  if (max <= 4) return "3-4";
  return "5+";
}

/** The plain-sentence reading of a filter set (Smart List rules and
 * the filter popover speak the same words). */
export function filterSentence(f: RoomFilters): string {
  const parts: string[] = [];
  if (f.systems.length) {
    const names = f.systems.map((k) => systemNameForKey(k));
    parts.push(`System is ${names.join(" or ")}`);
  }
  if (f.genres.length) parts.push(`Genre is ${f.genres.join(" or ")}`);
  for (const p of f.players) {
    parts.push(
      p === "1"
        ? "1 player"
        : p === "2"
          ? "2 players"
          : p === "3-4"
            ? "3–4 players"
            : "5+ players",
    );
  }
  for (const m of f.media) {
    parts.push(
      m === "has-art"
        ? "has box art"
        : m === "missing-art"
          ? "missing box art"
          : m === "has-video"
            ? "has video"
            : m,
    );
  }
  for (const fl of f.flags) {
    parts.push(
      fl === "starred"
        ? "Starred"
        : fl === "favorites"
          ? "Favorites"
          : fl === "completed"
            ? "Completed"
            : fl === "unplayed"
              ? "Not yet played"
              : fl,
    );
  }
  return parts.join(" · ");
}

const KEY_TO_NAME = new Map<string, string>();
function systemNameForKey(key: string): string {
  if (KEY_TO_NAME.size === 0) {
    for (const s of loadRomSystems()) {
      const k = romSystemKey(s);
      if (k) KEY_TO_NAME.set(k, s.name);
    }
  }
  return KEY_TO_NAME.get(key) ?? key;
}

export class RomHome {
  readonly el = document.createElement("div");
  active = false;
  private hooks: RomHomeHooks;
  private ctxMenu = new ContextMenu();

  // Data
  private games: RoomGame[] = [];
  private gamesByKey = new Map<string, RoomGame>();
  private systems: RoomSystem[] = [];
  private counts = { total: 0, missingMedia: 0 };
  private collections: CollectionInfo[] = [];
  private smartLists: RomSmartList[] = [];
  private trailersEnabled = false;
  private loaded = false;
  private extLoaded = new Set<string>();
  private loadSeq = 0;

  // Room state (persisted)
  private shelf: ShelfRef = { kind: "all" };
  private grouping: RomGrouping = "generation";
  private sort: SortKey = "title";
  private view: ViewMode = "grid";
  private artW = 150;
  private filters: RoomFilters = structuredClone(EMPTY_FILTERS);
  private tv = false;
  private selection = new Set<string>();
  /** The Smart List whose rule is being edited in place (§4). */
  private editingSmartId: string | null = null;
  private anchorKey: string | null = null;
  private scrollByShelf = new Map<string, number>();

  // DOM
  private railEl!: HTMLElement;
  private shelfEl!: HTMLElement;
  private shelfScroll!: HTMLElement;
  private inspectorEl!: HTMLElement;
  private searchInput!: HTMLInputElement;
  private chipsBar!: HTMLElement;
  private healthEl!: HTMLElement;
  private unlisten: UnlistenFn | null = null;
  private gamepadTimer = 0;

  constructor(hooks: RomHomeHooks) {
    this.hooks = hooks;
    this.el.className = "romhome";
    this.el.hidden = true;
    this.restoreState();
    this.buildChrome();
  }

  // ------------------------------------------------------------ chrome

  private buildChrome(): void {
    const toolbar = el("div", "rh-toolbar");
    const back = el("button", "icon-btn rh-back", "‹") as HTMLButtonElement;
    back.title = "Back to Files";
    back.setAttribute("aria-label", "Back to Files");
    back.addEventListener("click", () => this.hooks.onExit());
    const title = el("span", "rh-title", "ROMs");
    const searchWrap = el("div", "rh-search");
    this.searchInput = el("input", "rh-search-in") as HTMLInputElement;
    this.searchInput.type = "search";
    this.searchInput.placeholder = "Search all ROMs";
    this.searchInput.setAttribute("aria-label", "Search all ROMs");
    let searchTimer = 0;
    this.searchInput.addEventListener("input", () => {
      window.clearTimeout(searchTimer);
      searchTimer = window.setTimeout(() => {
        const q = this.searchInput.value.trim();
        if (q) {
          this.setShelf({ kind: "search", query: q });
        } else if (this.shelf.kind === "search") {
          this.setShelf({ kind: "all" });
        }
      }, 180);
    });
    this.searchInput.addEventListener("keydown", (e) => {
      if (e.key === "Escape") {
        this.searchInput.value = "";
        if (this.shelf.kind === "search") this.setShelf({ kind: "all" });
        this.searchInput.blur();
      }
      e.stopPropagation();
    });
    searchWrap.appendChild(this.searchInput);

    const filtersBtn = el("button", "pill-btn rh-filters-btn", "Filters ▾");
    filtersBtn.addEventListener("click", () => this.openFiltersMenu(filtersBtn));
    const sortBtn = el("button", "pill-btn rh-sort-btn", "Sort ▾");
    sortBtn.addEventListener("click", () => this.openSortMenu(sortBtn));
    const viewBtn = el("button", "pill-btn rh-view-btn") as HTMLButtonElement;
    viewBtn.textContent = this.view === "grid" ? "List" : "Grid";
    viewBtn.title = "Toggle grid / list";
    viewBtn.addEventListener("click", () => {
      this.view = this.view === "grid" ? "list" : "grid";
      viewBtn.textContent = this.view === "grid" ? "List" : "Grid";
      this.saveState();
      this.renderShelf();
    });
    const sizeWrap = el("label", "rh-size");
    sizeWrap.title = "Art size";
    const size = el("input", "rh-size-in") as HTMLInputElement;
    size.type = "range";
    size.min = "90";
    size.max = "280";
    size.value = String(this.artW);
    size.setAttribute("aria-label", "Art size");
    size.addEventListener("input", () => {
      this.artW = Number(size.value);
      this.applyArtSize();
      this.saveState();
    });
    sizeWrap.append(el("span", "rh-size-lab", "Art"), size);
    const moreBtn = el("button", "icon-btn rh-more", "⋯") as HTMLButtonElement;
    moreBtn.title = "More";
    moreBtn.addEventListener("click", () => this.openMoreMenu(moreBtn));
    toolbar.append(back, title, searchWrap, filtersBtn, sortBtn, viewBtn, sizeWrap, moreBtn);
    this.el.appendChild(toolbar);

    this.chipsBar = el("div", "rh-chipsbar");
    this.chipsBar.hidden = true;
    this.el.appendChild(this.chipsBar);

    const body = el("div", "rh-body");
    this.railEl = el("aside", "rh-rail");
    this.railEl.setAttribute("aria-label", "Library");
    this.shelfEl = el("section", "rh-shelf");
    this.inspectorEl = el("aside", "rh-inspector");
    this.inspectorEl.hidden = true;
    body.append(this.railEl, this.shelfEl, this.inspectorEl);
    this.el.appendChild(body);
    this.applyArtSize();
    if (this.tv) this.el.classList.add("rh-tv");
    this.el.addEventListener("keydown", (e) => this.onRoomKey(e));
  }

  private applyArtSize(): void {
    const w = this.tv ? Math.round(this.artW * 2.2) : this.artW;
    this.el.style.setProperty("--rh-art-w", `${w}px`);
  }

  // ------------------------------------------------------------ state

  private restoreState(): void {
    try {
      const raw = localStorage.getItem(STATE_KEY);
      if (!raw) return;
      const s = JSON.parse(raw) as Record<string, unknown>;
      if (s.shelf && typeof s.shelf === "object") this.shelf = s.shelf as ShelfRef;
      if (s.grouping === "maker" || s.grouping === "generation") this.grouping = s.grouping;
      if (typeof s.sort === "string" && s.sort in SORT_LABEL) this.sort = s.sort as SortKey;
      if (s.view === "grid" || s.view === "list") this.view = s.view;
      if (typeof s.artW === "number") this.artW = Math.min(280, Math.max(90, s.artW));
      if (s.filters && typeof s.filters === "object") {
        this.filters = { ...structuredClone(EMPTY_FILTERS), ...(s.filters as RoomFilters) };
      }
      if (s.tv === true) this.tv = true;
    } catch {
      // fresh room
    }
  }

  private saveState(): void {
    try {
      localStorage.setItem(
        STATE_KEY,
        JSON.stringify({
          shelf: this.shelf,
          grouping: this.grouping,
          sort: this.sort,
          view: this.view,
          artW: this.artW,
          filters: this.filters,
          tv: this.tv,
        }),
      );
    } catch {
      // session-only is fine
    }
  }

  private shelfKey(s: ShelfRef = this.shelf): string {
    switch (s.kind) {
      case "system":
        return `system|${s.name}`;
      case "hardware":
        return `hardware|${s.name}`;
      case "list":
        return `list|${s.id}`;
      case "smart":
        return `smart|${s.id}`;
      case "search":
        return `search|${s.query}`;
      default:
        return s.kind;
    }
  }

  // ------------------------------------------------------------ data

  /** Enter the room: paint from the catalog, then reconcile in the
   * background (identification is cached; extension shelves stream). */
  async open(target?: ShelfRef): Promise<void> {
    this.active = true;
    this.el.hidden = false;
    if (target) this.shelf = target;
    if (!this.unlisten) {
      this.unlisten = await listen<{ done?: boolean }>("job-progress", (e) => {
        if (this.active && e.payload?.done) void this.softRefresh();
      }).catch(() => null);
    }
    await this.loadAll();
    if (this.shelf.kind === "search") this.searchInput.value = this.shelf.query;
    if (this.tv) this.startGamepad();
    this.renderRail();
    this.renderShelf();
    this.restoreScroll();
    // Background reconciliation: identification catches new files;
    // when it lands and the tallies moved, reload quietly.
    const before = this.counts.total;
    void api
      .identifyRoms()
      .then(async () => {
        if (!this.active) return;
        const c = await api.romHomeCounts().catch(() => null);
        if (c && c.total !== before) await this.softRefresh();
      })
      .catch(() => undefined);
    void this.loadExtSystems();
  }

  close(): void {
    this.saveScroll();
    this.saveState();
    this.active = false;
    this.el.hidden = true;
    this.stopGamepad();
  }

  /** Data changed elsewhere (re-index, job): reload on next paint. */
  invalidate(): void {
    this.loaded = false;
    if (this.active) void this.softRefresh();
  }

  private async softRefresh(): Promise<void> {
    const keep = new Set(this.selection);
    await this.loadAll();
    this.selection = new Set([...keep].filter((k) => this.gamesByKey.has(k)));
    if (this.active) {
      this.renderRail();
      this.renderShelf(true);
      this.renderInspector();
    }
  }

  private async loadAll(): Promise<void> {
    const seq = ++this.loadSeq;
    const [counts, datGames, collections, smartLists, settings] = await Promise.all([
      api.romHomeCounts().catch(() => null),
      api.romHomeGames(null).catch(() => []),
      api.listCollections().catch(() => []),
      api.romHomeSmartLists().catch(() => []),
      api.getRomMediaSettings().catch(() => null),
    ]);
    if (seq !== this.loadSeq) return;
    if (counts) {
      this.counts = { total: counts.total, missingMedia: counts.missingMedia };
    }
    this.collections = collections;
    this.smartLists = smartLists;
    this.trailersEnabled = settings?.settings.trailers.enabled ?? false;
    // Systems: UI definitions + catalog tallies by dataset key.
    const perSystem = new Map<string, [number, number]>();
    if (counts) for (const [p, g, m] of counts.perSystem) perSystem.set(p, [g, m]);
    this.systems = loadRomSystems().map((sys) => {
      const key = romSystemKey(sys);
      const tally = key ? perSystem.get(key) : undefined;
      return {
        sys,
        name: sys.name,
        key,
        placeholder: isRomPlaceholder(sys),
        count: key ? (tally?.[0] ?? 0) : null,
        missing: key ? (tally?.[1] ?? 0) : 0,
        sizeBytes: 0,
      };
    });
    const games: RoomGame[] = datGames.map((g) => this.toRoomGame(g));
    // Extension-system games already loaded this session stay merged.
    for (const g of this.games) {
      if (g.source === "ext") games.push(g);
    }
    this.games = games;
    this.rebuildIndex();
    // Per-system byte totals from the games themselves.
    for (const s of this.systems) {
      s.sizeBytes = this.games
        .filter((g) => g.systemName === s.name)
        .reduce((a, g) => a + (g.item.size_bytes ?? 0), 0);
    }
    this.loaded = true;
  }

  private rebuildIndex(): void {
    this.gamesByKey = new Map(this.games.map((g) => [g.key, g]));
  }

  private toRoomGame(g: RomHomeGame): RoomGame {
    const media = new Map<string, string>();
    for (const m of g.media) {
      if (m.found && m.localPath) media.set(m.mediaType, m.localPath);
    }
    const systemName = systemNameForKey(g.platform);
    return {
      key: g.item.provider_key ?? g.item.id,
      item: g.item,
      title: g.state.titleOverride ?? g.title,
      datTitle: g.title,
      platform: g.platform,
      alsoPlatforms: g.alsoPlatforms,
      systemName,
      year: g.year,
      region: g.region,
      sha1: g.sha1,
      media,
      state: g.state,
      starred: g.item.starred,
      labels: g.item.labels,
      auditorFlagged: g.auditorFlagged,
      offline: g.volumeOffline,
      volumeLabel: g.volumeLabel,
      volumeLastSeen: g.volumeLastSeen,
      source: "dat",
      stateHydrated: true,
    };
  }

  /** Extension-rule systems gather through the rule store (the smart
   * folder machinery): their games join All Games + their shelf with
   * file-name titles and no media identity. */
  private async loadExtSystems(): Promise<void> {
    for (const s of this.systems) {
      if (s.key !== null || s.placeholder || this.extLoaded.has(s.name)) continue;
      this.extLoaded.add(s.name);
      try {
        const res = await api.ruleItems(s.sys.extensions, null, 100000, 0);
        if (!this.active && !this.loaded) return;
        const scoped = this.scopeExtItems(res.items, s.sys);
        const badgeById = new Map((res.badges ?? []).map((b) => [b.item_id, b]));
        let changed = false;
        for (const item of scoped) {
          const key = item.provider_key ?? item.id;
          if (this.gamesByKey.has(key)) continue;
          const b = badgeById.get(item.id);
          this.games.push({
            key,
            item,
            title: item.name.replace(/\.[^.]+$/, ""),
            datTitle: item.name,
            platform: null,
            alsoPlatforms: [],
            systemName: s.name,
            year: null,
            region: null,
            sha1: null,
            media: new Map(),
            state: {
              rating: null,
              favorite: false,
              completed: false,
              playCount: 0,
              lastPlayed: null,
              titleOverride: null,
              genre: null,
              players: null,
              notes: null,
            },
            starred: item.starred,
            labels: item.labels,
            auditorFlagged: false,
            offline: b?.offline ?? false,
            volumeLabel: b?.volume_label ?? null,
            volumeLastSeen: b?.last_seen ?? null,
            source: "ext",
            stateHydrated: false,
          });
          changed = true;
        }
        if (changed) {
          this.rebuildIndex();
          const rs = this.systems.find((x) => x.name === s.name);
          if (rs) {
            rs.count = this.games.filter((g) => g.systemName === s.name).length;
            rs.sizeBytes = this.games
              .filter((g) => g.systemName === s.name)
              .reduce((a, g) => a + (g.item.size_bytes ?? 0), 0);
          }
          // Personal state for the newcomers (path-keyed, one read).
          await this.hydrateExtState();
          if (this.active) {
            this.renderRail();
            this.renderShelf(true);
          }
        }
      } catch {
        // an extension shelf that fails to gather stays empty-honest
      }
    }
  }

  private scopeExtItems(items: Item[], sys: RomSystem): Item[] {
    const scope = romScopeOf(sys);
    if (scope.mode === "all") return items;
    // Boot/drives scoping needs location ids; the rule result items
    // carry their location — boot = non-volume locations are not
    // knowable here, so drives-scoped systems filter by drive ids.
    if (scope.mode === "drives") {
      const allowed = new Set(scope.drives);
      return items.filter((i) => allowed.has(i.location));
    }
    return items;
  }

  /** Extension games' marks live in the same path-keyed state
   * table; the games query only covers identified ROMs, so ext
   * games hydrate their state per path once, when they join. */
  private async hydrateExtState(): Promise<void> {
    const ext = this.games.filter((g) => g.source === "ext" && !g.stateHydrated);
    await Promise.all(
      ext.map(async (g) => {
        const pk = g.item.provider_key;
        if (!pk) {
          g.stateHydrated = true;
          return;
        }
        const st = await api.romHomeState(pk).catch(() => null);
        if (st) g.state = st;
        g.stateHydrated = true;
      }),
    );
  }

  // ------------------------------------------------------------ rail

  /** The system's icon in the chosen set, on NO tile of its own
   * (SPEC §3): the image itself, the flat outline in the theme's
   * label color, or the gamepad glyph for custom systems. */
  private iconEl(sysName: string, size: number): HTMLElement {
    const wrap = el("span", "rh-icon");
    wrap.style.width = `${size}px`;
    wrap.style.height = `${size}px`;
    wrap.setAttribute("aria-hidden", "true");
    const slug = romIconSlug(sysName);
    if (!slug) {
      wrap.textContent = "🎮";
      wrap.classList.add("rh-icon-glyph");
      return wrap;
    }
    const flat = (): void => {
      const svg = ROM_FLAT_ICONS[slug];
      if (svg) wrap.innerHTML = svg;
      else {
        wrap.textContent = "🎮";
        wrap.classList.add("rh-icon-glyph");
      }
    };
    const chosen = loadRomIconSet();
    if (chosen === "flat") {
      flat();
      return wrap;
    }
    const chain: string[] = [];
    for (const set of [chosen, "photo", "art"]) {
      if (!chain.includes(set)) chain.push(set);
    }
    const img = document.createElement("img");
    img.alt = "";
    img.draggable = false;
    let i = 0;
    img.addEventListener("error", () => {
      i += 1;
      if (i < chain.length) img.src = `/rom-icons/${chain[i]}/${slug}.webp`;
      else {
        img.remove();
        flat();
      }
    });
    img.src = `/rom-icons/${chain[0]}/${slug}.webp`;
    wrap.appendChild(img);
    return wrap;
  }

  private renderRail(): void {
    const rail = this.railEl;
    rail.innerHTML = "";
    const libHead = el("div", "rh-rail-head");
    libHead.appendChild(el("span", "rh-rail-title", "Library"));
    rail.appendChild(libHead);
    const lib = el("div", "rh-rail-group");
    const total = this.counts.total;
    lib.appendChild(this.shelfRow({ kind: "all" }, "🗄", "All Games", total > 0 ? String(total) : "—"));
    const favCount = this.games.filter((g) => g.state.favorite).length;
    lib.appendChild(this.shelfRow({ kind: "favorites" }, "♥", "Favorites", favCount > 0 ? String(favCount) : ""));
    const recentCount = this.games.filter((g) => g.state.lastPlayed).length;
    lib.appendChild(this.shelfRow({ kind: "recent" }, "🕘", "Recently Played", recentCount > 0 ? String(recentCount) : ""));
    lib.appendChild(
      this.shelfRow(
        { kind: "missing" },
        "🖼",
        "Missing Media",
        this.counts.missingMedia > 0 ? String(this.counts.missingMedia) : "",
      ),
    );
    rail.appendChild(lib);

    // Systems by division (generation default; maker available).
    const sysHead = el("div", "rh-rail-head");
    sysHead.appendChild(el("span", "rh-rail-title", "Systems"));
    const groupBtn = el("button", "rh-group-btn", this.grouping === "generation" ? "By generation" : "By maker");
    groupBtn.title = "Switch system grouping";
    groupBtn.addEventListener("click", () => {
      this.grouping = this.grouping === "generation" ? "maker" : "generation";
      this.saveState();
      this.renderRail();
    });
    sysHead.appendChild(groupBtn);
    rail.appendChild(sysHead);

    const systemsByName = new Map(this.systems.map((s) => [s.name, s]));
    const divisions = romDivisions(
      this.systems.map((s) => s.sys),
      this.grouping,
    );
    const currentSystem =
      this.shelf.kind === "system"
        ? this.shelf.name
        : this.shelf.kind === "hardware"
          ? this.shelf.name
          : null;
    for (const div of divisions) {
      const containsCurrent =
        currentSystem !== null &&
        (div.systems.some((s) => s.name === currentSystem) ||
          div.hardware.some((h) => h.name === currentSystem));
      const dEl = el("div", "rh-div");
      const head = el("button", "rh-div-head") as HTMLButtonElement;
      const isOpen = containsCurrent || !this.closedDivs().has(div.title);
      head.setAttribute("aria-expanded", String(isOpen));
      head.append(el("span", "side-chev", isOpen ? "▾" : "▸"), el("span", "rh-div-title", div.title));
      head.addEventListener("click", () => {
        const closed = this.closedDivs();
        if (closed.has(div.title)) closed.delete(div.title);
        else closed.add(div.title);
        this.saveClosedDivs(closed);
        this.renderRail();
      });
      dEl.appendChild(head);
      if (isOpen) {
        const box = el("div", "rh-div-body");
        for (const sys of div.systems) {
          const rs = systemsByName.get(sys.name);
          box.appendChild(this.systemRow(sys, rs));
        }
        for (const hw of div.hardware) {
          box.appendChild(this.hardwareRow(hw));
        }
        dEl.appendChild(box);
      }
      rail.appendChild(dEl);
    }

    // Lists: manual ROM Lists (Phase-2 collections) + Smart Lists.
    const listHead = el("div", "rh-rail-head");
    listHead.appendChild(el("span", "rh-rail-title", "Lists"));
    const newBtn = el("button", "rh-group-btn", "+ New List");
    newBtn.addEventListener("click", () => void this.newList());
    listHead.appendChild(newBtn);
    rail.appendChild(listHead);
    const lists = el("div", "rh-rail-group");
    for (const c of this.collections) {
      if (c.kind === "smart") continue;
      lists.appendChild(
        this.shelfRow({ kind: "list", id: c.id }, "📃", c.name, c.member_count > 0 ? String(c.member_count) : ""),
      );
    }
    for (const s of this.smartLists) {
      const row = this.shelfRow({ kind: "smart", id: s.id }, "✦", s.name, "");
      row.title = this.smartSentence(s);
      lists.appendChild(row);
    }
    if (this.collections.length === 0 && this.smartLists.length === 0) {
      lists.appendChild(el("p", "rh-rail-empty", "No lists yet — curate your first."));
    }
    rail.appendChild(lists);

    // Library health footer.
    this.healthEl = el("button", "rh-health") as HTMLButtonElement;
    if (this.counts.missingMedia > 0) {
      this.healthEl.textContent = `⚕ ${this.counts.missingMedia.toLocaleString()} game${this.counts.missingMedia === 1 ? "" : "s"} missing media`;
    } else if (this.counts.total > 0) {
      this.healthEl.textContent = "⚕ Every game has its media";
    } else {
      this.healthEl.textContent = "⚕ No games yet";
    }
    this.healthEl.addEventListener("click", () => {
      if (this.counts.missingMedia > 0) this.setShelf({ kind: "missing" });
    });
    rail.appendChild(this.healthEl);
  }

  private closedDivs(): Set<string> {
    try {
      const raw = localStorage.getItem("ff-romhome-closed-divs");
      if (raw) return new Set(JSON.parse(raw) as string[]);
    } catch {
      // none
    }
    return new Set();
  }

  private saveClosedDivs(s: Set<string>): void {
    try {
      localStorage.setItem("ff-romhome-closed-divs", JSON.stringify([...s]));
    } catch {
      // ignore
    }
  }

  private shelfRow(ref: ShelfRef, glyph: string, label: string, count: string): HTMLButtonElement {
    const b = el("button", "rh-row") as HTMLButtonElement;
    if (
      this.shelfKey(ref) === this.shelfKey() ||
      (ref.kind === "all" && this.shelf.kind === "search")
    ) {
      b.classList.add("selected");
    }
    const g = el("span", "rh-row-glyph", glyph);
    const l = el("span", "rh-row-label", label);
    b.append(g, l);
    if (count) b.appendChild(el("span", "rh-row-count", count));
    b.addEventListener("click", () => this.setShelf(ref));
    if (ref.kind === "list") {
      const listId = ref.id;
      b.addEventListener("dragover", (e) => {
        e.preventDefault();
        b.classList.add("drop-hover");
      });
      b.addEventListener("dragleave", () => b.classList.remove("drop-hover"));
      b.addEventListener("drop", (e) => {
        e.preventDefault();
        b.classList.remove("drop-hover");
        const raw = e.dataTransfer?.getData("application/x-fable-romhome");
        if (!raw) return;
        try {
          const keys = JSON.parse(raw) as string[];
          const games = keys
            .map((k) => this.gamesByKey.get(k))
            .filter((v): v is RoomGame => !!v);
          if (games.length > 0) void this.addToList(games, listId);
        } catch {
          // a foreign drag is not ours to judge
        }
      });
    }
    return b;
  }

  private systemRow(sys: RomSystem, rs: RoomSystem | undefined): HTMLButtonElement {
    const b = el("button", "rh-row rh-sys-row") as HTMLButtonElement;
    if (this.shelf.kind === "system" && this.shelf.name === sys.name) b.classList.add("selected");
    b.appendChild(this.iconEl(sys.name, 36));
    b.appendChild(el("span", "rh-row-label", sys.name));
    const offline = this.systemOffline(sys.name);
    if (offline) b.classList.add("offline");
    if (rs?.placeholder) {
      b.appendChild(el("span", "rh-row-count", "—"));
    } else if (rs && rs.count !== null) {
      b.appendChild(el("span", "rh-row-count", rs.count > 0 ? rs.count.toLocaleString() : "0"));
    }
    b.addEventListener("click", () => this.setShelf({ kind: "system", name: sys.name }));
    b.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      this.hooks.onManageSystem(sys, e.clientX, e.clientY);
    });
    return b;
  }

  private hardwareRow(hw: OwnedHardware): HTMLButtonElement {
    const b = el("button", "rh-row rh-hw-row") as HTMLButtonElement;
    if (this.shelf.kind === "hardware" && this.shelf.name === hw.name) b.classList.add("selected");
    b.append(el("span", "rh-row-glyph", "🕹"), el("span", "rh-row-label", hw.name));
    b.title = `Owned hardware (${hw.year}) — no ROM library`;
    b.addEventListener("click", () => this.setShelf({ kind: "hardware", name: hw.name }));
    return b;
  }

  /** A system is "offline" when it has games and every one of them
   * sits on a disconnected volume (Library Memory truth). */
  private systemOffline(name: string): boolean {
    const gs = this.games.filter((g) => g.systemName === name);
    return gs.length > 0 && gs.every((g) => g.offline);
  }

  private systemVolumeLine(name: string): string | null {
    const gs = this.games.filter((g) => g.systemName === name);
    if (gs.length === 0) return null;
    const first = gs[0];
    if (!first.offline || !first.volumeLabel) return null;
    return `${first.volumeLabel} · last seen ${fmtDay(first.volumeLastSeen)}`;
  }

  private async newList(): Promise<void> {
    const name = await promptDialog("New List", "List name");
    if (!name || !name.trim()) return;
    try {
      await api.createCollection(name.trim());
      const cols = await api.listCollections().catch(() => this.collections);
      this.collections = cols;
      this.renderRail();
      Toast.success(`List “${name.trim()}” created.`);
    } catch (err) {
      Toast.error(`Could not create the list: ${errText(err)}`);
    }
  }

  private smartSentence(s: RomSmartList): string {
    try {
      const f = JSON.parse(s.queryJson) as RoomFilters;
      return filterSentence({ ...structuredClone(EMPTY_FILTERS), ...f }) || s.name;
    } catch {
      return s.name;
    }
  }

  // ------------------------------------------------------------ shelves

  private setShelf(ref: ShelfRef): void {
    if (this.shelfKey(ref) !== this.shelfKey()) this.editingSmartId = null;
    this.saveScroll();
    this.shelf = ref;
    this.selection.clear();
    this.anchorKey = null;
    this.saveState();
    this.renderRail();
    this.renderShelf();
    this.restoreScroll();
    this.renderInspector();
  }

  private saveScroll(): void {
    if (this.shelfScroll) {
      this.scrollByShelf.set(this.shelfKey(), this.shelfScroll.scrollTop);
    }
  }

  private restoreScroll(): void {
    if (!this.shelfScroll) return;
    const top = this.scrollByShelf.get(this.shelfKey()) ?? 0;
    requestAnimationFrame(() => {
      if (this.shelfScroll) this.shelfScroll.scrollTop = top;
    });
  }

  /** The games a shelf holds, before filters/sort. */
  private shelfGames(ref: ShelfRef = this.shelf): RoomGame[] {
    switch (ref.kind) {
      case "all":
        return [...this.games];
      case "favorites":
        return this.games.filter((g) => g.state.favorite);
      case "recent":
        return this.games.filter((g) => g.state.lastPlayed !== null);
      case "missing":
        return this.games.filter((g) => g.source === "dat" && !g.media.has("boxart"));
      case "system":
        return this.games.filter(
          (g) =>
            g.systemName === ref.name ||
            (g.platform !== null &&
              (g.platform === this.systemKeyOf(ref.name) ||
                g.alsoPlatforms.includes(this.systemKeyOf(ref.name) ?? ""))),
        );
      case "list": {
        const memberIds = new Set(this.listMemberKeys(ref.id));
        return this.games.filter((g) => memberIds.has(g.key) || memberIds.has(g.item.id));
      }
      case "smart": {
        const s = this.smartLists.find((x) => x.id === ref.id);
        if (!s) return [];
        let f: RoomFilters = structuredClone(EMPTY_FILTERS);
        try {
          f = { ...f, ...(JSON.parse(s.queryJson) as RoomFilters) };
        } catch {
          return [];
        }
        return this.games.filter((g) => this.matchesFilters(g, f, true));
      }
      case "search": {
        const q = ref.query.toLowerCase();
        return this.games.filter(
          (g) =>
            g.title.toLowerCase().includes(q) ||
            g.datTitle.toLowerCase().includes(q) ||
            (g.state.genre ?? "").toLowerCase().includes(q) ||
            g.labels.some((l) => l.toLowerCase().includes(q)) ||
            g.systemName.toLowerCase().includes(q),
        );
      }
      case "hardware":
        return [];
    }
  }

  private listMemberKeysCache = new Map<string, string[]>();
  private listMemberKeys(id: string): string[] {
    return this.listMemberKeysCache.get(id) ?? [];
  }

  private async loadListMembers(id: string): Promise<void> {
    try {
      const contents = await api.collectionItems(id);
      const keys = contents.items.map((i) => i.provider_key ?? i.id);
      this.listMemberKeysCache.set(id, keys);
    } catch {
      this.listMemberKeysCache.set(id, []);
    }
  }

  private systemKeyOf(name: string): string | null {
    const rs = this.systems.find((s) => s.name === name);
    return rs?.key ?? null;
  }

  private matchesFilters(g: RoomGame, f: RoomFilters, ignoreSystems = false): boolean {
    if (!ignoreSystems && f.systems.length > 0) {
      const keys = [g.platform, ...g.alsoPlatforms].filter(Boolean) as string[];
      if (!keys.some((k) => f.systems.includes(k))) return false;
    }
    if (f.genres.length > 0) {
      if (!g.state.genre || !f.genres.includes(g.state.genre)) return false;
    }
    if (f.players.length > 0) {
      const b = playersBucket(g.state.players);
      if (!b || !f.players.includes(b)) return false;
    }
    for (const m of f.media) {
      if (m === "has-art" && !g.media.has("boxart")) return false;
      if (m === "missing-art" && g.media.has("boxart")) return false;
      if (m === "has-video" && !g.media.has("trailer")) return false;
    }
    for (const fl of f.flags) {
      if (fl === "starred" && !(g.starred || (g.state.rating ?? 0) >= 1)) return false;
      if (fl === "favorites" && !g.state.favorite) return false;
      if (fl === "completed" && !g.state.completed) return false;
      if (fl === "unplayed" && g.state.playCount > 0) return false;
    }
    return true;
  }

  private visibleGames(): RoomGame[] {
    let games = this.shelfGames();
    const f = this.filters;
    const hasFilters =
      f.systems.length + f.genres.length + f.players.length + f.media.length + f.flags.length > 0;
    if (hasFilters && this.shelf.kind !== "smart") {
      games = games.filter((g) => this.matchesFilters(g, f));
    }
    const byTitle = (a: RoomGame, b: RoomGame) => a.title.localeCompare(b.title, undefined, { sensitivity: "base" });
    switch (this.sort) {
      case "title":
        games.sort(byTitle);
        break;
      case "year":
        games.sort((a, b) => (b.year ?? -1) - (a.year ?? -1) || byTitle(a, b));
        break;
      case "recent-played":
        games.sort((a, b) => (b.state.lastPlayed ?? "").localeCompare(a.state.lastPlayed ?? "") || byTitle(a, b));
        break;
      case "most-played":
        games.sort((a, b) => b.state.playCount - a.state.playCount || byTitle(a, b));
        break;
      case "recent-added":
        games.sort((a, b) => (b.item.modified ?? "").localeCompare(a.item.modified ?? "") || byTitle(a, b));
        break;
    }
    return games;
  }

  // ------------------------------------------------------------ shelf

  private renderShelf(keepScroll = false): void {
    // Lists resolve their members first (catalog read), then paint.
    if (this.shelf.kind === "list" && !this.listMemberKeysCache.has(this.shelf.id)) {
      void this.loadListMembers(this.shelf.id).then(() => {
        if (this.active && this.shelf.kind === "list") this.renderShelf();
      });
    }
    const shelf = this.shelfEl;
    shelf.innerHTML = "";
    this.renderChipsBar();
    shelf.appendChild(this.shelfHeader());
    const scroll = el("div", "rh-scroll");
    this.shelfScroll = scroll;
    shelf.appendChild(scroll);
    if (this.shelf.kind === "list" && !this.listMemberKeysCache.has(this.shelf.id)) {
      const note = el("div", "rh-empty");
      note.append(el("div", "rh-empty-emoji", "📃"), el("h3", "", "Gathering the list…"));
      scroll.appendChild(note);
      return;
    }

    // Hardware door: the honest explanation, never a fake shelf.
    if (this.shelf.kind === "hardware") {
      const hw = OWNED_HARDWARE.find((h) => h.name === (this.shelf as { name: string }).name);
      const note = el("div", "rh-empty");
      note.append(
        el("div", "rh-empty-emoji", "🕹"),
        el("h3", "", `The ${hw?.name ?? "hardware"} on your shelf`),
        el(
          "p",
          "",
          "Fable Files tracks ROM libraries, and this console plays physical or store-bought media — there's no ROM shelf to gather here. It keeps its seat in the rail because the hardware is real.",
        ),
      );
      scroll.appendChild(note);
      return;
    }
    // Placeholder system: the coming-soon door.
    if (this.shelf.kind === "system") {
      const rs = this.systems.find((s) => s.name === (this.shelf as { name: string }).name);
      if (rs?.placeholder && (rs.count ?? 0) === 0) {
        scroll.appendChild(this.placeholderNote(rs));
        return;
      }
    }

    const games = this.visibleGames();
    if (games.length === 0) {
      scroll.appendChild(this.emptyState());
      return;
    }
    if (this.view === "grid") this.renderGrid(scroll, games);
    else this.renderList(scroll, games);
    if (keepScroll) this.restoreScroll();
    this.attachKeys(scroll);
  }

  private placeholderNote(rs: RoomSystem): HTMLElement {
    const note = el("div", "rh-empty");
    const meta = romSystemMeta(rs.name);
    note.append(
      this.iconEl(rs.name, 96),
      el("h3", "", rs.name),
      el("p", "rh-empty-sub", `${meta.gen !== null ? `${ordinal(meta.gen)} Generation · ` : ""}${meta.maker}`),
      el(
        "p",
        "",
        "A placeholder for what's coming. When the emulators arrive, this shelf wakes up.",
      ),
    );
    return note;
  }

  private emptyState(): HTMLElement {
    const note = el("div", "rh-empty");
    const add = (emoji: string, title: string, body: string): void => {
      note.append(el("div", "rh-empty-emoji", emoji), el("h3", "", title), el("p", "", body));
    };
    switch (this.shelf.kind) {
      case "missing":
        add(
          "🎉",
          "Every game has its media",
          "Nothing to fetch — this shelf celebrates being done. When new games are identified without media, they'll line up here.",
        );
        break;
      case "favorites":
        add("♥", "No favorites yet", "Open a game and tap the heart — the ones you love gather here.");
        break;
      case "recent":
        add("🕘", "Nothing played yet", "Press Play on any game and it takes its place here, most recent first.");
        break;
      case "search":
        add("🔍", "No games match", "Try a shorter title, a genre, or a system name — search reaches every shelf at once.");
        break;
      case "system": {
        const rs = this.systems.find((s) => s.name === (this.shelf as { name: string }).name);
        note.append(
          el("div", "rh-empty-emoji", "🎮"),
          el("h3", "", `No ${rs?.name ?? "games"} ROMs yet`),
        );
        const p = el("p", "");
        p.append(
          document.createTextNode("Games come from your own dumps, homebrew, and public-domain releases. Add a folder of ROMs and they'll be identified against the DAT sets. You can also hunt the sets at "),
        );
        const link = el("button", "linklike", "archive.org");
        link.addEventListener("click", () => {
          if (rs) openGuideLink(romArchiveLink(rs.key ?? "").url);
        });
        p.append(link, document.createTextNode("."));
        note.appendChild(p);
        const btn = el("button", "pill-btn rh-empty-action", "Add a ROM folder…") as HTMLButtonElement;
        btn.addEventListener("click", () => this.hooks.onAddRomFolder());
        note.appendChild(btn);
        break;
      }
      case "list":
        add("📃", "This list is empty", "Drag games onto it from any shelf, or use a game's right-click menu.");
        break;
      case "smart":
        add("✦", "Nothing matches… yet", "This list re-judges every game against its rule whenever you open it. As the library grows, matches gather here on their own.");
        break;
      default:
        add("🎮", "No ROMs yet", "This room is your collection's home — shelves of art, your ratings and history, every system's library in one calm place. Add a folder of ROMs and Fable will identify them against the DAT sets — then the shelves fill themselves.");
        if (this.counts.total === 0) {
          const btn = el("button", "pill-btn rh-empty-action", "Add a ROM folder…") as HTMLButtonElement;
          btn.addEventListener("click", () => this.hooks.onAddRomFolder());
          note.appendChild(btn);
        }
    }
    return note;
  }

  private shelfHeader(): HTMLElement {
    const head = el("div", "rh-shelf-head");
    const ref = this.shelf;
    let title = "";
    let sub = "";
    let hero: HTMLElement | null = null;
    let missingForFetch = 0;
    switch (ref.kind) {
      case "all":
        title = "All Games";
        sub = `${this.games.length.toLocaleString()} games`;
        missingForFetch = this.counts.missingMedia;
        break;
      case "favorites":
        title = "Favorites";
        sub = "The ones you love";
        break;
      case "recent":
        title = "Recently Played";
        sub = "Your history, newest first";
        break;
      case "missing":
        title = "Missing Media";
        sub = `${this.counts.missingMedia.toLocaleString()} games missing box art — fetch them right here`;
        missingForFetch = this.counts.missingMedia;
        break;
      case "system": {
        const rs = this.systems.find((s) => s.name === ref.name);
        title = ref.name;
        hero = this.iconEl(ref.name, 96);
        hero.classList.add("rh-hero-icon");
        if (rs) {
          const meta = romSystemMeta(ref.name);
          const n = this.shelfGames().length;
          const size = this.shelfGames().reduce((a, g) => a + (g.item.size_bytes ?? 0), 0);
          sub = `${meta.gen !== null ? `${ordinal(meta.gen)} Generation · ` : ""}${n.toLocaleString()} games${size > 0 ? ` · ${formatBytes(size)}` : ""}`;
          missingForFetch = rs.missing;
          const vol = this.systemVolumeLine(ref.name);
          if (vol) sub += ` — offline (${vol})`;
        }
        break;
      }
      case "hardware":
        title = ref.name;
        sub = "Owned hardware";
        break;
      case "list": {
        const c = this.collections.find((x) => x.id === ref.id);
        title = c?.name ?? "List";
        sub = "A shelf you fill by hand";
        break;
      }
      case "smart": {
        const s = this.smartLists.find((x) => x.id === ref.id);
        title = s?.name ?? "Smart List";
        sub = s ? this.smartSentence(s) : "";
        break;
      }
      case "search":
        title = `“${ref.query}”`;
        sub = `${this.shelfGames().length.toLocaleString()} games across every shelf`;
        break;
    }
    if (hero) head.appendChild(hero);
    const texts = el("div", "rh-head-texts");
    texts.append(el("h2", "rh-head-title", title), el("p", "rh-head-sub", sub));
    head.appendChild(texts);
    const actions = el("div", "rh-head-actions");
    if (ref.kind === "smart") {
      const editRule = el("button", "pill-btn rh-edit-rule", "Edit rule") as HTMLButtonElement;
      editRule.addEventListener("click", () => {
        const s2 = this.smartLists.find((x) => x.id === (ref as { id: string }).id);
        if (!s2) return;
        try {
          this.filters = {
            ...structuredClone(EMPTY_FILTERS),
            ...(JSON.parse(s2.queryJson) as RoomFilters),
          };
        } catch {
          this.filters = structuredClone(EMPTY_FILTERS);
        }
        this.editingSmartId = s2.id;
        this.renderShelf(true);
      });
      actions.appendChild(editRule);
    }
    const dice = el("button", "icon-btn rh-dice", "🎲") as HTMLButtonElement;
    dice.title = "Surprise me — a random game from this shelf";
    dice.addEventListener("click", () => this.surprise());
    actions.appendChild(dice);
    if (missingForFetch > 0) {
      const get = el(
        "button",
        "pill-btn primary rh-get-media",
        `Get missing media (${missingForFetch})`,
      ) as HTMLButtonElement;
      get.addEventListener("click", () => this.fetchMissingForShelf());
      actions.appendChild(get);
    }
    head.appendChild(actions);
    return head;
  }

  /** Surprise me: a random game off this shelf, straight into the
   * inspector (the spec's serendipity door — no auto-play). */
  private surprise(): void {
    const games = this.visibleGames().filter((g) => !g.offline);
    const pool = games.length > 0 ? games : this.visibleGames();
    if (pool.length === 0) return;
    const g = pool[Math.floor(Math.random() * pool.length)];
    this.selection = new Set([g.key]);
    this.anchorKey = g.key;
    this.renderShelf(true);
    this.renderInspector();
    const tile = this.shelfScroll?.querySelector<HTMLElement>(`[data-key="${CSS.escape(g.key)}"]`);
    tile?.scrollIntoView({ block: "center", behavior: reducedMotion() ? "auto" : "smooth" });
    tile?.focus({ preventScroll: true });
  }

  /** Start a fetch through the existing enrichment machinery
   * (api.startRomMediaDownload with retry pairs — the same door
   * the media report's per-item retry uses). The jobs panel shows
   * progress; when the job lands, the room refreshes itself. */
  private async startFetch(
    retry: { itemId: string; mediaType: string }[],
    note: string,
  ): Promise<void> {
    if (retry.length === 0) {
      Toast.show("Nothing missing to fetch.");
      return;
    }
    try {
      const start = await api.startRomMediaDownload({
        systems: null,
        systemLabel: null,
        retry,
      });
      if (start.alreadyRunning) {
        Toast.info("That download is already running — watch it in the jobs panel.");
      } else {
        Toast.success(note);
      }
    } catch (err) {
      Toast.info(errText(err));
    }
  }

  /** Get missing media for this shelf: every identified game here
   * without art queues through the existing fetch door. */
  private fetchMissingForShelf(): void {
    const games = this.shelfGames().filter(
      (g) => g.source === "dat" && !g.offline && !g.media.has("boxart"),
    );
    const retry: { itemId: string; mediaType: "boxart" | "screenshot" | "titleshot" | "trailer" }[] = [];
    for (const g of games) {
      for (const t of MEDIA_TYPES) {
        if (!g.media.has(t)) retry.push({ itemId: g.item.id, mediaType: t });
      }
    }
    void this.startFetch(
      retry,
      `Fetching ${retry.length} missing ${retry.length === 1 ? "file" : "files"} — watch it in the jobs panel.`,
    );
  }

  // ------------------------------------------------------------ chips bar

  private renderChipsBar(): void {
    const bar = this.chipsBar;
    bar.innerHTML = "";
    const f = this.filters;
    const chips: { label: string; clear: () => void }[] = [];
    const pushGroup = (values: string[], group: keyof RoomFilters, label: (v: string) => string) => {
      for (const v of values) {
        chips.push({
          label: label(v),
          clear: () => {
            this.filters[group] = (this.filters[group] as string[]).filter((x) => x !== v);
            this.afterFilterChange();
          },
        });
      }
    };
    pushGroup(f.systems, "systems", (v) => `System: ${systemNameForKey(v)}`);
    pushGroup(f.genres, "genres", (v) => `Genre: ${v}`);
    pushGroup(f.players, "players", (v) => (v === "1" ? "1 player" : v === "2" ? "2 players" : v === "3-4" ? "3–4 players" : "5+ players"));
    pushGroup(f.media, "media", (v) => (v === "has-art" ? "Has box art" : v === "missing-art" ? "Missing box art" : "Has video"));
    pushGroup(f.flags, "flags", (v) => v.charAt(0).toUpperCase() + v.slice(1));
    if (chips.length === 0) {
      bar.hidden = true;
      return;
    }
    bar.hidden = false;
    for (const c of chips) {
      const chip = el("button", "rh-chip", `${c.label} ✕`) as HTMLButtonElement;
      chip.title = "Remove this filter";
      chip.addEventListener("click", c.clear);
      bar.appendChild(chip);
    }
    const count = el("span", "rh-chip-count", `${this.visibleGames().length.toLocaleString()} games`);
    bar.appendChild(count);
    const clearAll = el("button", "rh-chip-clear", "Clear") as HTMLButtonElement;
    clearAll.addEventListener("click", () => {
      this.filters = structuredClone(EMPTY_FILTERS);
      this.afterFilterChange();
    });
    bar.appendChild(clearAll);
    if (this.editingSmartId) {
      const editing = this.smartLists.find((x) => x.id === this.editingSmartId);
      const save = el(
        "button",
        "rh-chip-keep",
        `Save rule to “${editing?.name ?? "list"}”`,
      ) as HTMLButtonElement;
      save.addEventListener("click", () => void this.saveSmartRule());
      bar.appendChild(save);
      const done = el("button", "rh-chip-clear", "Done editing") as HTMLButtonElement;
      done.addEventListener("click", () => {
        this.editingSmartId = null;
        this.filters = structuredClone(EMPTY_FILTERS);
        this.renderShelf(true);
      });
      bar.appendChild(done);
    } else {
      const keep = el("button", "rh-chip-keep", "Keep as Smart List") as HTMLButtonElement;
      keep.addEventListener("click", () => void this.keepAsSmartList());
      bar.appendChild(keep);
    }
  }

  /** Write the edited chips back onto the same Smart List (§4). */
  private async saveSmartRule(): Promise<void> {
    const id = this.editingSmartId;
    if (!id) return;
    try {
      await api.romHomeUpdateSmartList(id, JSON.stringify(this.filters));
      this.smartLists = await api.romHomeSmartLists().catch(() => this.smartLists);
      this.editingSmartId = null;
      this.filters = structuredClone(EMPTY_FILTERS);
      this.renderRail();
      this.renderShelf(true);
      Toast.success("The list's rule is updated — it re-judges itself from here.");
    } catch (err) {
      Toast.error(`Could not save the rule: ${errText(err)}`);
    }
  }

  private afterFilterChange(): void {
    this.saveState();
    this.renderShelf(true);
  }

  private async keepAsSmartList(): Promise<void> {
    const name = await promptDialog(
      `Keep as Smart List — ${filterSentence(this.filters) || "this filter"}`,
      "List name",
    );
    if (!name || !name.trim()) return;
    try {
      const id = await api.romHomeSaveSmartList(name.trim(), JSON.stringify(this.filters));
      const lists = await api.romHomeSmartLists().catch(() => this.smartLists);
      this.smartLists = lists;
      this.filters = structuredClone(EMPTY_FILTERS);
      this.renderRail();
      this.setShelf({ kind: "smart", id });
      Toast.success(`Smart List “${name.trim()}” is keeping watch.`);
    } catch (err) {
      Toast.error(`Could not save the list: ${errText(err)}`);
    }
  }

  // ------------------------------------------------------------ grid

  private renderGrid(scroll: HTMLElement, games: RoomGame[]): void {
    // Search results group by system; a shelf renders flat.
    if (this.shelf.kind === "search") {
      const bySystem = new Map<string, RoomGame[]>();
      for (const g of games) {
        const arr = bySystem.get(g.systemName) ?? [];
        arr.push(g);
        bySystem.set(g.systemName, arr);
      }
      for (const [name, gs] of bySystem) {
        scroll.appendChild(el("h3", "rh-group-head", `${name} · ${gs.length}`));
        const grid = el("div", "rh-grid");
        this.fillGrid(grid, gs);
        scroll.appendChild(grid);
      }
      const esc = el("div", "rh-search-escape");
      const escBtn = el("button", "linklike rh-escape-btn", "Also search all files…") as HTMLButtonElement;
      escBtn.addEventListener("click", () => {
        if (this.shelf.kind === "search") this.hooks.onSearchAllFiles(this.shelf.query);
      });
      esc.appendChild(escBtn);
      scroll.appendChild(esc);
      return;
    }
    const grid = el("div", "rh-grid");
    this.fillGrid(grid, games);
    scroll.appendChild(grid);
  }

  /** Chunked fill: first 240 tiles paint immediately, the rest
   * append as the reader scrolls (a 1,051-game shelf stays light). */
  private fillGrid(grid: HTMLElement, games: RoomGame[]): void {
    const CHUNK = 240;
    let idx = 0;
    const addChunk = (): void => {
      const frag = document.createDocumentFragment();
      for (let n = 0; n < CHUNK && idx < games.length; n++, idx++) {
        frag.appendChild(this.tileEl(games[idx], games));
      }
      grid.appendChild(frag);
    };
    addChunk();
    if (idx < games.length) {
      const sentinel = el("div", "rh-sentinel");
      grid.appendChild(sentinel);
      const io = new IntersectionObserver(
        (entries) => {
          if (entries.some((e) => e.isIntersecting)) {
            sentinel.remove();
            addChunk();
            if (idx < games.length) {
              grid.appendChild(sentinel);
              io.observe(sentinel);
            } else {
              io.disconnect();
            }
          }
        },
        { root: this.shelfScroll, rootMargin: "600px" },
      );
      io.observe(sentinel);
    }
  }

  private tileEl(g: RoomGame, ordered: RoomGame[]): HTMLElement {
    const tile = el("article", "rh-tile");
    tile.dataset.key = g.key;
    tile.tabIndex = -1;
    tile.setAttribute("role", "option");
    tile.setAttribute("aria-selected", String(this.selection.has(g.key)));
    if (this.selection.has(g.key)) tile.classList.add("selected");
    if (g.offline) tile.classList.add("offline");
    tile.draggable = true;

    // Art frame: the image in its TRUE aspect on the theme surface
    // (§3), or a system-tinted typographic placeholder — never a
    // gray void, never stretched.
    const frame = el("div", "rh-art");
    const art = g.media.get("boxart");
    if (art) {
      const img = document.createElement("img");
      img.className = "rh-art-img";
      img.alt = "";
      img.loading = "lazy";
      img.draggable = false;
      img.src = fileSrc(art);
      img.addEventListener("load", () => {
        if (img.naturalWidth > 0) {
          frame.style.aspectRatio = `${img.naturalWidth} / ${img.naturalHeight}`;
        }
      });
      img.addEventListener("error", () => {
        img.remove();
        frame.appendChild(this.placeholderEl(g));
      });
      frame.appendChild(img);
    } else {
      frame.appendChild(this.placeholderEl(g));
    }
    const badges = el("div", "rh-badges");
    if ((g.state.rating ?? 0) >= 1 || g.starred) {
      badges.appendChild(el("span", "rh-badge rh-badge-star", "★"));
    }
    if (g.state.completed) badges.appendChild(el("span", "rh-badge rh-badge-done", "✓"));
    if (!g.media.has("boxart")) {
      const dot = el("span", "rh-badge rh-badge-nomedia");
      dot.title = "Missing box art";
      badges.appendChild(dot);
    }
    frame.appendChild(badges);
    tile.appendChild(frame);

    const titleRow = el("div", "rh-tile-title");
    const t = el("span", "rh-tile-name", g.title);
    t.title = g.title;
    titleRow.appendChild(t);
    if (g.year) titleRow.appendChild(el("span", "rh-tile-year", String(g.year)));
    tile.appendChild(titleRow);

    tile.addEventListener("click", (e) => {
      this.selectFromPointer(g, ordered, e);
      tile.focus({ preventScroll: true });
    });
    tile.addEventListener("dblclick", () => {
      if (!g.offline) void this.playGame(g);
    });
    tile.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      if (!this.selection.has(g.key)) {
        this.selection = new Set([g.key]);
        this.anchorKey = g.key;
        this.syncSelectionDom();
        this.renderInspector();
      }
      this.openGameMenu(g, e.clientX, e.clientY);
    });
    tile.addEventListener("dragstart", (e) => {
      const keys = this.selection.has(g.key) ? [...this.selection] : [g.key];
      e.dataTransfer?.setData("application/x-fable-romhome", JSON.stringify(keys));
      if (e.dataTransfer) e.dataTransfer.effectAllowed = "copy";
    });
    return tile;
  }

  /** Typographic placeholder: the title set large over a system
   * tint — a designed stand-in, honest that the art isn't here. */
  private placeholderEl(g: RoomGame): HTMLElement {
    const p = el("div", "rh-placeholder");
    const hue = systemHue(g.systemName);
    p.style.background = `color-mix(in srgb, hsl(${hue} 55% 50%) 16%, transparent)`;
    p.style.color = `hsl(${hue} 45% 42%)`;
    const initial = el("span", "rh-ph-initial", (g.title.trim()[0] ?? "?").toUpperCase());
    const name = el("span", "rh-ph-name", g.title);
    const sys = el("span", "rh-ph-sys", g.systemName);
    p.append(initial, name, sys);
    return p;
  }

  private renderList(scroll: HTMLElement, games: RoomGame[]): void {
    const table = el("div", "rh-table");
    const head = el("div", "rh-tr rh-tr-head");
    const cols: [string, SortKey | null][] = [
      ["Title", "title"],
      ["Year", "year"],
      ["Genre", null],
      ["Players", null],
      ["Media", null],
      ["Last played", "recent-played"],
    ];
    cols.forEach(([label, key], i) => {
      const c = el("button", "rh-th", label) as HTMLButtonElement;
      if (key && this.sort === key) c.classList.add("sorted");
      if (key) {
        c.addEventListener("click", () => {
          this.sort = key;
          this.saveState();
          this.renderShelf(true);
        });
      }
      c.dataset.col = String(i);
      head.appendChild(c);
    });
    table.appendChild(head);
    const CHUNK = 300;
    let idx = 0;
    const addChunk = (): void => {
      const frag = document.createDocumentFragment();
      for (let n = 0; n < CHUNK && idx < games.length; n++, idx++) {
        frag.appendChild(this.rowEl(games[idx], games));
      }
      table.appendChild(frag);
    };
    addChunk();
    scroll.appendChild(table);
    if (idx < games.length) {
      const more = el("button", "pill-btn rh-more-rows", `Show more (${games.length - idx} more)`);
      more.addEventListener("click", () => {
        addChunk();
        if (idx >= games.length) more.remove();
        else more.textContent = `Show more (${games.length - idx} more)`;
      });
      scroll.appendChild(more);
    }
  }

  private rowEl(g: RoomGame, ordered: RoomGame[]): HTMLElement {
    const row = el("div", "rh-tr");
    row.dataset.key = g.key;
    row.tabIndex = -1;
    if (this.selection.has(g.key)) row.classList.add("selected");
    if (g.offline) row.classList.add("offline");
    const cells: string[] = [
      g.title,
      g.year ? String(g.year) : "",
      g.state.genre ?? "",
      g.state.players ?? "",
      "",
      g.state.lastPlayed ? fmtDay(g.state.lastPlayed) : "",
    ];
    cells.forEach((text, i) => {
      const c = el("span", "rh-td", text);
      c.dataset.col = String(i);
      if (i === 4) {
        const dots = el("span", "rh-media-dots");
        for (const t of MEDIA_TYPES) {
          const d = el("span", g.media.has(t) ? "rh-dot on" : "rh-dot");
          d.title = `${MEDIA_LABEL[t]}: ${g.media.has(t) ? "have it" : "missing"}`;
          dots.appendChild(d);
        }
        c.appendChild(dots);
      }
      row.appendChild(c);
    });
    row.addEventListener("click", (e) => {
      this.selectFromPointer(g, ordered, e);
      row.focus({ preventScroll: true });
    });
    row.addEventListener("dblclick", () => {
      if (!g.offline) void this.playGame(g);
    });
    row.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      if (!this.selection.has(g.key)) {
        this.selection = new Set([g.key]);
        this.anchorKey = g.key;
        this.syncSelectionDom();
        this.renderInspector();
      }
      this.openGameMenu(g, e.clientX, e.clientY);
    });
    return row;
  }

  // ------------------------------------------------------------ selection

  private selectFromPointer(g: RoomGame, ordered: RoomGame[], e: MouseEvent): void {
    if (e.shiftKey && this.anchorKey) {
      const a = ordered.findIndex((x) => x.key === this.anchorKey);
      const b = ordered.findIndex((x) => x.key === g.key);
      if (a >= 0 && b >= 0) {
        const [lo, hi] = a < b ? [a, b] : [b, a];
        if (!e.metaKey && !e.ctrlKey) this.selection.clear();
        for (let i = lo; i <= hi; i++) this.selection.add(ordered[i].key);
      }
    } else if (e.metaKey || e.ctrlKey) {
      if (this.selection.has(g.key)) this.selection.delete(g.key);
      else this.selection.add(g.key);
      this.anchorKey = g.key;
    } else {
      this.selection = new Set([g.key]);
      this.anchorKey = g.key;
    }
    this.syncSelectionDom();
    this.renderInspector();
  }

  private syncSelectionDom(): void {
    this.shelfScroll
      ?.querySelectorAll<HTMLElement>("[data-key]")
      .forEach((n) => {
        const on = this.selection.has(n.dataset.key ?? "");
        n.classList.toggle("selected", on);
        n.setAttribute("aria-selected", String(on));
      });
  }

  private attachKeys(scroll: HTMLElement): void {
    scroll.addEventListener("keydown", (e) => this.onShelfKey(e));
  }

  /** Room-wide find keys (§8): / and Cmd/Ctrl+F focus search from
   * anywhere in the room — never while he's typing in a field. */
  private onRoomKey(e: KeyboardEvent): void {
    const t = e.target as HTMLElement;
    if (t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement) return;
    if (e.key === "/" || ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "f")) {
      e.preventDefault();
      this.searchInput.focus();
      this.searchInput.select();
    }
  }

  private visibleOrder(): RoomGame[] {
    return this.visibleGames();
  }

  private focusGame(key: string): void {
    const node = this.shelfScroll?.querySelector<HTMLElement>(`[data-key="${CSS.escape(key)}"]`);
    if (node) {
      node.focus({ preventScroll: false });
      node.scrollIntoView({ block: "nearest" });
    }
  }

  private onShelfKey(e: KeyboardEvent): void {
    const ordered = this.visibleOrder();
    if (ordered.length === 0) return;
    const currentKey =
      this.selection.size === 1 ? [...this.selection][0] : (this.anchorKey ?? ordered[0].key);
    const idx = Math.max(
      0,
      ordered.findIndex((g) => g.key === currentKey),
    );
    const cols = this.view === "grid" ? this.gridColumns() : 1;
    let next = idx;
    switch (e.key) {
      case "ArrowRight":
        next = Math.min(ordered.length - 1, idx + 1);
        break;
      case "ArrowLeft":
        next = Math.max(0, idx - 1);
        break;
      case "ArrowDown":
        next = Math.min(ordered.length - 1, idx + cols);
        break;
      case "ArrowUp":
        next = Math.max(0, idx - cols);
        break;
      case "Home":
        next = 0;
        break;
      case "End":
        next = ordered.length - 1;
        break;
      case "Enter":
        if (!ordered[idx].offline) void this.playGame(ordered[idx]);
        e.preventDefault();
        return;
      case " ":
        this.hooks.onPreviewItem(ordered[idx].item);
        e.preventDefault();
        return;
      case "Escape":
        if (this.selection.size > 0) {
          this.selection.clear();
          this.renderInspector();
          this.syncSelectionDom();
        } else {
          this.hooks.onExit();
        }
        e.preventDefault();
        return;
      case "/":
        this.searchInput.focus();
        e.preventDefault();
        return;
      default:
        return;
    }
    if (next !== idx || e.key.startsWith("Arrow")) {
      e.preventDefault();
      const g = ordered[next];
      if (e.shiftKey) this.selection.add(g.key);
      else if (!e.metaKey && !e.ctrlKey) this.selection = new Set([g.key]);
      this.anchorKey = g.key;
      this.syncSelectionDom();
      this.renderInspector();
      this.focusGame(g.key);
    }
  }

  private gridColumns(): number {
    const grid = this.shelfScroll?.querySelector(".rh-grid");
    if (!grid) return 1;
    const style = getComputedStyle(grid);
    const cols = style.gridTemplateColumns.split(" ").filter(Boolean).length;
    return Math.max(1, cols);
  }

  // ------------------------------------------------------------ actions

  private async playGame(g: RoomGame): Promise<void> {
    const pk = g.item.provider_key;
    this.hooks.onPlayItem(g.item);
    if (pk) {
      try {
        const st = await api.romHomeNotePlay(pk);
        g.state = st;
        this.renderInspector();
        // Recently Played's membership changed under us; the rail
        // count and (if we're standing on it) the shelf follow.
        this.renderRail();
      } catch {
        // the launch already happened; bookkeeping can lag honestly
      }
    }
  }

  private async setRating(g: RoomGame, rating: number | null): Promise<void> {
    const pk = g.item.provider_key;
    if (!pk) return;
    g.state.rating = rating;
    // The tile ★ mirrors the room's stars; the path star follows a
    // fresh rating (and clears when the rating does).
    try {
      await api.romHomeSetRating(pk, rating);
      const wantStar = rating !== null;
      if (g.starred !== wantStar) {
        g.starred = await api.toggleStar(g.item.id);
        g.item.starred = g.starred;
      }
    } catch (err) {
      Toast.error(`Could not save the rating: ${errText(err)}`);
    }
    this.renderShelf(true);
    this.renderInspector();
  }

  private async setFavorite(g: RoomGame, on: boolean): Promise<void> {
    const pk = g.item.provider_key;
    if (!pk) return;
    g.state.favorite = on;
    try {
      await api.romHomeSetFavorite(pk, on);
    } catch (err) {
      Toast.error(`Could not save: ${errText(err)}`);
    }
    this.renderRail();
    this.renderInspector();
  }

  private async setCompleted(g: RoomGame, on: boolean): Promise<void> {
    const pk = g.item.provider_key;
    if (!pk) return;
    g.state.completed = on;
    try {
      await api.romHomeSetCompleted(pk, on);
    } catch (err) {
      Toast.error(`Could not save: ${errText(err)}`);
    }
    this.renderShelf(true);
    this.renderInspector();
  }

  private async toggleLabel(g: RoomGame, label: string, on: boolean): Promise<void> {
    try {
      await api.setPathLabel(g.item.provider_key ?? g.item.id, label, on);
      g.labels = on
        ? [...new Set([...g.labels, label])]
        : g.labels.filter((l) => l !== label);
      g.item.labels = g.labels;
    } catch (err) {
      Toast.error(`Could not save the tag: ${errText(err)}`);
    }
    this.renderInspector();
  }

  /** Fetch one game's missing types (the chips' job), or one type. */
  private fetchForGames(games: RoomGame[], onlyType?: string): void {
    const wanted = games.filter((g) => g.source === "dat" && !g.offline);
    const retry: { itemId: string; mediaType: "boxart" | "screenshot" | "titleshot" | "trailer" }[] = [];
    for (const g of wanted) {
      for (const t of MEDIA_TYPES) {
        if (onlyType && t !== onlyType) continue;
        if (!g.media.has(t)) retry.push({ itemId: g.item.id, mediaType: t });
      }
    }
    void this.startFetch(
      retry,
      `Fetching ${retry.length} missing ${retry.length === 1 ? "file" : "files"} — watch it in the jobs panel.`,
    );
  }

  private async addToList(games: RoomGame[], collectionId: string): Promise<void> {
    let added = 0;
    try {
      await api.addToCollection(
        collectionId,
        games.map((g) => g.item.id),
      );
      added = games.length;
    } catch {
      added = 0;
    }
    const cols = await api.listCollections().catch(() => this.collections);
    this.collections = cols;
    this.listMemberKeysCache.delete(collectionId);
    this.renderRail();
    Toast.success(`Added ${added} game${added === 1 ? "" : "s"} to the list.`);
  }

  // ------------------------------------------------------------ game menu

  private openGameMenu(g: RoomGame, x: number, y: number): void {
    const multi = this.selection.size > 1 && this.selection.has(g.key);
    const targets = multi
      ? [...this.selection].map((k) => this.gamesByKey.get(k)).filter((v): v is RoomGame => !!v)
      : [g];
    const items: MenuItem[] = [];
    if (!multi) {
      items.push({
        label: "▶ Play",
        disabled: g.offline,
        action: () => void this.playGame(g),
      });
      items.push({
        label: "Play with…",
        disabled: g.offline,
        action: () => this.hooks.onOpenWith(g.item),
      });
      items.push({ label: "", separator: true, action: () => undefined });
    }
    items.push({
      label: multi ? `Get missing media (${targets.length} games)` : "Get missing media",
      action: () => this.fetchForGames(targets),
    });
    const lists = this.collections.filter((c) => c.kind !== "smart");
    if (lists.length > 0) {
      items.push({ label: "", separator: true, action: () => undefined });
      for (const c of lists) {
        items.push({
          label: `Add to “${c.name}”`,
          action: () => void this.addToList(targets, c.id),
        });
      }
    }
    if (!multi) {
      items.push({ label: "", separator: true, action: () => undefined });
      items.push({
        label: "Show in Files",
        action: () => this.hooks.onRevealItem(g.item),
      });
      items.push({
        label: "Quick Look",
        action: () => this.hooks.onPreviewItem(g.item),
      });
    }
    this.ctxMenu.open(x, y, items);
  }

  // ------------------------------------------------------------ toolbar menus

  private openSortMenu(anchor: HTMLElement): void {
    const r = anchor.getBoundingClientRect();
    const keys: SortKey[] = ["title", "year", "recent-played", "most-played", "recent-added"];
    this.ctxMenu.open(
      r.left,
      r.bottom + 4,
      keys.map((k) => ({
        label: SORT_LABEL[k],
        checked: this.sort === k,
        action: () => {
          this.sort = k;
          this.saveState();
          this.renderShelf(true);
        },
      })),
    );
  }

  private openMoreMenu(anchor: HTMLElement): void {
    const r = anchor.getBoundingClientRect();
    this.ctxMenu.open(r.right - 220, r.bottom + 4, [
      {
        label: this.tv ? "Leave TV arrangement" : "TV arrangement",
        action: () => {
          this.tv = !this.tv;
          this.el.classList.toggle("rh-tv", this.tv);
          this.applyArtSize();
          this.saveState();
          if (this.tv) this.startGamepad();
          else this.stopGamepad();
          this.renderShelf(true);
        },
      },
      {
        label: this.grouping === "generation" ? "Group systems by maker" : "Group systems by generation",
        action: () => {
          this.grouping = this.grouping === "generation" ? "maker" : "generation";
          this.saveState();
          this.renderRail();
        },
      },
      { label: "", separator: true, action: () => undefined },
      {
        label: "Add a ROM folder…",
        action: () => this.hooks.onAddRomFolder(),
      },
      {
        label: "Get missing media (everything)",
        action: () => {
          const all = this.games.filter((g) => g.source === "dat");
          this.fetchForGames(all);
        },
      },
    ]);
  }

  /** The Filters popover: five plain-language groups as chips,
   * rendered in a small floating card (the app's picker language). */
  private openFiltersMenu(anchor: HTMLElement): void {
    document.querySelector(".rh-filter-pop")?.remove();
    const pop = el("div", "rh-filter-pop");
    const r = anchor.getBoundingClientRect();
    pop.style.left = `${Math.max(8, r.left)}px`;
    pop.style.top = `${r.bottom + 6}px`;
    const genreSet = new Map<string, number>();
    for (const g of this.games) {
      if (g.state.genre) genreSet.set(g.state.genre, (genreSet.get(g.state.genre) ?? 0) + 1);
    }
    const datKeys: [string, string][] = this.systems
      .filter((s) => s.key)
      .map((s) => [s.key as string, s.name]);
    const groups: { title: string; group: keyof RoomFilters; options: [string, string][] }[] = [
      { title: "System", group: "systems", options: datKeys },
      {
        title: "Genre",
        group: "genres",
        options: [...genreSet.entries()]
          .sort((a, b) => b[1] - a[1])
          .map(([name]): [string, string] => [name, name]),
      },
      {
        title: "Players",
        group: "players",
        options: [
          ["1", "1 player"],
          ["2", "2 players"],
          ["3-4", "3–4 players"],
          ["5+", "5+ players"],
        ],
      },
      {
        title: "Media",
        group: "media",
        options: [
          ["has-art", "Has box art"],
          ["missing-art", "Missing box art"],
          ["has-video", "Has video"],
        ],
      },
      {
        title: "Flags",
        group: "flags",
        options: [
          ["starred", "Starred"],
          ["favorites", "Favorites"],
          ["completed", "Completed"],
          ["unplayed", "Not yet played"],
        ],
      },
    ];
    for (const grp of groups) {
      if (grp.options.length === 0) continue;
      pop.appendChild(el("div", "rh-filter-head", grp.title));
      const row = el("div", "rh-filter-row");
      for (const [value, label] of grp.options) {
        const on = (this.filters[grp.group] as string[]).includes(value);
        const chip = el("button", on ? "rh-fchip on" : "rh-fchip", label) as HTMLButtonElement;
        chip.setAttribute("aria-pressed", String(on));
        chip.addEventListener("click", () => {
          const arr = this.filters[grp.group] as string[];
          const i = arr.indexOf(value);
          if (i >= 0) arr.splice(i, 1);
          else arr.push(value);
          chip.classList.toggle("on");
          chip.setAttribute("aria-pressed", String(i < 0));
          this.afterFilterChange();
        });
        row.appendChild(chip);
      }
      pop.appendChild(row);
    }
    const close = el("button", "pill-btn rh-filter-done", "Done") as HTMLButtonElement;
    close.addEventListener("click", () => pop.remove());
    pop.appendChild(close);
    document.body.appendChild(pop);
    const dismiss = (e: PointerEvent): void => {
      if (!pop.contains(e.target as Node)) {
        pop.remove();
        document.removeEventListener("pointerdown", dismiss);
      }
    };
    document.addEventListener("pointerdown", dismiss);
  }

  // ------------------------------------------------------------ inspector

  private renderInspector(): void {
    const insp = this.inspectorEl;
    const chosen = [...this.selection]
      .map((k) => this.gamesByKey.get(k))
      .filter((v): v is RoomGame => !!v);
    if (chosen.length === 0) {
      if (!insp.hidden) {
        insp.hidden = true;
        insp.innerHTML = "";
      }
      return;
    }
    const wasHidden = insp.hidden;
    insp.hidden = false;
    insp.innerHTML = "";
    if (chosen.length > 1) this.buildMultiInspector(insp, chosen);
    else this.buildInspector(insp, chosen[0]);
    if (wasHidden && !reducedMotion()) {
      // Spring in (Apple: interruptible, retargetable).
      springTo(
        "rh-inspector-x",
        0,
        (v) => {
          insp.style.transform = `translateX(${v}px)`;
          insp.style.opacity = String(1 - v / 48);
        },
        { from: 24 },
      );
    } else {
      insp.style.transform = "";
      insp.style.opacity = "";
    }
  }

  private buildMultiInspector(insp: HTMLElement, games: RoomGame[]): void {
    insp.appendChild(el("h3", "rh-insp-count", `${games.length} games selected`));
    const systems = new Set(games.map((g) => g.systemName));
    insp.append(
      el(
        "p",
        "rh-insp-sub",
        systems.size === 1 ? [...systems][0] : `${systems.size} systems`,
      ),
    );
    const starRow = el("div", "rh-insp-row");
    for (let n = 1; n <= 5; n++) {
      const b = el("button", "rh-bulk-star", `${n}★`) as HTMLButtonElement;
      b.addEventListener("click", () => {
        for (const g of games) void this.setRating(g, n);
      });
      starRow.appendChild(b);
    }
    insp.append(el("span", "rh-insp-lab", "Rate all"), starRow);
    const fav = el("button", "pill-btn rh-insp-btn", "♥ Favorite all") as HTMLButtonElement;
    fav.addEventListener("click", () => {
      for (const g of games) void this.setFavorite(g, true);
    });
    const done = el("button", "pill-btn rh-insp-btn", "✓ Completed all") as HTMLButtonElement;
    done.addEventListener("click", () => {
      for (const g of games) void this.setCompleted(g, true);
    });
    insp.append(fav, done);
    const missing = games.filter((g) => g.source === "dat" && !g.media.has("boxart")).length;
    if (missing > 0) {
      const get = el(
        "button",
        "pill-btn primary rh-insp-btn",
        `Get missing media (${missing})`,
      ) as HTMLButtonElement;
      get.addEventListener("click", () => this.fetchForGames(games));
      insp.appendChild(get);
    }
    insp.appendChild(el("span", "rh-insp-lab", "Tags"));
    {
      const row = el("div", "rh-tags");
      for (const d of loadLabelDefs()) {
        const allHave = games.every((g) => g.labels.includes(d.name));
        const chip = el("button", allHave ? "rh-fchip on" : "rh-fchip", d.name) as HTMLButtonElement;
        chip.style.setProperty("--chip-color", d.color);
        chip.addEventListener("click", () => {
          const want = !games.every((g) => g.labels.includes(d.name));
          for (const g of games) {
            if (g.labels.includes(d.name) !== want) void this.toggleLabel(g, d.name, want);
          }
        });
        row.appendChild(chip);
      }
      insp.appendChild(row);
    }
    const lists = this.collections.filter((c) => c.kind !== "smart");
    if (lists.length > 0) {
      insp.appendChild(el("span", "rh-insp-lab", "Add all to"));
      for (const c of lists) {
        const b = el("button", "pill-btn rh-insp-btn", c.name) as HTMLButtonElement;
        b.addEventListener("click", () => void this.addToList(games, c.id));
        insp.appendChild(b);
      }
    }
  }

  private buildInspector(insp: HTMLElement, g: RoomGame): void {
    // ---- Hero: trailer (hover) or the best still.
    const hero = el("div", "rh-hero");
    const stillPath = g.media.get("screenshot") ?? g.media.get("titleshot") ?? g.media.get("boxart");
    if (stillPath) {
      const img = document.createElement("img");
      img.className = "rh-hero-img";
      img.alt = "";
      img.src = fileSrc(stillPath);
      img.addEventListener("error", () => {
        img.remove();
        hero.appendChild(this.placeholderEl(g));
      });
      hero.appendChild(img);
    } else {
      hero.appendChild(this.placeholderEl(g));
    }
    const trailerPath = g.media.get("trailer");
    if (trailerPath && this.trailersEnabled && !g.offline) {
      let video: HTMLVideoElement | null = null;
      hero.addEventListener("pointerenter", () => {
        if (video) return;
        video = document.createElement("video");
        video.className = "rh-hero-video";
        video.muted = true;
        video.loop = true;
        video.playsInline = true;
        void api
          .mediaUrlForPath(trailerPath)
          .then((url) => {
            if (video) {
              video.src = url;
              void video.play().catch(() => undefined);
            }
          })
          .catch(() => undefined);
        hero.appendChild(video);
      });
      hero.addEventListener("pointerleave", () => {
        video?.remove();
        video = null;
      });
    }
    insp.appendChild(hero);

    // ---- Title + system line + edit pencil.
    const titleRow = el("div", "rh-insp-titlerow");
    const h = el("h3", "rh-insp-title", g.title);
    const edit = el("button", "icon-btn rh-edit-btn", "✎") as HTMLButtonElement;
    edit.title = "Edit details";
    edit.addEventListener("click", () => this.openEditSheet(g));
    titleRow.append(h, edit);
    insp.appendChild(titleRow);
    insp.appendChild(el("p", "rh-insp-sub", g.systemName));

    // ---- Play: ONE primary action.
    const playRow = el("div", "rh-play-row");
    const play = el("button", "pill-btn primary rh-play", "▶ Play") as HTMLButtonElement;
    if (g.offline) {
      play.disabled = true;
      play.title = `On ${g.volumeLabel ?? "a disconnected volume"}`;
    }
    play.addEventListener("click", () => void this.playGame(g));
    const withBtn = el("button", "pill-btn rh-playwith", "Play with…") as HTMLButtonElement;
    if (g.offline) withBtn.disabled = true;
    withBtn.addEventListener("click", () => this.hooks.onOpenWith(g.item));
    playRow.append(play, withBtn);
    insp.appendChild(playRow);
    if (g.offline) {
      insp.appendChild(
        el(
          "p",
          "rh-offline-note",
          `On ${g.volumeLabel ?? "a disconnected volume"} · last seen ${fmtDay(g.volumeLastSeen)} — reconnect it to play.`,
        ),
      );
    }

    // ---- Your marks: stars, favorite, completed.
    const marks = el("div", "rh-marks");
    const stars = el("div", "rh-stars");
    stars.setAttribute("role", "radiogroup");
    stars.setAttribute("aria-label", "Your rating");
    for (let n = 1; n <= 5; n++) {
      const on = (g.state.rating ?? 0) >= n;
      const b = el("button", on ? "rh-star on" : "rh-star", "★") as HTMLButtonElement;
      b.setAttribute("aria-label", `${n} star${n === 1 ? "" : "s"}`);
      b.setAttribute("aria-checked", String(g.state.rating === n));
      b.addEventListener("click", () => {
        void this.setRating(g, g.state.rating === n ? null : n);
      });
      stars.appendChild(b);
    }
    marks.appendChild(stars);
    const fav = el(
      "button",
      g.state.favorite ? "rh-mark on" : "rh-mark",
      g.state.favorite ? "♥" : "♡",
    ) as HTMLButtonElement;
    fav.title = "Favorite";
    fav.setAttribute("aria-pressed", String(g.state.favorite));
    fav.addEventListener("click", () => void this.setFavorite(g, !g.state.favorite));
    const done = el("button", g.state.completed ? "rh-mark on" : "rh-mark", "✓") as HTMLButtonElement;
    done.title = "Completed";
    done.setAttribute("aria-pressed", String(g.state.completed));
    done.addEventListener("click", () => void this.setCompleted(g, !g.state.completed));
    marks.append(fav, done);
    insp.appendChild(marks);
    if (g.state.playCount > 0) {
      insp.appendChild(
        el(
          "p",
          "rh-played-line",
          `Played ${g.state.playCount}× · last ${fmtDay(g.state.lastPlayed)}`,
        ),
      );
    }

    // ---- Tags (the app's labels, same store as Files).
    insp.appendChild(el("span", "rh-insp-lab", "Tags"));
    {
      const defs = loadLabelDefs();
      const row = el("div", "rh-tags");
      for (const d of defs) {
        const on = g.labels.includes(d.name);
        const chip = el("button", on ? "rh-fchip on" : "rh-fchip", d.name) as HTMLButtonElement;
        chip.style.setProperty("--chip-color", d.color);
        chip.addEventListener("click", () => void this.toggleLabel(g, d.name, !on));
        row.appendChild(chip);
      }
      const lab = insp.querySelectorAll(".rh-insp-lab");
      const tagsLab = lab[lab.length - 1];
      tagsLab?.after(row);
    }

    // ---- Facts: only what the game's sources actually say —
    // an unknown fact is ABSENT, never the word "Unknown".
    const facts: [string, string][] = [];
    if (g.year !== null && g.year !== undefined) facts.push(["Year", String(g.year)]);
    if (g.state.genre) facts.push(["Genre", g.state.genre]);
    if (g.state.players) facts.push(["Players", g.state.players]);
    if (g.region) facts.push(["Region", g.region]);
    if (facts.length > 0) {
      insp.appendChild(el("span", "rh-insp-lab", "Facts"));
      const dl = el("dl", "rh-facts");
      for (const [k, v] of facts) {
        const row = el("div", "rh-fact");
        row.append(el("dt", "", k), el("dd", "", v));
        dl.appendChild(row);
      }
      insp.appendChild(dl);
    }
    if (g.state.notes) {
      insp.appendChild(el("span", "rh-insp-lab", "Notes"));
      insp.appendChild(el("p", "rh-notes", g.state.notes));
    }

    // ---- Media: the chips ARE the fetch buttons.
    if (g.source === "dat") {
      insp.appendChild(el("span", "rh-insp-lab", "Media"));
      const chips = el("div", "rh-media-chips");
      for (const t of MEDIA_TYPES) {
        const have = g.media.get(t);
        const chip = el(
          "button",
          have ? "rh-mchip have" : "rh-mchip want",
          `${MEDIA_LABEL[t]} ${have ? "✓" : "＋"}`,
        ) as HTMLButtonElement;
        chip.title = have
          ? `View ${MEDIA_LABEL[t].toLowerCase()}`
          : `Fetch ${MEDIA_LABEL[t].toLowerCase()} (Get missing media)`;
        chip.disabled = g.offline && !have;
        chip.addEventListener("click", () => {
          if (have) this.openLightbox(g, t, have);
          else this.fetchForGames([g], t);
        });
        chips.appendChild(chip);
      }
      insp.appendChild(chips);
    }

    // ---- The file itself, quietly.
    insp.appendChild(el("span", "rh-insp-lab", "File"));
    const fileBox = el("div", "rh-filebox");
    const lines: [string, string][] = [];
    if (g.source === "dat") {
      lines.push(["DAT name", g.datTitle]);
      if (g.sha1) lines.push(["Hash", `${g.sha1.slice(0, 12)}… ✓ verified`]);
    }
    lines.push(["File", g.item.name]);
    if (g.item.size_bytes) lines.push(["Size", formatBytes(g.item.size_bytes)]);
    lines.push([
      "Volume",
      g.volumeLabel
        ? `${g.volumeLabel}${g.offline ? ` · offline · last seen ${fmtDay(g.volumeLastSeen)}` : ""}`
        : "—",
    ]);
    lines.push(["Presence", g.offline ? "Offline" : "Here"]);
    for (const [k, v] of lines) {
      const row = el("div", "rh-fileline");
      const kk = el("span", "rh-filekey", k);
      const vv = el("span", "rh-fileval", v);
      vv.title = v;
      row.append(kk, vv);
      fileBox.appendChild(row);
    }
    const fileActions = el("div", "rh-file-actions");
    const show = el("button", "pill-btn rh-insp-btn", "Show in Files") as HTMLButtonElement;
    show.addEventListener("click", () => this.hooks.onRevealItem(g.item));
    fileActions.appendChild(show);
    if (g.auditorFlagged) {
      const aud = el("button", "pill-btn rh-insp-btn", "See in File Auditor") as HTMLButtonElement;
      aud.addEventListener("click", () => this.hooks.onOpenAuditor());
      fileActions.appendChild(aud);
    }
    fileBox.appendChild(fileActions);
    insp.appendChild(fileBox);
  }

  /** A still (or the trailer) at full size — media is the point. */
  private openLightbox(g: RoomGame, type: string, path: string): void {
    document.querySelector(".rh-lightbox")?.remove();
    const box = el("div", "rh-lightbox");
    box.setAttribute("role", "dialog");
    box.setAttribute("aria-label", `${g.title} — ${MEDIA_LABEL[type]}`);
    if (type === "trailer") {
      const v = document.createElement("video");
      v.className = "rh-lightbox-media";
      v.controls = true;
      v.autoplay = true;
      void api
        .mediaUrlForPath(path)
        .then((url) => (v.src = url))
        .catch(() => undefined);
      box.appendChild(v);
    } else {
      const img = document.createElement("img");
      img.className = "rh-lightbox-media";
      img.alt = g.title;
      img.src = fileSrc(path);
      box.appendChild(img);
    }
    const close = (): void => {
      box.remove();
      document.removeEventListener("keydown", onKey);
    };
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === "Escape") close();
    };
    box.addEventListener("click", (e) => {
      if (e.target === box) close();
    });
    document.addEventListener("keydown", onKey);
    document.body.appendChild(box);
  }

  /** The ✎ sheet: correct the title, supply genre/players by hand,
   * keep notes. Labels ride the app's tag store (chips above). */
  private openEditSheet(g: RoomGame): void {
    document.querySelector(".rh-edit-sheet")?.remove();
    const sheet = el("div", "rh-edit-sheet");
    sheet.setAttribute("role", "dialog");
    sheet.setAttribute("aria-label", `Edit ${g.title}`);
    sheet.appendChild(el("h3", "", "Edit details"));
    const field = (label: string, value: string, placeholder: string): HTMLInputElement => {
      const wrap = el("label", "rh-edit-field");
      wrap.appendChild(el("span", "", label));
      const input = el("input", "rh-edit-in") as HTMLInputElement;
      input.value = value;
      input.placeholder = placeholder;
      wrap.appendChild(input);
      sheet.appendChild(wrap);
      return input;
    };
    const titleIn = field("Title", g.state.titleOverride ?? "", g.datTitle);
    const genreIn = field("Genre", g.state.genre ?? "", "e.g. Platformer");
    const playersIn = field("Players", g.state.players ?? "", "e.g. 2 or 1-4");
    const notesWrap = el("label", "rh-edit-field");
    notesWrap.appendChild(el("span", "", "Notes"));
    const notesIn = el("textarea", "rh-edit-in rh-edit-notes") as HTMLTextAreaElement;
    notesIn.value = g.state.notes ?? "";
    notesWrap.appendChild(notesIn);
    sheet.appendChild(notesWrap);
    const actions = el("div", "rh-edit-actions");
    const cancel = el("button", "pill-btn", "Cancel") as HTMLButtonElement;
    cancel.addEventListener("click", () => sheet.remove());
    const save = el("button", "pill-btn primary", "Save") as HTMLButtonElement;
    save.addEventListener("click", () => {
      void (async () => {
        const pk = g.item.provider_key;
        if (pk) {
          try {
            await api.romHomeSetOverrides(
              pk,
              titleIn.value.trim() || null,
              genreIn.value.trim() || null,
              playersIn.value.trim() || null,
              notesIn.value.trim() || null,
            );
            g.state = {
              ...g.state,
              titleOverride: titleIn.value.trim() || null,
              genre: genreIn.value.trim() || null,
              players: playersIn.value.trim() || null,
              notes: notesIn.value.trim() || null,
            };
            g.title = g.state.titleOverride ?? g.datTitle;
          } catch (err) {
            Toast.error(`Could not save: ${errText(err)}`);
            return;
          }
        }
        sheet.remove();
        this.renderShelf(true);
        this.renderInspector();
      })();
    });
    actions.append(cancel, save);
    sheet.appendChild(actions);
    this.inspectorEl.appendChild(sheet);
    titleIn.focus();
    titleIn.select();
  }

  // ------------------------------------------------------------ gamepad

  private startGamepad(): void {
    this.stopGamepad();
    let lastNav = 0;
    const tick = (): void => {
      const pads = navigator.getGamepads?.() ?? [];
      const pad = [...pads].find((p) => p && p.connected);
      if (pad) {
        const now = performance.now();
        const axisX = pad.axes[0] ?? 0;
        const axisY = pad.axes[1] ?? 0;
        const btn = (i: number): boolean => pad.buttons[i]?.pressed ?? false;
        const dir =
          btn(15) || axisX > 0.6
            ? "ArrowRight"
            : btn(14) || axisX < -0.6
              ? "ArrowLeft"
              : btn(13) || axisY > 0.6
                ? "ArrowDown"
                : btn(12) || axisY < -0.6
                  ? "ArrowUp"
                  : null;
        if (dir && now - lastNav > 180) {
          lastNav = now;
          this.onShelfKey(new KeyboardEvent("keydown", { key: dir }));
        } else if (btn(0)) {
          // A = Enter on the focused game
          const ordered = this.visibleOrder();
          const sel = this.selection.size === 1 ? [...this.selection][0] : null;
          const g = ordered.find((x) => x.key === sel) ?? ordered[0];
          if (g && now - lastNav > 400) {
            lastNav = now;
            if (!g.offline) void this.playGame(g);
          }
        } else if (btn(1) && now - lastNav > 400) {
          lastNav = now;
          this.hooks.onExit();
        }
      }
      this.gamepadTimer = window.setTimeout(tick, 50);
    };
    tick();
  }

  private stopGamepad(): void {
    window.clearTimeout(this.gamepadTimer);
    this.gamepadTimer = 0;
  }
}
