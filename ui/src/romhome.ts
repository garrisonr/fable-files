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
  type RomHomeCounts,
  type RomHomeShelfSpec,
  type RomCompleteness,
  type RomMissingList,
  type RomSystemSummary,
  type RomNeedsIdEntry,
  type RomVariantInfo,
  type RomSmartList,
  type PcGameCard,
  type PcGameSheet,
  type EsArtSheet,
} from "./api";
import {
  loadRomSystems,
  romSystemKey,
  soleClaimExtensions,
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
import { Toast, promptDialog, confirmDialog, errText } from "./toast";
import { openGuideLink } from "./guidelinks";
import { fmtDay } from "./search";
import { springTo, reducedMotion } from "./motion";
import { loadLabelDefs } from "./labels";
import { showEmuSheet, type EmuSheetResult } from "./emu";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface RomHomeHooks {
  /** Leave the room; the shell restores the Files view as it was. */
  onExit(): void;
  /** Play's fallback launch path (openItem) — used when a game
   * has no emulator resolution or the sheet is declined. */
  onPlayItem(item: Item): void;
  /** Space: Quick Look peek. */
  onPreviewItem(item: Item): void;
  /** "Show in Files": navigate the Files view to the item. */
  onRevealItem(item: Item): void;
  /** The OS-default open — reached from inside the "Play with…"
   * picker ("Open with the system default app…") and as the sheet's
   * "Not now" fallback: declining emulation keeps today's behavior. */
  onOpenWith(item: Item): void;
  /** "See in File Auditor". */
  onOpenAuditor(): void;
  /** "Add a game folder…" — the shell's add-location flow. */
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
  | { kind: "needsid" }
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
  /** Phase 3 grouping (DAT games): the group this game folds into
   * and its parsed variant descriptor. */
  groupKey: string | null;
  variant: RomVariantInfo | null;
  crc: string | null;
  /** Set on a group's shelf representative by the collapse:
   * every version of the game, default first. Copies of a version
   * ride inside it and never count as versions. */
  versions?: VersionView[];
  /** Group play totals (sheet display): sums across versions. */
  playTotal?: number;
  playLast?: string | null;
  /** PC Games P1: card facts for pc-game rows (null for ROMs). */
  pc: PcGameCard | null;
}

/** One version of a grouped game (Phase 3): a distinct dump /
 * region / revision / status — with its copies folded inside. */
export interface VersionView {
  descriptor: string;
  variant: RomVariantInfo | null;
  /** All items holding this version (copies — folded silently). */
  games: RoomGame[];
  /** The copy Play reaches for: online first, then has-art. */
  rep: RoomGame;
  isDefault: boolean;
}

/** The tag-stripped base title of a DAT name — the group display
 * form. Mirrors romdat::parsed_base_title (groups removed, ", The"
 * undone); the backend computes keys from the same rule. */
export function baseTitle(raw: string): string {
  let out = "";
  let dp = 0;
  let db = 0;
  for (const ch of raw) {
    if (ch === "(") dp++;
    else if (ch === ")") dp = Math.max(0, dp - 1);
    else if (ch === "[") db++;
    else if (ch === "]") db = Math.max(0, db - 1);
    else if (dp === 0 && db === 0) out += ch;
  }
  let s = out.split(/\s+/).filter(Boolean).join(" ");
  s = s.replace(/^[ ,-]+/, "").replace(/[ ,-]+$/, "");
  if (s.toLowerCase().endsWith(", the")) s = `The ${s.slice(0, -5)}`;
  return s || raw;
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
/** Groups per shelf window (Hyperbase #4) — one window paints
 * immediately; scrolling chains the next by cursor. */
const WINDOW_GROUPS = 240;
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
  /** Hyperbase #4: the loaded windows of the CURRENT shelf —
   * DAT member rows only, whole groups, in Rust window order.
   * The full set never crosses the wire; the catalog judges
   * shelves/filters/sorts and ships bounded windows. */
  private datMembers: RoomGame[] = [];
  /** Extension-rule games (they arrive whole per system, as
   * they always have — the rule store is their backend). */
  private extGames: RoomGame[] = [];
  private gamesByKey = new Map<string, RoomGame>();
  private systems: RoomSystem[] = [];
  private counts = { total: 0, missingMedia: 0, favoriteGroups: 0, recentGroups: 0 };
  /** PC Games P1: [library name, group count] for the PC
   * section's sub-shelf chips. */
  private pcLibraries: Array<[string, number]> = [];
  /** The PC library sub-shelf in force (null = all PC games). */
  private pcLibrary: string | null = null;
  private perSystemTallies = new Map<string, [number, number]>();
  private countsExtra = new Map<
    string,
    { sizeBytes: number; allOffline: boolean; volumeLabel: string | null; volumeLastSeen: string | null }
  >();
  private genreFacets: Array<[string, number]> = [];
  // Shelf window chain state (Hyperbase #4).
  private shelfTotal = 0;
  private shelfCursor: string | null = null;
  private shelfExhausted = true;
  private shelfLoading = false;
  private shelfError = false;
  private shelfSpecKey = "";
  private shelfGen = 0;
  /** While a "show all" chain runs, windows widen (2,000 groups)
   * so loading a whole shelf is a handful of fat windows, not
   * hundreds of thin ones. Cursors are size-agnostic. */
  private bulkLimit = 0;
  private fillSeq = 0;
  private bodyGen = 0;
  /** Needs-ID shelf contents (Phase 2): unidentified candidates in
   * ROM territory, pre-grouped by parsed filename on the backend. */
  private needsEntries: RomNeedsIdEntry[] = [];
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
  private headSubEl: HTMLElement | null = null;
  private headActionsEl: HTMLElement | null = null;
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
    const title = el("span", "rh-title", "Games");
    const searchWrap = el("div", "rh-search");
    this.searchInput = el("input", "rh-search-in") as HTMLInputElement;
    this.searchInput.type = "search";
    this.searchInput.placeholder = "Search all games";
    this.searchInput.setAttribute("aria-label", "Search all games");
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
    await this.ensureShelfWindow();
    this.selection = new Set([...keep].filter((k) => this.gamesByKey.has(k)));
    if (this.active) {
      this.renderRail();
      this.renderShelf(true);
      this.renderInspector();
    }
  }

  private async loadAll(): Promise<void> {
    const seq = ++this.loadSeq;
    const [counts, collections, smartLists, settings, needs, facets] =
      await Promise.all([
        api.romHomeCounts().catch(() => null),
        api.listCollections().catch(() => []),
        api.romHomeSmartLists().catch(() => []),
        api.getRomMediaSettings().catch(() => null),
        api.romHomeNeedsId().catch(() => null),
        api.romHomeGenreFacets().catch(() => null),
      ]);
    if (seq !== this.loadSeq) return;
    if (needs) this.needsEntries = needs;
    if (counts) this.applyCounts(counts);
    if (facets) this.genreFacets = facets;
    this.collections = collections;
    this.smartLists = smartLists;
    this.trailersEnabled = settings?.settings.trailers.enabled ?? false;
    this.rebuildSystems();
    // The set may have moved under the current shelf: its window
    // chain restarts from the fresh tallies (Hyperbase #4).
    this.shelfGen++;
    this.shelfSpecKey = "";
    this.datMembers = [];
    this.rebuildIndex();
    this.loaded = true;
  }

  /** Tallies from the catalog (Hyperbase #4: group counts for
   * every rail shelf + per-system size/offline extras ride the
   * same call, so the room never needs the full set to know its
   * numbers). */
  private applyCounts(counts: RomHomeCounts): void {
    this.counts = {
      total: counts.total,
      missingMedia: counts.missingMedia,
      favoriteGroups: counts.favoriteGroups,
      recentGroups: counts.recentGroups,
    };
    this.pcLibraries = counts.pcLibraries ?? [];
    if (
      this.pcLibrary &&
      !this.pcLibraries.some(([name]) => name === this.pcLibrary)
    ) {
      this.pcLibrary = null;
    }
    this.perSystemTallies = new Map(
      counts.perSystem.map(([p, g, m]) => [p, [g, m] as [number, number]]),
    );
    this.countsExtra = new Map(
      counts.perSystemExtra.map((e) => [
        e.platform,
        {
          sizeBytes: e.sizeBytes,
          allOffline: e.allOffline,
          volumeLabel: e.volumeLabel,
          volumeLastSeen: e.volumeLastSeen,
        },
      ]),
    );
  }

  private rebuildSystems(): void {
    this.systems = loadRomSystems().map((sys) => {
      const key = romSystemKey(sys);
      const tally = key ? this.perSystemTallies.get(key) : undefined;
      const extra = key ? this.countsExtra.get(key) : undefined;
      return {
        sys,
        name: sys.name,
        key,
        placeholder: isRomPlaceholder(sys),
        count: key ? (tally?.[0] ?? 0) : null,
        missing: key ? (tally?.[1] ?? 0) : 0,
        sizeBytes: extra?.sizeBytes ?? 0,
      };
    });
    // Extension systems keep the counts their own loads set.
    for (const s of this.systems) {
      if (s.key === null && this.extLoaded.has(s.name)) {
        const mine = this.extGames.filter((g) => g.systemName === s.name);
        s.count = mine.length;
        s.sizeBytes = mine.reduce((a, g) => a + (g.item.size_bytes ?? 0), 0);
      }
    }
  }

  /** Quiet tally refresh after a state write (a favorite lands,
   * a play is noted): the rail's numbers re-read the catalog —
   * never the loaded windows, which are a sample by design. */
  private async refreshTallies(): Promise<void> {
    const [counts, facets] = await Promise.all([
      api.romHomeCounts().catch(() => null),
      api.romHomeGenreFacets().catch(() => null),
    ]);
    if (counts) {
      this.applyCounts(counts);
      this.rebuildSystems();
    }
    if (facets) this.genreFacets = facets;
    if (this.active) this.renderRail();
  }

  /** Every loaded game (window members + extension games) —
   * the set the inspector, selection and group fan-out resolve
   * against. Shelves judge the FULL set in Rust; this is only
   * what has been shipped to the room so far. */
  private allLoaded(): RoomGame[] {
    return [...this.datMembers, ...this.extGames];
  }

  private rebuildIndex(): void {
    this.gamesByKey = new Map(this.allLoaded().map((g) => [g.key, g]));
  }

  // ------------------------------------------------------------ shelf windows (Hyperbase #4)

  /** The window spec for the current shelf: the room's shelf,
   * filters and sort, in the catalog's vocabulary. Smart shelves
   * send their list's rule AS the filters (systems ignored, the
   * standing list rule); other shelves layer the toolbar filters
   * over membership. Null for shelves with no game windows
   * (Needs-ID, hardware). */
  private buildSpec(
    cursor: string | null,
    applyFilters = true,
  ): RomHomeShelfSpec | null {
    const ref = this.shelf;
    const spec: RomHomeShelfSpec = {
      kind: "all",
      platform: null,
      query: null,
      searchPlatforms: [],
      memberKeys: null,
      pcSource:
        ref.kind === "system" && ref.name === "PC" ? this.pcLibrary : null,
      filters: {
        systems: [],
        genres: [],
        players: [],
        media: [],
        flags: [],
        ignoreSystems: false,
      },
      sort: this.sort,
      cursor,
      limit: this.bulkLimit > 0 ? this.bulkLimit : WINDOW_GROUPS,
    };
    switch (ref.kind) {
      case "all":
        spec.kind = "all";
        break;
      case "favorites":
        spec.kind = "favorites";
        break;
      case "recent":
        spec.kind = "recent";
        break;
      case "missing":
        spec.kind = "missing";
        break;
      case "system":
        spec.kind = "system";
        spec.platform = this.systemKeyOf(ref.name);
        break;
      case "search": {
        spec.kind = "search";
        spec.query = ref.query;
        const q = ref.query.toLowerCase();
        for (const s of loadRomSystems()) {
          const key = romSystemKey(s);
          if (key && (s.name.toLowerCase().includes(q) || key.toLowerCase().includes(q))) {
            spec.searchPlatforms.push(key);
          }
        }
        break;
      }
      case "list":
        spec.kind = "list";
        spec.memberKeys = this.listMemberKeys(ref.id);
        break;
      case "smart": {
        spec.kind = "smart";
        const s = this.smartLists.find((x) => x.id === ref.id);
        if (!s) return null;
        try {
          const f = JSON.parse(s.queryJson) as RoomFilters;
          spec.filters = {
            systems: f.systems ?? [],
            genres: f.genres ?? [],
            players: f.players ?? [],
            media: f.media ?? [],
            flags: f.flags ?? [],
            ignoreSystems: true,
          };
        } catch {
          return null;
        }
        return spec;
      }
      default:
        return null;
    }
    if (applyFilters) {
      // (Smart shelves returned above with their rule as the
      // filters — the toolbar layer never applies to them.)
      spec.filters = {
        systems: [...this.filters.systems],
        genres: [...this.filters.genres],
        players: [...this.filters.players],
        media: [...this.filters.media],
        flags: [...this.filters.flags],
        ignoreSystems: false,
      };
    }
    return spec;
  }

  /** Change key for the window chain: shelf + filters + sort +
   * the refresh generation. When it moves, the chain restarts. */
  private specKey(): string {
    const spec = this.buildSpec(null);
    if (!spec) return `none|${this.shelfGen}`;
    const { cursor: _c, limit: _l, ...rest } = spec;
    return `${JSON.stringify(rest)}|${this.shelfGen}`;
  }

  /** Make sure the first window of the current spec is loaded
   * (resetting the chain when the spec moved). */
  private async ensureShelfWindow(): Promise<void> {
    const key = this.specKey();
    if (key !== this.shelfSpecKey) {
      this.shelfSpecKey = key;
      this.datMembers = [];
      this.shelfCursor = null;
      this.shelfExhausted = false;
      this.shelfTotal = 0;
      this.shelfError = false;
      this.rebuildIndex();
    }
    if (!this.shelfExhausted && this.datMembers.length === 0) {
      await this.fetchNextWindow();
    }
  }

  /** Pull the next window of the current chain and append its
   * groups' members. Whole groups only (the catalog's promise);
   * a group already riding along is never duplicated. */
  private async fetchNextWindow(): Promise<void> {
    if (this.shelfLoading || this.shelfExhausted) return;
    const spec = this.buildSpec(this.shelfCursor);
    if (!spec) {
      this.shelfExhausted = true;
      return;
    }
    this.shelfLoading = true;
    const keyAtStart = this.shelfSpecKey;
    try {
      const page = await api.romHomeShelfWindow(spec);
      if (keyAtStart !== this.shelfSpecKey) return; // superseded mid-flight
      this.shelfTotal = page.total;
      // The catalog never repeats a group within a chain; the
      // key guard only has to survive refresh races.
      const haveKeys = new Set(this.datMembers.map((g) => g.key));
      for (const g of page.games) {
        const rg = this.toRoomGame(g);
        if (!haveKeys.has(rg.key)) {
          this.datMembers.push(rg);
          haveKeys.add(rg.key);
        }
      }
      this.shelfCursor = page.nextCursor;
      this.shelfExhausted = page.nextCursor === null;
      this.rebuildIndex();
    } catch {
      // Honest stop: the header keeps the tallies it has, the
      // body says the shelf couldn't be gathered.
      this.shelfError = this.datMembers.length === 0;
      this.shelfExhausted = true;
    } finally {
      this.shelfLoading = false;
    }
  }

  /** "Show all in section": chain windows until the shelf is
   * whole. Bounded chunks, one fetch at a time — the opposite
   * of the old single giant payload. */
  private async loadAllWindows(): Promise<void> {
    const prev = this.bulkLimit;
    this.bulkLimit = 2000;
    try {
      await this.ensureShelfWindow();
      while (!this.shelfExhausted) {
        await this.fetchNextWindow();
      }
    } finally {
      this.bulkLimit = prev;
    }
  }

  private loadedGroupCount(): number {
    return new Set(this.datMembers.map((g) => g.groupKey)).size;
  }

  /** The count a shelf header may print: the window's true
   * total once judged, else the catalog tallies' best reading
   * (corrected the moment the first window lands). */
  private displayTotal(): number {
    const ext = this.extSubset().length;
    if (this.shelfSpecKey === this.specKey() && this.buildSpec(null)) {
      return this.shelfTotal + ext;
    }
    switch (this.shelf.kind) {
      case "all":
        return this.counts.total + this.extGames.length;
      case "favorites":
        return this.counts.favoriteGroups + ext;
      case "recent":
        return this.counts.recentGroups + ext;
      case "missing":
        return this.counts.missingMedia;
      case "system": {
        const key = this.systemKeyOf(this.shelf.name);
        const tally = key ? this.perSystemTallies.get(key) : undefined;
        return (tally?.[0] ?? 0) + ext;
      }
      default:
        return ext;
    }
  }

  private toRoomGame(g: RomHomeGame): RoomGame {
    const media = new Map<string, string>();
    for (const m of g.media) {
      if (m.found && m.localPath) media.set(m.mediaType, m.localPath);
    }
    // PC games carry their adopted art as "cover"; the room's art
    // surfaces all read "boxart" (and only PC items carry cover).
    if (media.has("cover") && !media.has("boxart")) {
      media.set("boxart", media.get("cover")!);
    }
    const systemName = systemNameForKey(g.platform);
    return {
      key: g.item.provider_key ?? g.item.id,
      item: g.item,
      // Group display title: the user's override, else the DAT
      // name with its tag groups stripped (Phase 3 — the canonical
      // region-free form; the full DAT name rides in datTitle).
      title: g.state.titleOverride ?? baseTitle(g.title),
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
      groupKey: g.groupKey,
      variant: g.variant,
      crc: g.crc,
      pc: g.pc ?? null,
    };
  }

  /** Extension-rule systems gather through the rule store (the smart
   * folder machinery): their games join All Games + their shelf with
   * file-name titles and no media identity. Gathering is by
   * sole-claim extensions only (soleClaimExtensions in roms.ts):
   * an extension several systems list assigns NOTHING — the files
   * it names wait for a hash identification in Needs
   * Identification instead of wearing a fabricated platform. */
  private async loadExtSystems(): Promise<void> {
    const soleClaims = soleClaimExtensions(this.systems.map((x) => x.sys));
    for (const s of this.systems) {
      if (s.key !== null || s.placeholder || this.extLoaded.has(s.name)) continue;
      this.extLoaded.add(s.name);
      const claimExts = soleClaims.get(s.name) ?? [];
      if (claimExts.length === 0) continue;
      try {
        const res = await api.ruleItems(claimExts, null, 100000, 0);
        if (!this.active && !this.loaded) return;
        const scoped = this.scopeExtItems(res.items, s.sys);
        const badgeById = new Map((res.badges ?? []).map((b) => [b.item_id, b]));
        let changed = false;
        for (const item of scoped) {
          if (item.kind !== "file") continue;
          const key = item.provider_key ?? item.id;
          if (this.gamesByKey.has(key)) continue;
          const b = badgeById.get(item.id);
          this.extGames.push({
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
            groupKey: null,
            variant: null,
            crc: null,
            pc: null,
          });
          changed = true;
        }
        if (changed) {
          this.rebuildIndex();
          const rs = this.systems.find((x) => x.name === s.name);
          if (rs) {
            rs.count = this.extGames.filter((g) => g.systemName === s.name).length;
            rs.sizeBytes = this.extGames
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
    const ext = this.extGames.filter((g) => g.source === "ext" && !g.stateHydrated);
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
    const favCount = this.counts.favoriteGroups;
    lib.appendChild(this.shelfRow({ kind: "favorites" }, "♥", "Favorites", favCount > 0 ? String(favCount) : ""));
    const recentCount = this.counts.recentGroups;
    lib.appendChild(this.shelfRow({ kind: "recent" }, "🕘", "Recently Played", recentCount > 0 ? String(recentCount) : ""));
    lib.appendChild(
      this.shelfRow(
        { kind: "missing" },
        "🖼",
        "Missing Media",
        this.counts.missingMedia > 0 ? String(this.counts.missingMedia) : "",
      ),
    );
    lib.appendChild(
      this.shelfRow(
        { kind: "needsid" },
        "🔍",
        "Needs Identification",
        this.needsEntries.length > 0 ? String(this.needsEntries.length) : "",
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
   * sits on a disconnected volume (Library Memory truth). Keyed
   * systems read the counts call's session extras (Hyperbase #4);
   * extension systems judge their own loaded games, as before. */
  private systemOffline(name: string): boolean {
    const key = this.systemKeyOf(name);
    if (key) return this.countsExtra.get(key)?.allOffline ?? false;
    const gs = this.extGames.filter((g) => g.systemName === name);
    return gs.length > 0 && gs.every((g) => g.offline);
  }

  private systemVolumeLine(name: string): string | null {
    const key = this.systemKeyOf(name);
    if (key) {
      const extra = this.countsExtra.get(key);
      if (!extra?.volumeLabel) return null;
      return `${extra.volumeLabel} · last seen ${fmtDay(extra.volumeLastSeen)}`;
    }
    const gs = this.extGames.filter((g) => g.systemName === name);
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

  /** The EXTENSION games a shelf holds (Hyperbase #4): DAT
   * shelves are judged in Rust by window spec; extension games
   * still arrive whole per system, so their shelf membership,
   * filter and search readings stay client-side over their own
   * (bounded, per-system) set — the same rules the room has
   * always used, over the ext set alone. */
  private extSubset(ref: ShelfRef = this.shelf): RoomGame[] {
    let pool: RoomGame[];
    switch (ref.kind) {
      case "all":
        pool = [...this.extGames];
        break;
      case "favorites":
        pool = this.extGames.filter((g) => g.state.favorite);
        break;
      case "recent":
        pool = this.extGames.filter((g) => g.state.lastPlayed !== null);
        break;
      case "missing":
      case "needsid":
      case "hardware":
        return [];
      case "system":
        pool = this.extGames.filter((g) => g.systemName === ref.name);
        break;
      case "list": {
        const memberIds = new Set(this.listMemberKeys(ref.id));
        pool = this.extGames.filter((g) => memberIds.has(g.key) || memberIds.has(g.item.id));
        break;
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
        return this.extGames.filter((g) => this.matchesFilters(g, f, true));
      }
      case "search": {
        const q = ref.query.toLowerCase();
        pool = this.extGames.filter(
          (g) =>
            g.title.toLowerCase().includes(q) ||
            g.datTitle.toLowerCase().includes(q) ||
            (g.state.genre ?? "").toLowerCase().includes(q) ||
            g.labels.some((l) => l.toLowerCase().includes(q)) ||
            g.systemName.toLowerCase().includes(q),
        );
        break;
      }
    }
    // The toolbar filters judge ext games exactly as they judge
    // DAT games in the spec (smart shelves already judged by
    // their rule above).
    const f = this.filters;
    const hasFilters =
      f.systems.length + f.genres.length + f.players.length + f.media.length + f.flags.length > 0;
    if (hasFilters) {
      pool = pool.filter((g) => this.matchesFilters(g, f));
    }
    return pool;
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
      // Media presence is a group question now: any copy counts.
      if (m === "has-art" && !this.groupHasMedia(g, "boxart")) return false;
      if (m === "missing-art" && this.groupHasMedia(g, "boxart")) return false;
      if (m === "has-video" && !this.groupHasMedia(g, "trailer")) return false;
    }
    for (const fl of f.flags) {
      if (fl === "starred" && !(g.starred || (g.state.rating ?? 0) >= 1)) return false;
      if (fl === "favorites" && !g.state.favorite) return false;
      if (fl === "completed" && !g.state.completed) return false;
      if (fl === "unplayed" && g.state.playCount > 0) return false;
    }
    return true;
  }

  // ------------------------------------------------------------ grouping (Phase 3)

  /** The last collapsed shelf view — the inspector resolves group
   * representatives through it. */
  private lastVisible: RoomGame[] = [];

  /** Fold items into games (AUDIT §8): DAT items sharing a group
   * key become ONE representative carrying every version (default
   * first); copies of a version fold inside it silently and never
   * count as versions. Extension-rule games never group. */
  private collapseGroups(games: RoomGame[]): RoomGame[] {
    const byGroup = new Map<string, RoomGame[]>();
    const out: RoomGame[] = [];
    for (const g of games) {
      if (g.source !== "dat" || !g.groupKey) {
        out.push(g);
        continue;
      }
      const arr = byGroup.get(g.groupKey);
      if (arr) arr.push(g);
      else byGroup.set(g.groupKey, [g]);
    }
    for (const members of byGroup.values()) out.push(this.collapseOne(members));
    return out;
  }

  private collapseOne(members: RoomGame[]): RoomGame {
    if (members.length === 1) {
      const g = members[0];
      const v = this.versionOf([g]);
      v.isDefault = true;
      g.versions = [v];
      g.playTotal = g.state.playCount;
      g.playLast = g.state.lastPlayed;
      return g;
    }
    // Versions are distinct (descriptor, content) pairs — two
    // dumps of Klax (USA) are two versions; the same dump in two
    // places is one version with copies. Content = sha1 when real,
    // else the evidence CRC (zip identities carry zeroed sha1),
    // else the DAT title.
    const byVersion = new Map<string, RoomGame[]>();
    for (const g of members) {
      const content =
        g.sha1 && !/^0+$/.test(g.sha1) ? g.sha1 : (g.crc ?? g.datTitle);
      const key = `${g.variant?.descriptor ?? ""}|${content}`;
      const arr = byVersion.get(key);
      if (arr) arr.push(g);
      else byVersion.set(key, [g]);
    }
    const versions = [...byVersion.values()].map((copies) =>
      this.versionOf(copies),
    );
    // The default (Dickles' order): never-default variants
    // (badged hacks / patched / trainers / bad dumps) yield while
    // any clean variant exists; then USA → World → Europe →
    // other regions, a release over Beta/Proto/Sample/Demo, the
    // newest revision, descriptor for stability.
    const cmpRank = (a: VersionView, b: VersionView): number =>
      (a.variant?.regionRank ?? 4) - (b.variant?.regionRank ?? 4) ||
      (a.variant?.statusRank ?? 0) - (b.variant?.statusRank ?? 0) ||
      (b.variant?.revisionKey ?? -1) - (a.variant?.revisionKey ?? -1) ||
      a.descriptor.localeCompare(b.descriptor);
    versions.sort(cmpRank);
    const clean = versions.filter((v) => !v.variant?.neverDefault);
    const def = (clean.length > 0 ? clean : versions)[0];
    def.isDefault = true;
    const rep = def.rep;
    rep.versions = versions;
    rep.playTotal = members.reduce((a, g) => a + g.state.playCount, 0);
    rep.playLast = members.reduce<string | null>(
      (acc, g) =>
        g.state.lastPlayed && (!acc || g.state.lastPlayed > acc)
          ? g.state.lastPlayed
          : acc,
      null,
    );
    return rep;
  }

  private versionOf(copies: RoomGame[]): VersionView {
    const sorted = [...copies].sort(
      (a, b) =>
        Number(a.offline) - Number(b.offline) ||
        Number(b.media.has("boxart")) - Number(a.media.has("boxart")) ||
        a.key.localeCompare(b.key),
    );
    const rep = sorted[0];
    return {
      descriptor: rep.variant?.descriptor ?? "",
      variant: rep.variant,
      games: sorted,
      rep,
      isDefault: false,
    };
  }

  /** The whole group a representative stands for, rebuilt from
   * every loaded game — the Missing Media shelf collapses over a
   * filtered member set, but its version lists must be complete.
   * (Hyperbase #4: windows ship whole groups, so a loaded group
   * is always complete here — the rebuild now only ever confirms
   * what the window already carried.) */
  private expandGroup(rep: RoomGame): RoomGame {
    if (!rep.groupKey || !rep.versions) return rep;
    const held = rep.versions.reduce((a, v) => a + v.games.length, 0);
    const members = this.allLoaded().filter((g) => g.groupKey === rep.groupKey);
    if (members.length <= held) return rep;
    return this.collapseOne(members);
  }

  /** The shelf representative for any member of a group (the
   * inspector thinks in groups, selection may hold a member). */
  private groupRepOf(g: RoomGame): RoomGame {
    if (!g.groupKey) return g;
    return this.lastVisible.find((v) => v.groupKey === g.groupKey) ?? g;
  }

  /** Any copy of any version carries this media type? */
  private groupHasMedia(g: RoomGame, type: string): boolean {
    if (!g.versions) return g.media.has(type);
    return g.versions.some((v) => v.games.some((c) => c.media.has(type)));
  }

  /** Mirror a game-scoped state change onto every loaded member
   * of the group (the backend stores it on the group; the room's
   * copies must agree without a reload). */
  private fanOutScoped(g: RoomGame): void {
    // A scoped-state write can move shelf membership (favorite,
    // completed, genre, a fresh play): the window chain re-judges
    // on its next fetch (Hyperbase #4).
    this.shelfGen++;
    if (!g.groupKey) return;
    for (const m of this.allLoaded()) {
      if (m === g || m.groupKey !== g.groupKey || m.source !== "dat") continue;
      m.state = {
        ...m.state,
        rating: g.state.rating,
        favorite: g.state.favorite,
        completed: g.state.completed,
        titleOverride: g.state.titleOverride,
        genre: g.state.genre,
        players: g.state.players,
        notes: g.state.notes,
      };
      m.title = m.state.titleOverride ?? baseTitle(m.datTitle);
    }
  }

  /** The shelf's display list over what's LOADED (Hyperbase
   * #4): DAT groups arrive judged and ordered by the window
   * spec (filters + sort already applied over the full set in
   * Rust), so the collapse is all that remains here. Extension
   * games merge in client-side; when a shelf holds any, the
   * merged list is client-sorted as the room has always sorted
   * merged shelves — the Rust order governs the DAT backbone,
   * and the seam is declared in the build receipt. */
  private visibleLoaded(): RoomGame[] {
    let games = this.collapseGroups(this.datMembers);
    if (this.shelf.kind === "missing") {
      games = games.map((rep) => this.expandGroup(rep));
    }
    const ext = this.extSubset();
    if (ext.length > 0) {
      games = [...games, ...ext];
      this.sortLoaded(games);
    }
    this.lastVisible = games;
    return games;
  }

  /** The room's standing sort orders, applied to a merged
   * (DAT + extension) display list. */
  private sortLoaded(games: RoomGame[]): void {
    const byTitle = (a: RoomGame, b: RoomGame) => a.title.localeCompare(b.title, undefined, { sensitivity: "base" });
    switch (this.sort) {
      case "title":
        games.sort(byTitle);
        break;
      case "year":
        games.sort((a, b) => (b.year ?? -1) - (a.year ?? -1) || byTitle(a, b));
        break;
      case "recent-played":
        games.sort((a, b) => (b.playLast ?? b.state.lastPlayed ?? "").localeCompare(a.playLast ?? a.state.lastPlayed ?? "") || byTitle(a, b));
        break;
      case "most-played":
        games.sort((a, b) => (b.playTotal ?? b.state.playCount) - (a.playTotal ?? a.state.playCount) || byTitle(a, b));
        break;
      case "recent-added":
        games.sort((a, b) => (b.item.modified ?? "").localeCompare(a.item.modified ?? "") || byTitle(a, b));
        break;
    }
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

    // Needs-ID shelf: the grouped dark list, not game tiles.
    if (this.shelf.kind === "needsid") {
      scroll.appendChild(this.needsIdView());
      return;
    }

    // Game shelves fill asynchronously now (Hyperbase #4): the
    // first window is fetched, then the body paints; scrolling
    // chains further windows into the same body.
    this.bodyGen++;
    void this.fillShelf(keepScroll);
    this.attachKeys(scroll);
  }

  /** Fetch the current shelf's window, then paint its body —
   * superseded fills (a newer render, a shelf hop) stand down. */
  private async fillShelf(keepScroll = false): Promise<void> {
    const seq = ++this.fillSeq;
    const gen = this.bodyGen;
    await this.ensureShelfWindow();
    if (seq !== this.fillSeq || gen !== this.bodyGen || !this.active) return;
    const scroll = this.shelfScroll;
    if (!scroll) return;
    this.refreshHeadTexts();
    this.renderChipsBar();
    this.paintShelfGames(scroll);
    if (keepScroll) this.restoreScroll();
  }

  /** Paint the game body from what's loaded: window members
   * collapsed, extension games merged (see visibleLoaded). */
  private paintShelfGames(scroll: HTMLElement): void {
    // A fresh paint retires every filler closure of the last one.
    this.bodyGen++;
    scroll.innerHTML = "";
    if (this.shelfError) {
      const note = el("div", "rh-empty");
      note.append(
        el("div", "rh-empty-emoji", "🛠"),
        el("h3", "", "This shelf couldn't be gathered"),
        el("p", "", "The catalog didn't answer — reopen the room to try again. Nothing was changed."),
      );
      scroll.appendChild(note);
      return;
    }
    const games = this.visibleLoaded();
    if (games.length === 0) {
      scroll.appendChild(this.emptyState());
      return;
    }
    if (this.view === "grid") {
      if (this.shelf.kind === "search") {
        this.renderGridSearch(scroll, games);
      } else {
        const grid = el("div", "rh-grid");
        scroll.appendChild(grid);
        this.fillGridLive(grid);
      }
    } else {
      this.renderListLive(scroll);
    }
  }

  /** The Needs-ID shelf body (Dickles Q3 ✓, 2026-10-16): files no
   * DAT claims, grouped under their parsed filename — regional
   * twins and tagged copies gather under one heading instead of a
   * flat 16k-row swamp. Each row says why it's dark, in plain
   * words, and where it lives. */
  private needsIdView(): HTMLElement {
    const wrap = el("div", "rh-needsid");
    wrap.appendChild(
      el(
        "p",
        "rh-needsid-intro",
        "No DAT set claims these files — hacks, fan translations, bad dumps, homebrew, sets Fable can't read yet, and files that were never games at all. Identifying again is cheap: files already scanned are skipped by size and date.",
      ),
    );
    const run = el(
      "button",
      "pill-btn primary rh-needsid-run",
      "Identify Games",
    ) as HTMLButtonElement;
    run.addEventListener("click", () => {
      run.disabled = true;
      run.textContent = "Identifying…";
      void api
        .identifyRoms()
        .catch(() => undefined)
        .finally(() => void this.softRefresh());
    });
    wrap.appendChild(run);
    if (this.needsEntries.length === 0) {
      const done = el("div", "rh-empty");
      done.append(
        el("div", "rh-empty-emoji", "🎉"),
        el("h3", "", "Everything's identified"),
        el(
          "p",
          "",
          "Every candidate in ROM territory matched a DAT. New files land here until the next identification pass claims them.",
        ),
      );
      wrap.appendChild(done);
      return wrap;
    }
    const groups = new Map<string, RomNeedsIdEntry[]>();
    for (const e of this.needsEntries) {
      const arr = groups.get(e.group);
      if (arr) arr.push(e);
      else groups.set(e.group, [e]);
    }
    for (const [group, entries] of [...groups.entries()].sort((a, b) =>
      a[0].localeCompare(b[0]),
    )) {
      const g = el("div", "rh-needsid-group");
      g.appendChild(
        el(
          "h3",
          "rh-needsid-group-title",
          entries.length > 1 ? `${group} · ${entries.length} files` : group,
        ),
      );
      for (const e of entries) {
        const row = el("div", "rh-needsid-row");
        row.append(
          el("span", "rh-needsid-name", e.item.name),
          el("span", "rh-needsid-reason", e.reason),
        );
        const pk = e.item.provider_key ?? "";
        const folder = pk.includes("/")
          ? pk.slice(0, pk.lastIndexOf("/"))
          : "";
        if (folder) {
          row.appendChild(el("span", "rh-needsid-path", folder));
        }
        g.appendChild(row);
      }
      wrap.appendChild(g);
    }
    return wrap;
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
          el("h3", "", rs ? `No ${rs.name} games yet` : "No games yet"),
        );
        const p = el("p", "");
        p.append(
          document.createTextNode("Games come from your own dumps, homebrew, and public-domain releases. Add a folder of ROMs and they'll be identified against the DAT sets."),
        );
        // Game Warden (2026-10-18): the hunt sentence exists ONLY
        // for a system with a verified archive.org set. No generic
        // fallback — a system without one points nowhere.
        const hunt = rs ? romArchiveLink(rs.key ?? "") : null;
        if (hunt) {
          p.append(document.createTextNode(" You can also hunt the sets at "));
          const link = el("button", "linklike", "Internet Archive");
          link.addEventListener("click", () => openGuideLink(hunt));
          p.append(link, document.createTextNode("."));
        }
        note.appendChild(p);
        const btn = el("button", "pill-btn rh-empty-action", "Add a game folder…") as HTMLButtonElement;
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
        add("🎮", "No games yet", "This room is your collection's home — shelves of art, your ratings and history, every system's library in one calm place. Add a folder of games and Fable will identify them against the DAT sets — then the shelves fill themselves.");
        if (this.counts.total === 0) {
          const btn = el("button", "pill-btn rh-empty-action", "Add a game folder…") as HTMLButtonElement;
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
    let hero: HTMLElement | null = null;
    switch (ref.kind) {
      case "all":
        title = "All Games";
        break;
      case "favorites":
        title = "Favorites";
        break;
      case "recent":
        title = "Recently Played";
        break;
      case "missing":
        title = "Missing Media";
        break;
      case "needsid":
        title = "Needs Identification";
        break;
      case "system":
        title = ref.name;
        hero = this.iconEl(ref.name, 96);
        hero.classList.add("rh-hero-icon");
        break;
      case "hardware":
        title = ref.name;
        break;
      case "list": {
        const c = this.collections.find((x) => x.id === ref.id);
        title = c?.name ?? "List";
        break;
      }
      case "smart": {
        const s = this.smartLists.find((x) => x.id === ref.id);
        title = s?.name ?? "Smart List";
        break;
      }
      case "search":
        title = `“${ref.query}”`;
        break;
    }
    if (hero) head.appendChild(hero);
    const texts = el("div", "rh-head-texts");
    const sub = el("p", "rh-head-sub", this.shelfSubText());
    this.headSubEl = sub;
    texts.append(el("h2", "rh-head-title", title), sub);
    head.appendChild(texts);
    const actions = el("div", "rh-head-actions");
    this.headActionsEl = actions;
    head.appendChild(actions);
    this.renderHeadActions();
    return head;
  }

  /** The header's sub-line, computed from the best numbers the
   * room holds: catalog tallies before the window lands, the
   * shelf's TRUE total after (Hyperbase #4 — refreshHeadTexts
   * re-runs this the moment the first window arrives). */
  private shelfSubText(): string {
    const ref = this.shelf;
    switch (ref.kind) {
      case "all":
        return `${this.displayTotal().toLocaleString()} games`;
      case "favorites":
        return "The ones you love";
      case "recent":
        return "Your history, newest first";
      case "missing":
        return `${this.counts.missingMedia.toLocaleString()} games missing box art — fetch them right here`;
      case "needsid":
        return this.needsEntries.length > 0
          ? `${this.needsEntries.length.toLocaleString()} files no DAT claims — grouped by name`
          : "Every file in ROM territory is identified";
      case "system": {
        const rs = this.systems.find((s) => s.name === ref.name);
        if (!rs) return "";
        const meta = romSystemMeta(ref.name);
        const n = this.displayTotal();
        const key = this.systemKeyOf(ref.name);
        const size = key
          ? (this.countsExtra.get(key)?.sizeBytes ?? 0)
          : this.extSubset().reduce((a, g) => a + (g.item.size_bytes ?? 0), 0);
        let sub = `${meta.gen !== null ? `${ordinal(meta.gen)} Generation · ` : ""}${n.toLocaleString()} games${size > 0 ? ` · ${formatBytes(size)}` : ""}`;
        const vol = this.systemVolumeLine(ref.name);
        if (vol) sub += ` — offline (${vol})`;
        return sub;
      }
      case "hardware":
        return "Owned hardware";
      case "list":
        return "A shelf you fill by hand";
      case "smart": {
        const s = this.smartLists.find((x) => x.id === ref.id);
        return s ? this.smartSentence(s) : "";
      }
      case "search":
        return `${this.displayTotal().toLocaleString()} games across every shelf`;
    }
  }

  /** Header actions (factored out so the window fill can refresh
   * them — the Show-all button only exists once the true total
   * is known, Hyperbase #4). */
  private renderHeadActions(): void {
    const actions = this.headActionsEl;
    if (!actions) return;
    actions.innerHTML = "";
    const ref = this.shelf;
    const isPcShelf = ref.kind === "system" && ref.name === "PC";
    let missingForFetch = 0;
    if (ref.kind === "all" || ref.kind === "missing") {
      missingForFetch = this.counts.missingMedia;
    } else if (ref.kind === "system" && !isPcShelf) {
      // PC games fetch their art at import, never from the ROM
      // cascade — no Get-missing-media button on the PC shelf.
      missingForFetch = this.systems.find((s) => s.name === ref.name)?.missing ?? 0;
    }
    if (isPcShelf) {
      // Library sub-shelves + the two PC doors, right on the
      // section header (PC Games P1).
      const chip = (label: string, active: boolean, pick: () => void) => {
        const b = el(
          "button",
          active ? "pill-btn rh-pc-chip on" : "pill-btn rh-pc-chip",
          label,
        ) as HTMLButtonElement;
        b.addEventListener("click", pick);
        actions.appendChild(b);
      };
      chip("All PC games", this.pcLibrary === null, () => {
        this.pcLibrary = null;
        this.renderShelf(true);
      });
      for (const [name, n] of this.pcLibraries) {
        chip(`${name} (${n.toLocaleString()})`, this.pcLibrary === name, () => {
          this.pcLibrary = name;
          this.renderShelf(true);
        });
      }
      const imp = el(
        "button",
        "pill-btn rh-pc-import",
        "Import Playnite library…",
      ) as HTMLButtonElement;
      imp.addEventListener("click", () => void this.importPcLibrary());
      actions.appendChild(imp);
      const scan = el(
        "button",
        "pill-btn rh-pc-scan",
        "Scan a folder…",
      ) as HTMLButtonElement;
      scan.addEventListener("click", () => this.scanPcFolderMenu(scan));
      actions.appendChild(scan);
    }
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
    if (this.buildSpec(null) && this.shelfTotal > this.loadedGroupCount()) {
      const total = this.displayTotal();
      const showAll = el(
        "button",
        "pill-btn rh-show-all",
        `Show all ${total.toLocaleString()}`,
      ) as HTMLButtonElement;
      showAll.title = "Load the whole shelf, window by window";
      showAll.addEventListener("click", () => {
        void this.loadAllWindows().then(() => {
          if (this.active) {
            this.refreshHeadTexts();
            this.renderChipsBar();
            this.paintShelfGames(this.shelfScroll);
          }
        });
      });
      actions.appendChild(showAll);
    }
    if (missingForFetch > 0) {
      const get = el(
        "button",
        "pill-btn primary rh-get-media",
        `Get missing media (${missingForFetch})`,
      ) as HTMLButtonElement;
      get.addEventListener("click", () => this.fetchMissingForShelf());
      actions.appendChild(get);
    }
  }

  /** Re-read the header's dynamic texts once the first window
   * (and its true total) has landed. */
  private refreshHeadTexts(): void {
    if (this.headSubEl) this.headSubEl.textContent = this.shelfSubText();
    this.renderHeadActions();
  }

  /** Surprise me: a random game off this shelf, straight into the
   * inspector (the spec's serendipity door — no auto-play). */
  private surprise(): void {
    // Picks from what's loaded (Hyperbase #4: the loaded windows
    // are the room's working set — the dice never force a full
    // fetch just to roll).
    const games = this.visibleLoaded().filter((g) => !g.offline);
    const pool = games.length > 0 ? games : this.visibleLoaded();
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

  /** Get missing media for this shelf: the catalog judges the
   * shelf's missing pairs over the FULL set (Hyperbase #4) and
   * the command drops pairs on disconnected volumes, exactly as
   * the room's loaded-set reading did — the fetch list no
   * longer depends on what's on screen. */
  private async fetchMissingForShelf(): Promise<void> {
    // Membership without the toolbar filter layer (the standing
    // reading: the button means the shelf, not the filter).
    const spec = this.buildSpec(null, false);
    if (!spec) return;
    const pairs = await api.romHomeMissingPairs(spec).catch(() => null);
    if (!pairs) {
      Toast.info("Couldn't gather the missing list — try again in a moment.");
      return;
    }
    const retry = pairs
      .filter((p) => (MEDIA_TYPES as readonly string[]).includes(p.mediaType))
      .map((p) => ({
        itemId: p.itemId,
        mediaType: p.mediaType as "boxart" | "screenshot" | "titleshot" | "trailer",
      }));
    void this.startFetch(
      retry,
      `Fetching ${retry.length} missing ${retry.length === 1 ? "file" : "files"} — watch it in the jobs panel.`,
    );
  }

  // ------------------------------------------------------------ chips bar

  private renderChipsBar(): void {
    const bar = this.chipsBar;
    bar.innerHTML = "";
    // Filters judge games; the Needs-ID shelf has none to judge.
    if (this.shelf.kind === "needsid") {
      bar.hidden = true;
      return;
    }
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
    const count = el("span", "rh-chip-count", `${this.displayTotal().toLocaleString()} games`);
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

  /** Search results group by system (Hyperbase #4: sections
   * build from the loaded windows; further windows pull in on
   * the reader's ask and the sections repaint). */
  private renderGridSearch(scroll: HTMLElement, games: RoomGame[]): void {
    const bySystem = new Map<string, RoomGame[]>();
    for (const g of games) {
      const arr = bySystem.get(g.systemName) ?? [];
      arr.push(g);
      bySystem.set(g.systemName, arr);
    }
    for (const [name, gs] of bySystem) {
      scroll.appendChild(el("h3", "rh-group-head", `${name} · ${gs.length}`));
      const grid = el("div", "rh-grid");
      this.fillGridStatic(grid, gs);
      scroll.appendChild(grid);
    }
    if (!this.shelfExhausted) {
      const remaining = Math.max(0, this.displayTotal() - games.length);
      const more = el(
        "button",
        "pill-btn rh-more-results",
        `Show more results${remaining > 0 ? ` (${remaining.toLocaleString()} more)` : ""}`,
      ) as HTMLButtonElement;
      more.addEventListener("click", () => {
        void this.fetchNextWindow().then(() => {
          if (this.active && this.shelfScroll === scroll) this.paintShelfGames(scroll);
        });
      });
      scroll.appendChild(more);
    }
    const esc = el("div", "rh-search-escape");
    const escBtn = el("button", "linklike rh-escape-btn", "Also search all files…") as HTMLButtonElement;
    escBtn.addEventListener("click", () => {
      if (this.shelf.kind === "search") this.hooks.onSearchAllFiles(this.shelf.query);
    });
    esc.appendChild(escBtn);
    scroll.appendChild(esc);
  }

  /** Chunked fill over a fixed set: first 240 tiles paint
   * immediately, the rest append as the reader scrolls. */
  private fillGridStatic(grid: HTMLElement, games: RoomGame[]): void {
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

  /** The live shelf grid (Hyperbase #4): tiles append in
   * window order as the reader scrolls; when the loaded groups
   * run out, the sentinel pulls the NEXT window by cursor —
   * never a re-fetch from zero. Rendered keys are tracked so a
   * mid-scroll extension load can append without ever
   * duplicating or skipping a tile. */
  private fillGridLive(grid: HTMLElement): void {
    const gen = this.bodyGen;
    const CHUNK = 240;
    const rendered = new Set<string>();
    let sentinel: HTMLElement | null = null;
    let io: IntersectionObserver | null = null;
    const step = (): void => {
      if (gen !== this.bodyGen) {
        io?.disconnect();
        return;
      }
      const reps = this.visibleLoaded();
      const frag = document.createDocumentFragment();
      let n = 0;
      for (const g of reps) {
        if (n >= CHUNK) break;
        if (rendered.has(g.key)) continue;
        rendered.add(g.key);
        frag.appendChild(this.tileEl(g, reps));
        n++;
      }
      grid.appendChild(frag);
      const pending = reps.some((g) => !rendered.has(g.key));
      if (!pending && this.shelfExhausted) {
        io?.disconnect();
        sentinel?.remove();
        return;
      }
      if (!sentinel) {
        sentinel = el("div", "rh-sentinel");
        grid.appendChild(sentinel);
        io = new IntersectionObserver(
          (entries) => {
            if (!entries.some((e) => e.isIntersecting)) return;
            if (this.visibleLoaded().some((g) => !rendered.has(g.key))) {
              step();
              return;
            }
            void this.fetchNextWindow().then(() => step());
          },
          { root: this.shelfScroll, rootMargin: "600px" },
        );
        io.observe(sentinel);
      }
    };
    step();
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
    if ((g.versions?.length ?? 0) > 1) {
      const pill = el("span", "rh-tile-ver", `${g.versions!.length} versions`);
      pill.title = "Several versions of this game — open it to pick";
      titleRow.appendChild(pill);
    }
    tile.appendChild(titleRow);
    if (g.pc) {
      // PC Games P1 card facts: the library it came from, the
      // verified-install state, the launch badge, the size.
      const pc = g.pc;
      const row = el("div", "rh-pc-row");
      row.appendChild(el("span", "rh-pc-lib", pc.libraryName));
      const state = el(
        "span",
        pc.installState === "present" ? "rh-pc-state ok" : "rh-pc-state",
        pc.installState === "present" ? "On this volume" : "Not on this volume",
      );
      row.appendChild(state);
      const badgeText =
        pc.launchBadge === "not-checked"
          ? "Launch: not checked"
          : pc.launchBadge === "boots"
            ? "Launch: boots"
            : pc.launchBadge === "boots-with-notes"
              ? "Launch: boots with notes"
              : pc.launchBadge === "wont-boot"
                ? "Launch: won't boot"
                : "Launch: Windows only";
      row.appendChild(el("span", "rh-pc-badge", badgeText));
      if (pc.installSize) {
        row.appendChild(el("span", "rh-pc-size", formatBytes(pc.installSize)));
      }
      if (pc.mislabelNote) {
        const note = el("span", "rh-pc-mislabel", "⚠ naming");
        note.title = pc.mislabelNote;
        row.appendChild(note);
      }
      tile.appendChild(row);
    }

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

  /** The list view over the window chain (Hyperbase #4): the
   * Show-more button spends loaded rows first, then pulls the
   * next window — its count is the shelf's true remaining
   * total, never a window-local number. */
  private renderListLive(scroll: HTMLElement): void {
    const gen = this.bodyGen;
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
    scroll.appendChild(table);
    const CHUNK = 300;
    const rendered = new Set<string>();
    let more: HTMLButtonElement | null = null;
    const syncMore = (): void => {
      const repsLeft = this.visibleLoaded().some((g) => !rendered.has(g.key));
      if (!repsLeft && this.shelfExhausted) {
        more?.remove();
        more = null;
        return;
      }
      if (!more) {
        more = el("button", "pill-btn rh-more-rows", "") as HTMLButtonElement;
        more.addEventListener("click", () => {
          if (gen !== this.bodyGen) return;
          if (this.visibleLoaded().some((g) => !rendered.has(g.key))) {
            addChunk();
            return;
          }
          void this.fetchNextWindow().then(() => {
            if (gen === this.bodyGen) addChunk();
          });
        });
        scroll.appendChild(more);
      }
      const remaining = Math.max(0, this.displayTotal() - rendered.size);
      more.textContent = `Show more (${remaining.toLocaleString()} more)`;
    };
    const addChunk = (): void => {
      if (gen !== this.bodyGen) return;
      const reps = this.visibleLoaded();
      const frag = document.createDocumentFragment();
      let n = 0;
      for (const g of reps) {
        if (n >= CHUNK) break;
        if (rendered.has(g.key)) continue;
        rendered.add(g.key);
        frag.appendChild(this.rowEl(g, reps));
        n++;
      }
      table.appendChild(frag);
      syncMore();
    };
    addChunk();
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
      if (i === 0 && (g.versions?.length ?? 0) > 1) {
        c.appendChild(el("span", "rh-tile-ver", `${g.versions!.length} versions`));
      }
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
    return this.visibleLoaded();
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
        if (!this.shelfExhausted) {
          // The shelf's true end isn't loaded yet — chain the
          // remaining windows, then land on the last game.
          e.preventDefault();
          void this.loadAllWindows().then(() => {
            if (!this.active) return;
            this.paintShelfGames(this.shelfScroll);
            const ord = this.visibleLoaded();
            if (ord.length > 0) {
              this.selection = new Set([ord[ord.length - 1].key]);
              this.anchorKey = ord[ord.length - 1].key;
              this.syncSelectionDom();
              this.renderInspector();
              this.focusGame(ord[ord.length - 1].key);
            }
          });
          return;
        }
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
    if (!pk || !g.platform) {
      this.legacyPlay(g);
      return;
    }
    if (g.pc) {
      // PC games don't launch through the emulator sheet —
      // launching is the PC launch ladder (a later phase). The
      // inspector carries the honest state + the mark door.
      this.selection = new Set([g.key]);
      this.renderInspector();
      Toast.info(
        "Launching PC games from Fable arrives with the PC launch phase — for now the card shows the verified install and its launch state.",
        7000,
      );
      return;
    }
    // Emulation resolution (Architecture A): the backend checks
    // presence, the saved choice and what's installed; Play either
    // launches the real file or returns the ask-once sheet. Any
    // failure falls back to the pre-emulation behavior — Play is
    // never a dead button.
    try {
      const outcome = await api.emuPlay(pk, g.platform);
      if (outcome.kind === "launched") {
        this.afterEmuLaunch(g, outcome);
        return;
      }
      if (outcome.kind === "unavailable") {
        Toast.info(outcome.message);
        return;
      }
      // "access-needed" (the saved emulator's sandbox can't see
      // this drive) also lands on the sheet — its primary becomes
      // the one-tap consent there.
      const sheetData =
        outcome.kind === "sheet"
          ? outcome.sheet
          : await api.emuSheet(pk, g.platform);
      const result = await showEmuSheet(sheetData, {
        onOpenEmulator: (platform) => {
          void api.emuOpenEmulator(platform).catch((err) => {
            Toast.error(`Could not open the emulator: ${errText(err)}`);
          });
        },
      });
      this.applySheetResult(g, result, true);
    } catch {
      this.legacyPlay(g);
    }
  }

  /** "Play with…" — the emulator/core picker (the ask-once sheet
   * in picker mode), with the OS-default open demoted to an entry
   * inside it. */
  private async playWith(g: RoomGame): Promise<void> {
    const pk = g.item.provider_key;
    if (!pk || !g.platform) {
      this.hooks.onOpenWith(g.item);
      return;
    }
    try {
      const sheet = await api.emuSheet(pk, g.platform);
      const result = await showEmuSheet(sheet, {
        onOpenEmulator: (platform) => {
          void api.emuOpenEmulator(platform).catch((err) => {
            Toast.error(`Could not open the emulator: ${errText(err)}`);
          });
        },
      });
      this.applySheetResult(g, result, false);
    } catch {
      this.hooks.onOpenWith(g.item);
    }
  }

  /** The ES-DE door: open the companion front-end on the same
   * library (installs it first when it isn't here — the backend
   * owns that road). */
  private openEsde(): void {
    Toast.info("Opening ES-DE… (first time? Fable sets it up first)");
    void api.emuEsdeOpen().catch((err) => {
      Toast.error(`Could not open ES-DE: ${errText(err)}`);
    });
  }

  /** EmulationStation import (Hyperbase #5): bring gamelist
   * descriptions, artwork and play history into the catalog for
   * the games already indexed. The backend reconciles honestly —
   * the toast reports its numbers, good and bad. */
  private async importEsData(): Promise<void> {
    Toast.info(
      "Importing EmulationStation data… big libraries take a while; the room refreshes when it lands.",
      6000,
    );
    try {
      const rep = await api.esImport();
      if (rep.alreadyRunning) {
        Toast.info("An EmulationStation import is already running.");
        return;
      }
      if (rep.rootsWithEs === 0) {
        Toast.info(
          "No EmulationStation lists found in your locations. Add the folder that holds them — a RetroBat folder, its roms folder, or one system folder — as a location first.",
          9000,
        );
        return;
      }
      const t = rep.totals;
      const matched = t.matchedExact + t.matchedCasefold;
      const unmatched = t.unmatchedNotIndexed + t.unmatchedMissingFile;
      const broken = rep.systems.filter((s) => s.parseError).length;
      Toast.success(
        `EmulationStation import: ${matched.toLocaleString()} games matched · ${t.artFilled.toLocaleString()} artworks in · ${t.datKept.toLocaleString()} DAT identities kept · ${unmatched.toLocaleString()} list entries had no indexed file${broken > 0 ? ` · ${broken} list(s) couldn't be read` : ""}.`,
        12000,
      );
      await this.softRefresh();
    } catch (err) {
      Toast.error(`EmulationStation import failed: ${errText(err)}`);
    }
  }

  /** PC Games P1: the Playnite door. Preview first (parse +
   * plan, no writes) — the confirm names the real numbers,
   * including anything the records get wrong (mislabels,
   * duplicate records) — then the import runs idempotently. */
  private async importPcLibrary(): Promise<void> {
    Toast.info("Reading your Playnite library…", 4000);
    let preview;
    try {
      preview = await api.pcImportPreview();
    } catch (err) {
      Toast.error(`Playnite import failed: ${errText(err)}`);
      return;
    }
    if (!preview.found) {
      Toast.info(
        preview.errors[0] ??
          "No Playnite library found in your locations. Add the drive or folder that holds Playnite as a location first.",
        9000,
      );
      return;
    }
    const lines = [
      `${preview.recordsSeen.toLocaleString()} games in the library at ${preview.libraryPath}.`,
      `${preview.wouldImportPresent.toLocaleString()} verified on a mounted volume · ${preview.recordedNotPresent.toLocaleString()} recorded but not found here (another drive letter, or the drive isn't plugged in).`,
      `${preview.artAvailable.toLocaleString()} artworks to adopt · ${preview.orphanMediaFolders.toLocaleString()} orphan media folder(s) left alone.`,
    ];
    if (preview.duplicatesMerged > 0) {
      lines.push(
        `${preview.duplicatesMerged} duplicate record(s) share a folder with another record — they merge into one game.`,
      );
    }
    for (const n of preview.notes.slice(0, 3)) lines.push(n);
    if (preview.notes.length > 3) {
      lines.push(`…and ${preview.notes.length - 3} more naming notes.`);
    }
    const ok = await confirmDialog(
      "Import Playnite library",
      lines.join("\n"),
      "Import",
    );
    if (!ok) return;
    Toast.info(
      "Importing your Playnite library… the room refreshes when it lands.",
      6000,
    );
    try {
      const rep = await api.pcImportPlaynite();
      if (rep.alreadyRunning) {
        Toast.info("A PC games import or scan is already running.");
        return;
      }
      if (rep.errors.length > 0 && rep.imported + rep.refreshed === 0) {
        Toast.error(`Playnite import failed: ${rep.errors[0]}`, 12000);
        return;
      }
      Toast.success(
        `Playnite import: ${rep.imported.toLocaleString()} new · ${rep.refreshed.toLocaleString()} refreshed · ${rep.recordedNotPresent.toLocaleString()} recorded (not on a mounted volume) · ${rep.artAdopted.toLocaleString()} artworks in${rep.duplicatesMerged > 0 ? ` · ${rep.duplicatesMerged} duplicate record(s) merged` : ""}${rep.mislabelNotes > 0 ? ` · ${rep.mislabelNotes} naming note(s) on cards` : ""}${rep.errors.length > 0 ? ` · ${rep.errors.length} error(s)` : ""}.`,
        12000,
      );
      await this.softRefresh();
    } catch (err) {
      Toast.error(`Playnite import failed: ${errText(err)}`);
    }
  }

  /** PC Games P1: the folder-scan door. Pick one of your
   * locations; its game folders (each holding an .exe) join the
   * PC shelf as "Folder finds" — no Playnite required. */
  private scanPcFolderMenu(anchor: HTMLElement): void {
    void api.listLocations().then((locs) => {
      const withPath = locs.filter((l) => l.path);
      if (withPath.length === 0) {
        Toast.info(
          "No locations to scan yet — add the folder that holds your PC games as a location first.",
          9000,
        );
        return;
      }
      const r = anchor.getBoundingClientRect();
      this.ctxMenu.open(
        r.left,
        r.bottom + 4,
        withPath.map((l) => ({
          label: `${l.display_name} — ${l.path}`,
          action: () => void this.scanPcFolder(l.path!),
        })),
      );
    });
  }

  private async scanPcFolder(root: string): Promise<void> {
    Toast.info(`Scanning ${root} for PC games…`, 5000);
    try {
      const rep = await api.pcScanFolder(root);
      if (rep.alreadyRunning) {
        Toast.info("A PC games import or scan is already running.");
        return;
      }
      if (rep.errors.length > 0 && rep.imported + rep.refreshed === 0) {
        Toast.error(`Folder scan failed: ${rep.errors[0]}`, 12000);
        return;
      }
      Toast.success(
        `Folder scan: ${rep.imported.toLocaleString()} new PC games · ${rep.refreshed.toLocaleString()} refreshed · ${rep.coveredByPlaynite.toLocaleString()} already in Playnite · ${rep.noExeFound.toLocaleString()} folder(s) with no game executable.`,
        12000,
      );
      await this.softRefresh();
    } catch (err) {
      Toast.error(`Folder scan failed: ${errText(err)}`);
    }
  }

  private applySheetResult(
    g: RoomGame,
    result: EmuSheetResult,
    fromPlay: boolean,
  ): void {
    if (result.action === "launched") {
      this.afterEmuLaunch(g, result.outcome);
    } else if (result.action === "es-de") {
      this.openEsde();
    } else if (result.action === "os-default") {
      // Declining is free of consequence: Play does what it did
      // before emulation existed (from Play: launched + noted;
      // from the picker: the plain OS open).
      if (fromPlay) this.legacyPlay(g);
      else this.hooks.onOpenWith(g.item);
    }
  }

  private afterEmuLaunch(
    g: RoomGame,
    outcome: Extract<
      Awaited<ReturnType<typeof api.emuPlay>>,
      { kind: "launched" }
    >,
  ): void {
    if (outcome.playedProviderKey === g.item.provider_key) {
      g.state = outcome.state;
      this.fanOutScoped(g);
    }
    this.renderInspector();
    // Recently Played's membership changed under us; the rail
    // count and (if we're standing on it) the shelf follow.
    void this.refreshTallies();
  }

  /** The pre-emulation Play: shell launch + play bookkeeping. */
  private legacyPlay(g: RoomGame): void {
    const pk = g.item.provider_key;
    this.hooks.onPlayItem(g.item);
    if (pk) {
      api
        .romHomeNotePlay(pk)
        .then((st) => {
          g.state = st;
          this.fanOutScoped(g);
          this.renderInspector();
          void this.refreshTallies();
        })
        .catch(() => {
          // the launch already happened; bookkeeping can lag honestly
        });
    }
  }

  private async setRating(g: RoomGame, rating: number | null): Promise<void> {
    const pk = g.item.provider_key;
    if (!pk) return;
    g.state.rating = rating;
    this.fanOutScoped(g);
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
    this.fanOutScoped(g);
    try {
      await api.romHomeSetFavorite(pk, on);
    } catch (err) {
      Toast.error(`Could not save: ${errText(err)}`);
    }
    void this.refreshTallies();
    this.renderInspector();
  }

  private async setCompleted(g: RoomGame, on: boolean): Promise<void> {
    const pk = g.item.provider_key;
    if (!pk) return;
    g.state.completed = on;
    this.fanOutScoped(g);
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
    // Groups fetch for every version's copies, not just the rep.
    games = games.flatMap((g) =>
      g.versions ? g.versions.flatMap((v) => v.games) : [g],
    );
    const wanted = games.filter(
      (g) => g.source === "dat" && !g.offline && !g.pc,
    );
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
        action: () => void this.playWith(g),
      });
      // Play a specific version (Phase 3): the picker's doors in
      // the room's right-click menu, same as the sheet's list.
      const rep = this.groupRepOf(g);
      if (rep.versions && rep.versions.length > 1) {
        for (const v of rep.versions) {
          const playable = v.games.find((c) => !c.offline) ?? v.rep;
          items.push({
            label: `Play — ${v.descriptor || v.rep.datTitle}${v.isDefault ? " (default)" : ""}`,
            disabled: v.games.every((c) => c.offline),
            action: () => void this.playGame(playable),
          });
        }
      }
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

  // ------------------------------------------------------------ collection completeness

  /** Saved wanted-format preferences (ui-prefs completenessFormats). */
  private ccPrefs: Record<string, string> = {};
  private ccData: RomCompleteness | null = null;
  private ccMissing: RomMissingList | null = null;
  private ccView: { kind: "systems" } | { kind: "missing"; key: string } = { kind: "systems" };
  private ccExpanded = new Set<string>();
  private ccShowCount = 300;
  private ccBody: HTMLElement | null = null;

  private ccNum(n: number): string {
    return n.toLocaleString("en-US");
  }

  private async ccLoadPrefs(): Promise<Record<string, string>> {
    const prefs = await api.getUiPrefs().catch(() => ({}) as Record<string, unknown>);
    const f = (prefs as Record<string, unknown>).completenessFormats;
    if (f && typeof f === "object" && !Array.isArray(f)) {
      return { ...(f as Record<string, string>) };
    }
    return {};
  }

  private async ccSaveFormatPref(key: string, fmt: string): Promise<void> {
    const prefs = await api.getUiPrefs().catch(() => ({}) as Record<string, unknown>);
    const cur = { ...this.ccPrefs };
    if (fmt && fmt !== "Any") cur[key] = fmt;
    else delete cur[key];
    this.ccPrefs = cur;
    await api
      .setUiPrefs({ ...prefs, completenessFormats: cur })
      .catch(() => undefined);
  }

  /** The Collection Completeness sheet: what he owns vs everything
   * there is to own, per system — with the empty systems stated
   * plainly and the unscored ones never faked. */
  private openCompleteness(): void {
    document.querySelector(".rh-cc-backdrop")?.remove();
    const backdrop = el("div", "rh-cc-backdrop");
    const sheet = el("div", "rh-cc-sheet");
    sheet.setAttribute("role", "dialog");
    sheet.setAttribute("aria-label", "Collection completeness");
    const head = el("div", "rh-cc-head");
    const titleWrap = el("div", "rh-cc-titlewrap");
    titleWrap.append(
      el("div", "rh-cc-title", "Collection completeness"),
      el(
        "div",
        "rh-cc-sub",
        "What you own against everything there is to own — counted the same way your shelves count: one game, every version folded in.",
      ),
    );
    const closeBtn = el("button", "icon-btn rh-cc-close", "✕") as HTMLButtonElement;
    closeBtn.title = "Close";
    closeBtn.addEventListener("click", () => backdrop.remove());
    head.append(titleWrap, closeBtn);
    const body = el("div", "rh-cc-body");
    body.append(el("div", "rh-cc-loading", "Counting your shelves…"));
    sheet.append(head, body);
    backdrop.append(sheet);
    backdrop.addEventListener("mousedown", (e) => {
      if (e.target === backdrop) backdrop.remove();
    });
    sheet.addEventListener("keydown", (e) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        backdrop.remove();
      }
    });
    this.el.appendChild(backdrop);
    this.ccBody = body;
    this.ccView = { kind: "systems" };
    closeBtn.focus();
    void this.loadCompleteness();
  }

  private async loadCompleteness(): Promise<void> {
    this.ccPrefs = await this.ccLoadPrefs();
    const data = await api.romCompleteness(this.ccPrefs).catch(() => null);
    if (!this.ccBody || !document.body.contains(this.ccBody)) return;
    if (!data) {
      this.ccBody.replaceChildren(
        el("div", "rh-cc-loading", "Couldn't count just now — the reference data didn't answer. Try again in a moment."),
      );
      return;
    }
    this.ccData = data;
    if (this.ccView.kind === "systems") this.renderCcSystems();
  }

  private ccFormatSelect(s: RomSystemSummary): HTMLSelectElement {
    const sel = document.createElement("select");
    sel.className = "rh-cc-format";
    sel.title = "The format you collect this system in — exports ask for exactly this";
    const current = this.ccData?.formats[s.key] ?? "Any";
    const choices = [
      current,
      "CHD", "GCZ", "CSO", "WBFS", "RVZ", "ISO", "BIN/CUE", "ZIP", "Any",
    ].filter((v, i, a) => a.indexOf(v) === i);
    for (const c of choices) {
      const opt = document.createElement("option");
      opt.value = c;
      opt.textContent = c;
      if (c === current) opt.selected = true;
      sel.appendChild(opt);
    }
    sel.addEventListener("change", () => {
      void this.ccSaveFormatPref(s.key, sel.value).then(() => {
        if (this.ccData) {
          this.ccData.formats[s.key] = sel.value;
          if (sel.value !== "Any" && !this.ccData.formatPrefs.includes(s.key)) {
            this.ccData.formatPrefs.push(s.key);
          }
        }
        Toast.info(
          sel.value === "Any"
            ? `${s.display}: format preference cleared.`
            : `${s.display}: the want list now asks for ${sel.value}.`,
        );
        this.renderCcSystems();
      });
    });
    return sel;
  }

  private ccExportButtons(scope: string | null): HTMLElement {
    const wrap = el("span", "rh-cc-export");
    const mk = (label: string, fmt: string) => {
      const b = el("button", "pill-btn rh-cc-btn", label) as HTMLButtonElement;
      b.addEventListener("click", () => {
        void api
          .romCompletenessExport(scope, fmt, this.ccPrefs)
          .then((r) => {
            if (r.cancelled) return;
            Toast.info(
              `Saved ${this.ccNum(r.rows)} releases (${this.ccNum(r.groups)} games) to ${r.path ?? "the file you picked"}.`,
            );
          })
          .catch(() => Toast.info("The export didn't land — try again in a moment."));
      });
      return b;
    };
    wrap.append(mk("CSV", "csv"), mk("XLSX", "xlsx"));
    return wrap;
  }

  private renderCcSystems(): void {
    if (!this.ccBody || !this.ccData) return;
    const data = this.ccData;
    const body = this.ccBody;
    body.replaceChildren();
    const scored = data.systems.filter((s) => s.hasReference);
    const collecting = scored
      .filter((s) => s.ownedGroups > 0)
      .sort(
        (a, b) =>
          b.ownedGroups / b.totalGroups - a.ownedGroups / a.totalGroups ||
          a.display.localeCompare(b.display),
      );
    const notStarted = scored
      .filter((s) => s.ownedGroups === 0)
      .sort((a, b) => a.display.localeCompare(b.display));

    const band = (title: string, hint: string) => {
      const h = el("div", "rh-cc-band");
      h.append(el("span", "rh-cc-bandtitle", title), el("span", "rh-cc-bandhint", hint));
      body.append(h);
    };
    const rowFor = (s: RomSystemSummary, empty: boolean) => {
      const row = el("div", "rh-cc-row");
      const main = el("div", "rh-cc-main");
      main.append(el("div", "rh-cc-name", s.display));
      if (empty) {
        main.append(
          el(
            "div",
            "rh-cc-line rh-cc-blunt",
            `You don't own a single one of these yet — all ${this.ccNum(s.totalGroups)} games are missing. Here's the list anyway.`,
          ),
        );
      } else {
        const pct = s.totalGroups > 0 ? (s.ownedGroups / s.totalGroups) * 100 : 0;
        const bar = el("div", "rh-cc-bar");
        const fill = el("div", "rh-cc-fill");
        fill.style.width = `${Math.max(1.5, pct)}%`;
        bar.append(fill);
        main.append(bar);
        main.append(
          el(
            "div",
            "rh-cc-line",
            `${this.ccNum(s.ownedGroups)} of ${this.ccNum(s.totalGroups)} games · ${pct.toFixed(1)}% · ${this.ccNum(s.missingGroups)} missing · ${this.ccNum(s.totalReleases)} known releases`,
          ),
        );
        if (s.ownedUnmatched > 0) {
          main.append(
            el(
              "div",
              "rh-cc-line rh-cc-note",
              `${this.ccNum(s.ownedUnmatched)} owned ${s.ownedUnmatched === 1 ? "game isn't" : "games aren't"} in this reference set — counted on your shelf, not scored here.`,
            ),
          );
        }
      }
      row.append(main);
      const fmtWrap = el("label", "rh-cc-fmtwrap");
      fmtWrap.append(el("span", "rh-cc-fmtlab", "Want"));
      fmtWrap.append(this.ccFormatSelect(s));
      if (!data.formatPrefs.includes(s.key)) {
        fmtWrap.append(el("span", "rh-cc-sugg", "suggested"));
      }
      row.append(fmtWrap);
      const listBtn = el(
        "button",
        "pill-btn primary rh-cc-btn",
        empty ? "See the list" : "Missing list",
      ) as HTMLButtonElement;
      listBtn.addEventListener("click", () => void this.openCcMissing(s.key));
      row.append(listBtn);
      row.append(this.ccExportButtons(s.key));
      body.append(row);
    };

    if (collecting.length > 0) {
      band("On your shelves", "the shelf fills as the collection grows");
      for (const s of collecting) rowFor(s, false);
    }
    if (notStarted.length > 0) {
      band("Not started yet", "no shame in it — that's what the list is for");
      for (const s of notStarted) rowFor(s, true);
    }

    // Can't score yet: dataset systems whose reference set is empty,
    // plus room systems Fable shelves but the dataset doesn't carry.
    const unscored: Array<{ name: string; note: string }> = [];
    for (const s of data.systems) {
      if (!s.hasReference) {
        unscored.push({
          name: s.display,
          note: "The reference set for this system is empty — Fable won't guess what's missing.",
        });
      }
    }
    const scoredKeys = new Set(data.systems.map((s) => s.key));
    const scoredNames = new Set(data.systems.map((s) => s.display));
    for (const rs of this.systems) {
      const hasPresence = (rs.count ?? 0) > 0;
      if (!hasPresence) continue;
      if (rs.key && scoredKeys.has(rs.key)) continue;
      if (!rs.key && scoredNames.has(rs.name)) continue;
      unscored.push({
        name: rs.name,
        note: "No reference set loaded for this system yet — Fable can't honestly say what's missing.",
      });
    }
    if (unscored.length > 0) {
      band("Can't score yet", "no reference set, no invented numbers");
      for (const u of unscored) {
        const row = el("div", "rh-cc-row rh-cc-unscored");
        const main = el("div", "rh-cc-main");
        main.append(el("div", "rh-cc-name", u.name), el("div", "rh-cc-line", u.note));
        row.append(main);
        body.append(row);
      }
    }

    const foot = el("div", "rh-cc-foot");
    foot.append(
      el("span", "rh-cc-footlab", "Take the whole want list hunting:"),
      this.ccExportButtons(null),
    );
    body.append(foot);
  }

  private async openCcMissing(key: string): Promise<void> {
    if (!this.ccBody) return;
    this.ccView = { kind: "missing", key };
    this.ccShowCount = 300;
    this.ccExpanded = new Set();
    this.ccBody.replaceChildren(el("div", "rh-cc-loading", "Fetching the list…"));
    const list = await api.romMissingList(key, this.ccPrefs).catch(() => null);
    if (!this.ccBody || !document.body.contains(this.ccBody)) return;
    if (!list) {
      this.ccBody.replaceChildren(
        el("div", "rh-cc-loading", "That list didn't answer — try again in a moment."),
      );
      return;
    }
    this.ccMissing = list;
    this.renderCcMissing();
  }

  private renderCcMissing(): void {
    if (!this.ccBody || !this.ccMissing) return;
    const list = this.ccMissing;
    const body = this.ccBody;
    body.replaceChildren();
    const head = el("div", "rh-cc-listhead");
    const back = el("button", "pill-btn rh-cc-btn", "← All systems") as HTMLButtonElement;
    back.addEventListener("click", () => {
      this.ccView = { kind: "systems" };
      this.renderCcSystems();
    });
    const releases = list.groups.reduce((a, g) => a + g.releases.length, 0);
    head.append(
      back,
      el(
        "div",
        "rh-cc-listtitle",
        `${list.display} — ${this.ccNum(list.groups.length)} missing games · ${this.ccNum(releases)} known releases · hunting format: ${list.format}`,
      ),
      this.ccExportButtons(list.key),
    );
    body.append(head);
    if (list.groups.length === 0) {
      body.append(
        el("div", "rh-cc-loading", "Nothing missing. Every game in the set is on your shelves. Beautiful."),
      );
      return;
    }
    const shown = list.groups.slice(0, this.ccShowCount);
    for (const g of shown) {
      const row = el("div", "rh-cc-group");
      const regions = [...new Set(g.releases.map((r) => r.region).filter(Boolean))].join(" · ");
      const titleLine = el("button", "rh-cc-grouptitle") as HTMLButtonElement;
      titleLine.append(
        el("span", "rh-cc-gname", g.baseTitle),
        el(
          "span",
          "rh-cc-gmeta",
          `${g.releases.length} ${g.releases.length === 1 ? "release" : "releases"}${regions ? ` · ${regions}` : ""}`,
        ),
        el("span", "rh-cc-chev", this.ccExpanded.has(g.groupKey) ? "▾" : "▸"),
      );
      titleLine.addEventListener("click", () => {
        if (this.ccExpanded.has(g.groupKey)) this.ccExpanded.delete(g.groupKey);
        else this.ccExpanded.add(g.groupKey);
        this.renderCcMissing();
      });
      row.append(titleLine);
      if (this.ccExpanded.has(g.groupKey)) {
        const relWrap = el("div", "rh-cc-releases");
        for (const r of g.releases) {
          const rel = el("div", "rh-cc-release");
          const bits = [
            r.region ?? "",
            r.notes,
            r.size > 0 ? formatBytes(r.size) : "",
            r.setShort ? `MAME set ${r.setShort}` : r.crc ? `CRC ${r.crc.toString(16).toUpperCase().padStart(8, "0")}` : "",
          ].filter(Boolean);
          rel.append(
            el("span", "rh-cc-rname", r.name),
            el("span", "rh-cc-rmeta", bits.join(" · ")),
          );
          relWrap.append(rel);
        }
        row.append(relWrap);
      }
      body.append(row);
    }
    if (list.groups.length > shown.length) {
      const more = el(
        "button",
        "pill-btn rh-cc-btn rh-cc-more",
        `Show more (${this.ccNum(list.groups.length - shown.length)} still below)`,
      ) as HTMLButtonElement;
      more.addEventListener("click", () => {
        this.ccShowCount += 500;
        this.renderCcMissing();
      });
      body.append(more);
    }
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
        label: "Open in ES-DE",
        action: () => this.openEsde(),
      },
      {
        label: "Import EmulationStation data…",
        action: () => void this.importEsData(),
      },
      {
        label: "Import Playnite library…",
        action: () => void this.importPcLibrary(),
      },
      {
        label: "Scan a folder for PC games…",
        action: () => this.scanPcFolderMenu(anchor),
      },
      {
        label: "Add a game folder…",
        action: () => this.hooks.onAddRomFolder(),
      },
      {
        label: "Get missing media (everything)",
        action: () => {
          // The whole library's missing pairs, judged in the
          // catalog (Hyperbase #4) — same door as the shelf
          // button, with the "all" spec and no filter layer.
          const spec: RomHomeShelfSpec = {
            kind: "all",
            platform: null,
            query: null,
            searchPlatforms: [],
            memberKeys: null,
            filters: {
              systems: [],
              genres: [],
              players: [],
              media: [],
              flags: [],
              ignoreSystems: false,
            },
            sort: "title",
            cursor: null,
            limit: 1,
          };
          void api
            .romHomeMissingPairs(spec)
            .then((pairs) => {
              const retry = pairs
                .filter((p) => (MEDIA_TYPES as readonly string[]).includes(p.mediaType))
                .map((p) => ({
                  itemId: p.itemId,
                  mediaType: p.mediaType as "boxart" | "screenshot" | "titleshot" | "trailer",
                }));
              void this.startFetch(
                retry,
                `Fetching ${retry.length} missing ${retry.length === 1 ? "file" : "files"} — watch it in the jobs panel.`,
              );
            })
            .catch(() => Toast.info("Couldn't gather the missing list — try again in a moment."));
        },
      },
      { label: "", separator: true, action: () => undefined },
      {
        label: "Collection completeness…",
        action: () => this.openCompleteness(),
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
    // Genre facets come from the catalog's full-set counts now
    // (Hyperbase #4) — the popover must offer every genre in the
    // library, not just the ones in the loaded windows. The
    // loaded fallback covers a facets call that failed.
    const genreSet = new Map<string, number>(this.genreFacets);
    if (genreSet.size === 0) {
      for (const g of this.allLoaded()) {
        if (g.state.genre) genreSet.set(g.state.genre, (genreSet.get(g.state.genre) ?? 0) + 1);
      }
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
      .filter((v): v is RoomGame => !!v)
      // The sheet thinks in groups: any member resolves to its
      // group's shelf representative (versions ride along).
      .map((g) => this.groupRepOf(g));
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
    const missing = games.filter((g) => g.source === "dat" && !g.pc && !g.media.has("boxart")).length;
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

  /** One version row in the sheet: descriptor, the badges that
   * matter (bad dumps wear theirs openly — Q2), copies and play
   * counts, and its own Play. */
  private versionRow(v: VersionView): HTMLElement {
    const row = el("div", "rh-ver-row");
    const main = el("div", "rh-ver-main");
    const name = el(
      "span",
      "rh-ver-name",
      v.descriptor || v.rep.datTitle,
    );
    main.appendChild(name);
    const badges = el("span", "rh-ver-badges");
    if (v.isDefault) {
      badges.appendChild(el("span", "rh-ver-badge rh-ver-default", "Default"));
    }
    const flagBadge: Record<string, [string, string]> = {
      "bad-dump": ["Bad dump", "rh-ver-badge rh-ver-danger"],
      overdump: ["Overdump", "rh-ver-badge rh-ver-danger"],
      trainer: ["Trainer", "rh-ver-badge rh-ver-warn"],
      hack: ["Hack", "rh-ver-badge rh-ver-warn"],
      patched: ["Patched", "rh-ver-badge rh-ver-warn"],
    };
    for (const f of v.variant?.flags ?? []) {
      const b = flagBadge[f];
      if (b) badges.appendChild(el("span", b[1], b[0]));
    }
    main.appendChild(badges);
    const plays = v.games.reduce((a, c) => a + c.state.playCount, 0);
    const meta: string[] = [];
    if (v.games.length > 1) meta.push(`${v.games.length} copies`);
    if (plays > 0) meta.push(`played ${plays}×`);
    meta.push(
      v.games.some((c) => c.media.has("boxart")) ? "has box art" : "no box art",
    );
    main.appendChild(el("span", "rh-ver-meta", meta.join(" · ")));
    row.appendChild(main);
    const playable = v.games.find((c) => !c.offline) ?? v.rep;
    const btn = el("button", "pill-btn rh-ver-play", "▶ Play") as HTMLButtonElement;
    if (v.games.every((c) => c.offline)) {
      btn.disabled = true;
      btn.title = "On a disconnected volume";
    }
    btn.addEventListener("click", () => void this.playGame(playable));
    row.appendChild(btn);
    return row;
  }

  /** PC Games P1: the inspector's PC block — the launch row
   * (badge + the manual mark door; launching itself is a later
   * phase and the row says so) and the imported record's
   * sections, filled from the catalog sheet. Vendor description
   * HTML is rendered as TEXT — never injected. */
  private buildPcInspector(insp: HTMLElement, g: RoomGame): void {
    const pc = g.pc!;
    const row = el("div", "rh-play-row rh-pc-launch-row");
    const badgeLabel =
      pc.launchBadge === "not-checked"
        ? "Launch: not checked"
        : pc.launchBadge === "boots"
          ? "Launch: boots"
          : pc.launchBadge === "boots-with-notes"
            ? "Launch: boots with notes"
            : pc.launchBadge === "wont-boot"
              ? "Launch: won't boot"
              : "Launch: Windows only";
    row.appendChild(el("span", "rh-pc-launch-badge", badgeLabel));
    const mark = el(
      "button",
      "pill-btn rh-pc-mark",
      "Mark launch state…",
    ) as HTMLButtonElement;
    mark.addEventListener("click", () => {
      const r = mark.getBoundingClientRect();
      const options: Array<[string, string]> = [
        ["boots", "Boots"],
        ["boots-with-notes", "Boots with notes"],
        ["wont-boot", "Won't boot"],
        ["windows-only", "Windows only"],
        ["not-checked", "Not checked"],
      ];
      this.ctxMenu.open(
        r.left,
        r.bottom + 4,
        options.map(([badge, label]) => ({
          label,
          checked: pc.launchBadge === badge,
          action: () => {
            const pk = g.item.provider_key;
            if (!pk) return;
            void api
              .pcSetLaunchBadge(pk, badge)
              .then(() => {
                pc.launchBadge = badge;
                this.renderInspector();
                this.renderShelf(true);
              })
              .catch((err) => Toast.error(`Couldn't save: ${errText(err)}`));
          },
        })),
      );
    });
    row.appendChild(mark);
    insp.appendChild(row);
    insp.appendChild(
      el(
        "p",
        "rh-pc-launch-note",
        pc.installState === "present"
          ? "Install verified on this volume. Launching from Fable arrives with the PC launch phase."
          : "Recorded from the library, but the install wasn't found on a mounted volume — reconnect the drive it lives on, or re-import.",
      ),
    );
    const host = el("div", "rh-pc-sheet");
    insp.appendChild(host);
    const pk = g.item.provider_key;
    if (!pk) return;
    void api
      .pcGameSheet(pk)
      .then((sheet) => {
        if (!sheet || !host.isConnected) return;
        this.fillPcSheet(host, sheet);
      })
      .catch(() => undefined);
  }

  private fillPcSheet(host: HTMLElement, sheet: PcGameSheet): void {
    const lab = (t: string) => host.appendChild(el("span", "rh-insp-lab", t));
    const line = (t: string) =>
      host.appendChild(el("p", "rh-pc-line", t));
    if (sheet.mislabelNote) {
      host.appendChild(el("p", "rh-pc-mislabel-note", `⚠ ${sheet.mislabelNote}`));
    }
    if (sheet.description) {
      lab("About");
      // Vendor HTML → plain text (tags stripped by the parser,
      // text assigned via textContent — never markup).
      const doc = new DOMParser().parseFromString(sheet.description, "text/html");
      const text = (doc.body.textContent ?? "").replace(/\s+/g, " ").trim();
      if (text) {
        const p = el("p", "rh-pc-desc");
        p.textContent = text.slice(0, 1200);
        host.appendChild(p);
      }
    }
    const facts: Array<[string, string]> = [];
    if (sheet.developers.length)
      facts.push(["Developed by", sheet.developers.join(", ")]);
    if (sheet.publishers.length)
      facts.push(["Published by", sheet.publishers.join(", ")]);
    if (sheet.genres.length) facts.push(["Genres", sheet.genres.join(" · ")]);
    if (sheet.series.length) facts.push(["Series", sheet.series.join(", ")]);
    if (sheet.features.length)
      facts.push(["Features", sheet.features.join(" · ")]);
    if (sheet.ageRatings.length)
      facts.push(["Age ratings", sheet.ageRatings.join(", ")]);
    if (sheet.storefronts.length)
      facts.push(["Storefronts", sheet.storefronts.join(" · ")]);
    if (sheet.criticScore != null)
      facts.push(["Critic score (from Playnite)", String(sheet.criticScore)]);
    if (sheet.communityScore != null)
      facts.push([
        "Community score (from Playnite)",
        String(sheet.communityScore),
      ]);
    if (sheet.installSize)
      facts.push(["Install size", formatBytes(sheet.installSize)]);
    if (sheet.version) facts.push(["Version", sheet.version]);
    if (sheet.releaseRaw) facts.push(["Released", sheet.releaseRaw]);
    if (facts.length) {
      lab("Details");
      for (const [k, v] of facts) line(`${k}: ${v}`);
    }
    if (sheet.links.length) {
      lab("Links");
      for (const l of sheet.links.slice(0, 8)) {
        const a = el("a", "rh-pc-link", l.name) as HTMLAnchorElement;
        a.href = l.url;
        a.title = l.url;
        a.addEventListener("click", (e) => {
          e.preventDefault();
          void api.openUrl(l.url).catch(() => undefined);
        });
        host.appendChild(a);
      }
    }
    if (sheet.installs.length) {
      lab(
        sheet.installs.length > 1
          ? `Installs (${sheet.installs.length})`
          : "Install",
      );
      for (const inst of sheet.installs) {
        line(
          `${inst.present ? "On this volume" : "Not on this volume"} · ${inst.libraryName} · ${inst.providerKey}`,
        );
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
        if (hero.dataset.artTaken) return;
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

    // ---- Play: ONE primary action. PC games get the honest
    // launch row instead (PC Games P1): there is no emulator to
    // launch through, and the ladder that will run them is a
    // later phase — the badge + the manual mark are what's true
    // today.
    let romPlayRow: HTMLElement | null = null;
    if (g.pc) {
      this.buildPcInspector(insp, g);
    } else {
      const playRow = el("div", "rh-play-row");
      romPlayRow = playRow;
      const play = el("button", "pill-btn primary rh-play", "▶ Play") as HTMLButtonElement;
      if (g.offline) {
        play.disabled = true;
        play.title = `On ${g.volumeLabel ?? "a disconnected volume"}`;
      }
      play.addEventListener("click", () => void this.playGame(g));
      const withBtn = el("button", "pill-btn rh-playwith", "Play with…") as HTMLButtonElement;
      if (g.offline) withBtn.disabled = true;
      withBtn.addEventListener("click", () => void this.playWith(g));
      playRow.append(play, withBtn);
      insp.appendChild(playRow);
    }
    if (g.offline) {
      insp.appendChild(
        el(
          "p",
          "rh-offline-note",
          `On ${g.volumeLabel ?? "a disconnected volume"} · last seen ${fmtDay(g.volumeLastSeen)} — reconnect it to play.`,
        ),
      );
    }

    // ---- Versions (Phase 3): every dump of this game, badged,
    // each playable. The big Play button above plays the default
    // (first row). Bad dumps show badged — never hidden (Q2).
    if (g.versions && g.versions.length > 1) {
      insp.appendChild(
        el("span", "rh-insp-lab", `Versions (${g.versions.length})`),
      );
      for (const v of g.versions) {
        insp.appendChild(this.versionRow(v));
      }
      if ((g.playTotal ?? 0) > 0) {
        insp.appendChild(
          el(
            "p",
            "rh-ver-total",
            `Played ${g.playTotal} ${g.playTotal === 1 ? "time" : "times"} across all versions.`,
          ),
        );
      }
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

    // ---- ES artwork (2026-10-18): the marquee / fanart / manual
    // refs the ES import stored in es_metadata, fetched after
    // first paint (Forge §Y). Until art actually arrives — most
    // games have none — this inspector is exactly the one that
    // shipped before the surface existed.
    if (!g.pc) {
      const pk = g.item.provider_key;
      if (pk) {
        void api
          .esArtSheet(pk)
          .then((sheet) => {
            if (!sheet || !insp.isConnected) return;
            this.applyEsArt(insp, hero, titleRow, romPlayRow, sheet, g);
          })
          .catch(() => undefined);
      }
    }
  }

  /** ES artwork arrival: fanart takes the hero (cover treatment),
   * the marquee becomes the title treatment, and a stored manual
   * earns a button beside Play. Every piece is independently
   * optional, and a failed image load removes only itself — the
   * surface it would have replaced stays exactly as it was. */
  private applyEsArt(
    insp: HTMLElement,
    hero: HTMLElement,
    titleRow: HTMLElement,
    playRow: HTMLElement | null,
    sheet: EsArtSheet,
    g: RoomGame,
  ): void {
    if (sheet.fanart) {
      const img = document.createElement("img");
      img.className = "rh-hero-img rh-hero-fanart";
      img.alt = "";
      img.src = fileSrc(sheet.fanart);
      img.addEventListener("load", () => {
        if (!hero.isConnected) return;
        hero.dataset.artTaken = "1";
        hero
          .querySelectorAll(".rh-hero-img, .rh-placeholder")
          .forEach((n) => n.remove());
        hero.prepend(img);
      });
      img.addEventListener("error", () => img.remove());
    }
    if (sheet.marquee) {
      const img = document.createElement("img");
      img.className = "rh-insp-marquee";
      img.alt = g.title;
      img.src = fileSrc(sheet.marquee);
      img.addEventListener("load", () => {
        if (!insp.isConnected) return;
        titleRow.before(img);
        titleRow.classList.add("rh-has-marquee");
      });
      img.addEventListener("error", () => img.remove());
    }
    if (sheet.manual && playRow) {
      const manualPath = sheet.manual;
      const btn = el(
        "button",
        "pill-btn rh-manual-btn",
        "📖 Manual",
      ) as HTMLButtonElement;
      btn.title = "Open the manual";
      btn.addEventListener("click", () => {
        void api
          .openPath(manualPath)
          .catch((err) =>
            Toast.error(`Couldn't open the manual: ${errText(err)}`),
          );
      });
      playRow.appendChild(btn);
    }
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
            g.title = g.state.titleOverride ?? baseTitle(g.datTitle);
            this.fanOutScoped(g);
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
