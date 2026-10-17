/**
 * Shell assembly: toolbar (nav buttons, breadcrumb path bar, view toggle,
 * search, theme menu), translucent sidebar (Favorites + Locations), tab bar,
 * viewport (file view / search results / Quick Look), and status bar.
 * All styling comes from --ufm-* tokens; this file creates no colors.
 */
import {
  api,
  formatBytes,
  onEmuFleetChanged,
  onLocationsChanged,
  type Item,
  type JobOutcome,
  type JobStart,
  type LocationSummary,
  type ThemeInfo,
} from "./api";
import { FileView, dragState, showItemMenu, romSystemNameForKey, type FileViewHooks } from "./fileview";
import { ColumnsView } from "./columns";
import { GalleryView } from "./gallery";
import { AboutDialog } from "./about";
import { CommandPalette, type PaletteCommand } from "./palette";
import { IndexTracker } from "./indexing";
import { reducedMotion, animateFromLive, liveOpacity } from "./motion";
import { QuickLook } from "./quicklook";
import { ContextMenu } from "./contextmenu";
import { Toast, confirmDialog, promptDialog, errText} from "./toast";
import { openConnectDialog } from "./network";
import { showPluginRunDialog } from "./plugins";
import { SearchView, fmtDay } from "./search";
import { RomHome } from "./romhome";
import { MediaResDialog } from "./mediares";
import { RomOrganizeDialog } from "./romorg";
import { MusicCompleteDialog } from "./musiccomplete";
import { PropertiesDialog } from "./properties";
import { AuditorPanel } from "./auditor";
import { FolderPicker, type PickerShortcut } from "./picker";
import { RenameDialog } from "./rename";
import { LabelManager, saveLabelRules } from "./labels";
import { loadLabelDefs, migrateLabelAssignments, saveLabelDefs } from "./labels";
import { loadRomSystems, saveRomSystems, romsEnabled, romScopeOf, romSystemKey, isRomPlaceholder, setRomsEnabled, romDivisions, loadRomGrouping, saveRomGrouping, loadOpenDivisions, saveOpenDivisions, romIconSlug, loadRomIconSet, saveRomIconSet, type RomSystem, type OwnedHardware } from "./roms";
import { ROM_FLAT_ICONS } from "./romicons_flat";
import { MusicPlayer } from "./music";
import { sectionGlyph, SECTION_ICON_SVG } from "./sectionicons";
import { isPlayableName, isVideoName } from "./video";
import { OpenWithDialog } from "./openwith";
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
  type DirState,
  type Tab,
  type ViewMode,
} from "./tabs";
import { DEFAULT_THEME, DISTRO_ENGINES, ThemeManager, type MenuThemeItem } from "./themes";
import { PreferencesDialog, type PrefsPage } from "./preferences";

/** Parent directory of an absolute path: dirname("/a/b") === "/a",
 * dirname("/foo.mp3") === "/", dirname("/") === "/". Never returns ""
 * (the old `replace(/\/[^/]*$/, "")` yielded "" for root-level files,
 * breaking parent comparisons — Gemini audit, Round 7). */
export function dirname(path: string): string {
  const idx = path.lastIndexOf("/");
  return idx <= 0 ? "/" : path.slice(0, idx);
}

/** Normalize an absolute path for favorite-identity comparisons
 * (inc-2 item 14): trailing slashes don't make a different folder. */
function normFavPath(p: string): string {
  return p.length > 1 ? p.replace(/\/+$/, "") : p;
}

/** Cap on cached archive listings (LRU eviction) — Round 7. */
const ARCHIVE_CACHE_LIMIT = 10;

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
  /** Rule-based folder (Round 5.5): evaluated live by the backend. */
  rule?: { extensions: string[]; minDurationSecs?: number | null };
}

/** Sidebar icon for a smart folder, by its rule's media kind
 * (sections/README.md mapping; mono adaptive set). Extension sets
 * mirror the seeded rule folders; name and search-type are
 * fallbacks; anything unrecognized wears the plain folder. */
function smartFolderIcon(sf: SmartFolder): string {
  const exts = new Set(
    (sf.rule?.extensions ?? []).map((e) => e.toLowerCase()),
  );
  const has = (...xs: string[]) => xs.some((x) => exts.has(x));
  if (has("m4b")) return "audiobooks";
  if (has("mkv", "mp4", "avi", "mov", "m4v", "webm", "mpg", "mpeg", "wmv"))
    return "movies";
  if (has("mp3", "aac", "flac", "ogg", "oga", "opus", "wav", "m4a", "aif", "aiff"))
    return "music";
  if (has("xls", "xlsx", "ods", "csv")) return "spreadsheets";
  if (has("jpg", "jpeg", "png", "gif", "webp", "bmp", "svg", "avif", "heic"))
    return "pictures";
  if (has("epub", "mobi", "azw3")) return "books";
  if (exts.size === 1 && exts.has("pdf")) return "books";
  if (has("doc", "docx", "odt", "pdf", "rtf", "ppt", "pptx", "txt", "md"))
    return "documents";
  switch ((sf.type ?? "").toLowerCase()) {
    case "audio":
      return "music";
    case "video":
      return "movies";
    case "image":
      return "pictures";
    case "document":
      return "documents";
  }
  const n = sf.name.toLowerCase();
  if (n.includes("audiobook")) return "audiobooks";
  if (n.includes("spreadsheet") || n.includes("sheet")) return "spreadsheets";
  if (n.includes("movie") || n.includes("film") || n.includes("video"))
    return "movies";
  if (n.includes("music") || n.includes("song")) return "music";
  if (n.includes("photo") || n.includes("picture") || n.includes("image"))
    return "pictures";
  if (n.includes("book")) return "books";
  if (n.includes("doc")) return "documents";
  return "folder";
}

/** Sidebar icon for a built-in favorite door, by door name. */
function favoriteIcon(name: string): string {
  switch (name) {
    case "Home":
      return "home";
    case "Documents":
      return "documents";
    case "Downloads":
      return "downloads";
    case "Videos":
      return "videos";
    case "Pictures":
      return "pictures";
    case "Music":
      return "music";
    case "Trash":
      return "trash";
    default:
      return "folder";
  }
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
  private refreshBtn!: HTMLButtonElement;
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
  private preferencesDialog = new PreferencesDialog();
  /** The Ctrl+? shortcuts overlay element while it is open. */
  private shortcutsEl: HTMLElement | null = null;
  private palette = new CommandPalette();
  private menuBtn!: HTMLButtonElement;
  /** Undoable file operations (trash / drag-move), newest last. */
  private undoStack: Array<
    | { kind: "trash"; trashName: string; label: string }
    | { kind: "move"; newPath: string; fromDir: string; label: string }
  > = [];
  /** Completion continuations for transfer jobs, keyed by JobId
   * (Phase 1 inc-2 §3.3): jobs finish asynchronously, so the
   * post-completion work (toast, refresh, undo push) is registered at
   * call time and drained by JobsPanel.onTransferDone. */
  private transferContinuations = new Map<
    string,
    (error: string | null) => Promise<void>
  >();
  private quickLook: QuickLook;
  private searchView: SearchView;
  private romHome: RomHome;
  private themeManager: ThemeManager;
  private indexTracker: IndexTracker;
  private ctxMenu = new ContextMenu();
  private propertiesDialog = new PropertiesDialog();
  private auditorPanel = new AuditorPanel();
  private auditorBadgeEl: HTMLElement | null = null;
  private fontScale = 1.0;
  private picker = new FolderPicker();
  private renameDialog = new RenameDialog();
  private labelManager = new LabelManager();
  private searchOpenWith = new OpenWithDialog();
  private mediaRename = new MediaResDialog();
  private romOrganize = new RomOrganizeDialog();
  private musicComplete = new MusicCompleteDialog();
  private musicPlayer = new MusicPlayer();
  private labelList!: HTMLElement;
  private collectionList!: HTMLElement;
  /** Phase 2: collections snapshot for the sidebar + item menu
   * (refreshed by buildSidebar and after every mutation). */
  private collectionsCache: import("./api").CollectionInfo[] = [];
  /** Phase 2: last location whose view profile was applied, per tab
   * — a location's profile applies on ENTRY, not on every load. */
  private profileLocByTab = new Map<number, string>();
  private smartList!: HTMLElement;
  private romsHead!: HTMLElement;
  /** ROM Home door tallies (2026-10-15). */
  private romsDoorCount: HTMLElement | null = null;
  private romsDoorDot: HTMLElement | null = null;
  /** v2.1: fleet pill beside the missing-media dot — emulator
   * updates waiting, or the prep pass at work. */
  private romsFleetPill: HTMLElement | null = null;
  private romsList!: HTMLElement;
  /** Chevron spans of the collapsible sidebar sections (Round 7.2:
   * kept so boot can re-apply durable collapse prefs to the DOM). */
  private labelChevEl: HTMLElement | null = null;
  private collectionChevEl: HTMLElement | null = null;
  private romsChevEl: HTMLElement | null = null;
  /** Durable UI prefs (backend sidecar via api.getUiPrefs). */
  private uiPrefs: Record<string, unknown> = {};
  /** Normalized paths currently shown in Favorites, rebuilt by
   * buildSidebar — the item context menu reads its Add/Remove state
   * from here (inc-2 item 14). */
  private favoritePathSet = new Set<string>();
  /** Built-in favorite name by normalized path, including ones the
   * user has hidden — rebuilt by buildSidebar (inc-2 item 14). */
  private builtinFavNames = new Map<string, string>();
  /** Archive browsing state: archive item id -> its catalog Item and
   * cached listing. */
  private archiveMeta = new Map<string, Item>();
  private archiveCache = new Map<string, ArchiveEntry[]>();
  /** Monotonic per-tab load tokens: a stale async loadDir must never
   * overwrite a newer view (Round 7 race fix — Gemini audit). */
  private loadSeqByTab = new WeakMap<Tab, number>();
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
      onMediaRename: (items: Item[]) =>
        this.mediaRename.open(items, () => {
          this.refreshActiveDir();
          if (this.searchView.active) this.searchView.refresh();
        }),
      onMusicComplete: (items: Item[]) =>
        void this.musicComplete.open(items, () => {
          this.refreshActiveDir();
        }),
      onPlayMusic: (items: Item[]) => this.musicPlayer.open(items),
      onOpen: (item) => this.openItem(item),
      onOpenInNewTab: (item) => void this.openItemInNewTab(item),
      onOpenInRomHome: (item) => {
        // Identity-first (Phase 2): the door exists only for
        // identified games, and it opens THEIR system — resolved
        // from the catalog identity, never an extension guess.
        void api
          .romIdentityMap([item.id])
          .then((hits) => {
            const name = hits[0]
              ? romSystemNameForKey(hits[0].platform)
              : null;
            this.enterRomHome(
              name ? { kind: "system", name } : undefined,
            );
          })
          .catch(() => this.enterRomHome(undefined));
      },
      onOpenRomFolder: (systemName) =>
        this.enterRomHome({ kind: "system", name: systemName }),
      isFavorite: (item) => this.isFavoriteItem(item),
      onToggleFavorite: (item) => void this.toggleFavorite(item),
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
          .catch((err) => Toast.error(`Compress failed: ${errText(err)}`));
      },
      onEmail: (items) => {
        void api
          .emailItems(items.map((i) => i.id))
          .catch((err) => Toast.error(`Email failed: ${errText(err)}`));
      },
      onSendLocalSend: (item) => {
        void api
          .sendViaLocalsend(item.id)
          .then(() => Toast.success("Sent to LocalSend"))
          .catch((err) => Toast.error(`LocalSend failed: ${errText(err)}`));
      },
      onDuplicate: (items) => void this.duplicateItems(items),
      onMakeAlias: (item) => {
        void api
          .makeAlias(item.id)
          .then((p) => {
            Toast.success(`Created alias ${p.split("/").pop()}`, 9000);
            const t = this.activeTab;
            if (t) void this.loadDir(t);
          })
          .catch((err) => Toast.error(`Alias failed: ${errText(err)}`));
      },
      onConvertImage: (item, format) => {
        void api
          .convertImage(item.id, format)
          .then((p) => {
            Toast.success(`Converted to ${p.split("/").pop()}`);
            const t = this.activeTab;
            if (t) void this.loadDir(t);
          })
          .catch((err) => Toast.error(`Convert failed: ${errText(err)}`));
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
          .catch((err) => Toast.error(`PDF failed: ${errText(err)}`));
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
          .catch((err) => Toast.error(`Rotate failed: ${errText(err)}`));
      },
      onSetWallpaper: (item) => {
        void api
          .setWallpaper(item.id)
          .then(() => Toast.success("Desktop picture set"))
          .catch((err) => Toast.error(`Wallpaper failed: ${errText(err)}`));
      },
      onToggleLabel: (items, label, on) =>
        void this.toggleLabel(items, label, on),
      onToggleStar: (items, on) => void this.toggleStarItems(items, on),
      onListCollections: () =>
        this.collectionsCache
          .filter((c) => c.kind === "manual")
          .map((c) => ({ id: c.id, name: c.name })),
      onAddToCollection: (items, id) => {
        const c = this.collectionsCache.find((x) => x.id === id);
        void this.addItemsToCollection(items, id, c?.name ?? "collection");
      },
      onNewCollection: (items) => {
        void (async () => {
          const created = await this.createCollectionFlow();
          if (created) {
            await this.addItemsToCollection(items, created.id, created.name);
          }
        })();
      },
      inCollectionView: () => this.currentCollectionId() !== null,
      onRemoveFromCollection: (items) => {
        const id = this.currentCollectionId();
        if (!id) return;
        void (async () => {
          try {
            await api.removeFromCollection(
              id,
              items.map((i) => i.id),
            );
            void this.buildSidebar();
            this.refreshActiveDir();
            Toast.success(
              `Removed ${items.length} item${items.length === 1 ? "" : "s"} from the collection.`,
            );
          } catch (err) {
            Toast.error(`Remove failed: ${errText(err)}`);
          }
        })();
      },
      onViewStateChange: () => this.saveViewProfile(),
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
          .catch((err) => Toast.error(`Transcode failed: ${errText(err)}`));
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
          .catch((err) => Toast.error(`Combine failed: ${errText(err)}`));
      },
      onPrint: (item) => void this.printDialog.open(item),
      onCreateTorrent: (item) => void this.torrentDialog.open(item),
      onNewFolder: () => void this.createFolderHere(),
      onListPlugins: () => this.pluginCache,
      onRunPlugin: (item, key) => void this.runPluginFlow(item, key),
      onListTemplates: () => this.templateCache,
      onNewFolderFromTemplate: (templateId) =>
        void this.newFolderFromTemplateFlow(templateId),
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
    this.quickLook.castPicker = (name, video) =>
      this.castDialog.openPicker(name, video);
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
      currentDirPath: () => this.currentDirPath(),
      scopes: () =>
        this.locations.map((l) => ({
          id: l.id as string | null,
          name: l.display_name,
          provider: l.provider,
        })),
      onNavigateToItem: (item) => {
        this.searchView.clear(); // back to the file view before navigating
        this.openItem(item);
      },
      onPreviewItem: (item) => {
        // Space on the hit already previewed (fix batch item 1):
        // while its Quick Look media is inline, Space is
        // play/pause — not a re-open that restarts the preview.
        if (
          this.quickLook.isOpen &&
          this.quickLook.item?.id === item.id &&
          this.quickLook.toggleMediaPlayback()
        ) {
          return;
        }
        void this.quickLook.open(item);
      },
      // Attachment rules (Dickles 2026-10-15): selection inside
      // search / Smart Folder results reaches Quick Look through
      // the same follow door as the browse views.
      onSelectItem: (item) => this.quickLook.followSelection(item),
      onPopOutItem: (item) => this.popOutItem(item),
      onOfflineItem: (hit) => {
        // Library Memory: plain language, never a dead click.
        Toast.info(
          `“${hit.item.name}” is on “${hit.volume_label ?? "a drive"}”, which isn't connected right now (last seen ${fmtDay(hit.volume_last_seen)}). Connect the drive and try again.`,
          6000,
        );
      },
      onTrashItems: (items) => {
        void this.trashItems(items).then(() => this.searchView.refresh());
      },
      onContextMenu: (item, targets, x, y) => {
        void showItemMenu(
          this.ctxMenu,
          x,
          y,
          item,
          targets.length > 0 ? targets : [item],
          viewHooks,
          (i) => this.searchOpenWith.open(i),
        );
      },
      onResults: () => {
        this.fileView.el.hidden = true;
        this.columnsView.el.hidden = true;
        this.galleryView.el.hidden = true;
        this.renderCrumbs();
      },
      onClear: () => {
        this.showFileView();
        this.renderCrumbs();
      },
      onReindex: () => this.reindexLibrary(),
      onSaveSmartFolder: () => void this.saveSmartFolder(),
    });
    this.romHome = new RomHome({
      onExit: () => this.exitRomHome(),
      onPlayItem: (item) => this.openItem(item),
      onPreviewItem: (item) => void this.quickLook.open(item),
      onRevealItem: (item) => void this.revealRomInFiles(item),
      onOpenWith: (item) => this.searchOpenWith.open(item),
      onOpenAuditor: () => this.openAuditor(),
      onAddRomFolder: () => void this.addRomFolder(),
      onSearchAllFiles: (query) => {
        this.exitRomHome();
        this.searchView.focus();
        const input = this.searchView.field.querySelector("input");
        if (input) {
          input.value = query;
          input.dispatchEvent(new Event("input", { bubbles: true }));
        }
      },
      onManageSystem: (sys, x, y) => {
        this.ctxMenu.open(x, y, [
          {
            label: `Manage "${sys.name}"…`,
            action: () => this.openRomEditor(sys),
          },
          {
            label: `Delete "${sys.name}"`,
            danger: true,
            action: () => {
              void (async () => {
                const ok = await confirmDialog(
                  `Delete "${sys.name}"?`,
                  "The system is removed from the collection. Your ROM files are not touched.",
                  "Delete",
                  true,
                );
                if (!ok) return;
                saveRomSystems(
                  loadRomSystems().filter((s) => s.name !== sys.name),
                );
                void this.buildSidebar();
                this.romHome.invalidate();
                Toast.success(`Deleted system "${sys.name}".`);
              })();
            },
          },
        ]);
      },
    });
    this.viewport.append(
      this.fileView.el,
      this.columnsView.el,
      this.galleryView.el,
      this.searchView.el,
      this.romHome.el,
      this.quickLook.el
    );
    // The search field lives in the toolbar; its results panel is in the viewport.
    this.searchWrap.appendChild(this.searchView.field);
    // Re-index also rides beside the search field (Round 7.2): it is
    // in the hamburger menu and the results header too — an important
    // feature earns the duplication (Dickles). Since 2026-10-16 it is
    // WORD-labeled: as a bare ↻ glyph it impersonated a folder Refresh
    // and users pressed it hunting for one (Dickles). The ↻ glyph now
    // belongs to Refresh in the path bar, alone.
    const reindexBtn = document.createElement("button");
    reindexBtn.className = "icon-btn reindex-btn";
    reindexBtn.textContent = "Reindex";
    reindexBtn.title = "Rebuild the catalog index";
    reindexBtn.setAttribute("aria-label", "Rebuild the catalog index");
    reindexBtn.addEventListener("click", () => this.reindexLibrary());
    this.searchWrap.appendChild(reindexBtn);
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
      Toast.error(`Job failed: ${errText(error)}`);
    };
    this.jobsPanel.onTransferDone = (jobId, error) => {
      const cont = this.transferContinuations.get(jobId);
      this.transferContinuations.delete(jobId);
      if (cont) {
        void cont(error);
      } else {
        // No call-site continuation (e.g. a hydrated recovered job
        // cancelled from the panel): still refresh + report.
        if (error) Toast.error(`Job failed: ${errText(error)}`);
        this.refreshActiveDir();
      }
    };
    void this.jobsPanel.hydrate();
    this.dupDialog.onTrackJob = (jobId, label, onDone) =>
      this.trackTransfer(jobId, label, onDone);

    this.themeManager = new ThemeManager({
      renderMenu: (themes, activeId, anchor) => this.renderThemeMenu(themes, activeId, anchor),
      closeMenu: () => this.closeMenu(),
    });
    this.indexTracker = new IndexTracker(
      (text) => {
        this.statusRight.textContent = text ?? "";
        this.statusRight.hidden = !text;
      },
      () => {
        // A full re-index pass finished: refresh the visible smart
        // folder / search results so new files appear live.
        if (this.searchView.active) this.searchView.rerunAfterIndex();
        // ROM Lists: refresh identifications after each full pass —
        // unchanged files are never rehashed (the pass is fresh-cached).
        void api.identifyRoms().catch(() => undefined);
      },
    );
    // Inc-2 item 13: the search view mirrors indexing state in its
    // results area (banner + honest empty-state copy).
    this.indexTracker.onObserve = (busy, files) =>
      this.searchView.setIndexingState(busy, files);

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
    // Phase 6: warm the plugin/template menu caches.
    void this.refreshPluginTemplateCaches();
    // Durable UI prefs (Round 7.2): hydrate sidebar collapse state
    // from the backend sidecar — WebKit localStorage alone lost it
    // across sessions. Adopt any localStorage-only values once so
    // existing users keep their layout.
    try {
      const prefs = await api.getUiPrefs();
      if (prefs && typeof prefs === "object") {
        this.uiPrefs = prefs;
        let migrated = false;
        for (const key of ["ff-labels-collapsed", "ff-roms-collapsed"]) {
          if (typeof this.uiPrefs[key] !== "boolean") {
            try {
              const ls = localStorage.getItem(key);
              if (ls !== null) {
                this.uiPrefs[key] = ls === "1";
                migrated = true;
              }
            } catch {
              // no fast path available
            }
          }
        }
        if (migrated) {
          void api.setUiPrefs({ ...this.uiPrefs }).catch(() => undefined);
        }
        this.applyCollapsePrefs();
        // Text size + grid icon size (UI round): durable prefs,
        // hydrated with the rest.
        const fs = this.uiPrefs["ff-font-scale"];
        if (typeof fs === "number" && fs >= 0.7 && fs <= 2.0) {
          this.setFontScale(fs);
        }
        const isz = this.uiPrefs["ff-icon-size"];
        if (typeof isz === "number") {
          this.iconSize = Math.min(96, Math.max(32, Math.round(isz)));
        }
        this.applyIconSize();
      }
    } catch {
      // prefs are a nicety — the localStorage fast path already ran
    }
    // Volumes come and go while the app runs (Round 7.2): refresh the
    // locations list when the window regains focus, throttled so a
    // burst of focus events doesn't rebuild the sidebar repeatedly.
    let lastLocRefresh = Date.now();
    window.addEventListener("focus", () => {
      const now = Date.now();
      if (now - lastLocRefresh < 3000) return;
      lastLocRefresh = now;
      void this.refreshLocations();
    });
    // Mount hot-plug (backend VolumeMonitor watcher): a phone plugged
    // in or a drive yanked re-lists locations right away — no focus
    // change, no relaunch. One plug fires several monitor signals, so
    // coalesce bursts: refresh at most once a second, with a trailing
    // refresh so the last signal of a burst is never dropped.
    let locEventTimer: number | undefined;
    void onLocationsChanged(() => {
      const now = Date.now();
      if (now - lastLocRefresh >= 1000) {
        lastLocRefresh = now;
        void this.refreshLocations();
        return;
      }
      if (locEventTimer !== undefined) return;
      locEventTimer = window.setTimeout(() => {
        locEventTimer = undefined;
        lastLocRefresh = Date.now();
        void this.refreshLocations();
      }, 1000 - (now - lastLocRefresh));
    });
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
    // Auto volumes are browsable immediately but are NOT walked at
    // boot (a multi-TB USB drive would stall the chain); Re-index
    // Library still covers them on demand. Devices (phones/cameras)
    // are not walked either — a phone is a guest, never library
    // memory, and the backend refuses to index one outright.
    void this.indexTracker.boot(
      this.locations
        // Offline network bookmarks aren't registered for browsing —
        // indexing one would just be refused. They index on Connect.
        .filter(
          (l) => l.provider !== "volume" && l.provider !== "device" && l.online,
        )
        .map((l) => l.id),
    );
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
    // Refresh lives in the path bar, where instinct looks for it
    // (Dickles, 2026-10-16): it re-reads the current view in place.
    // The bare ↻ glyph means THIS and only this in the chrome now —
    // the re-index control beside the search field is word-labeled.
    const refreshBtn = document.createElement("button");
    refreshBtn.className = "icon-btn refresh-btn";
    refreshBtn.textContent = "↻";
    refreshBtn.title = "Refresh (F5)";
    refreshBtn.setAttribute("aria-label", "Refresh current view");
    refreshBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      this.refreshCurrentView();
    });
    crumbsWrap.append(this.crumbBar, copyBtn, refreshBtn);
    this.refreshBtn = refreshBtn;
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
    // Inc-2 item 14: the Favorites section itself is a drop door —
    // drag a FOLDER onto the header or the list background to pin
    // it. Buttons inside the list keep their own drop behavior
    // (file-into / reorder); files are never accepted here.
    const favDoorOver = (e: DragEvent): void => {
      const it = dragState.item;
      if (!it || dragState.favName || it.kind !== "folder") return;
      e.preventDefault();
      if (e.dataTransfer) e.dataTransfer.dropEffect = "move";
      favHead.classList.add("drop-hover");
      this.favList.classList.add("drop-hover");
    };
    const favDoorClear = (): void => {
      favHead.classList.remove("drop-hover");
      this.favList.classList.remove("drop-hover");
    };
    const favDoorDrop = (e: DragEvent): void => {
      e.preventDefault();
      favDoorClear();
      const it = dragState.item;
      if (it && it.kind === "folder") void this.addFavoriteFromItem(it);
    };
    favHead.addEventListener("dragover", favDoorOver);
    favHead.addEventListener("dragleave", favDoorClear);
    favHead.addEventListener("drop", favDoorDrop);
    this.favList.addEventListener("dragover", (e) => {
      if ((e.target as HTMLElement).closest("button")) return;
      favDoorOver(e);
    });
    this.favList.addEventListener("dragleave", favDoorClear);
    this.favList.addEventListener("drop", (e) => {
      if ((e.target as HTMLElement).closest("button")) return;
      favDoorDrop(e);
    });
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
    this.labelChevEl = labelChev;
    this.labelList = document.createElement("div");
    this.labelList.className = "side-list";
    // Inc-2 item 11 fix: initialize from prefCollapsed (durable
    // backend prefs win, localStorage is only the pre-boot fast
    // path). The old localStorage-only read here is why a re-index
    // rebuild re-expanded sections whose collapse lived only in the
    // backend sidecar — buildSidebar runs on ~18 paths, boot's
    // applyCollapsePrefs only once.
    const labelsCollapsed = this.prefCollapsed("ff-labels-collapsed");
    this.labelList.hidden = labelsCollapsed;
    labelChev.style.transform = labelsCollapsed ? "rotate(-90deg)" : "";
    labelHead.addEventListener("click", () => {
      const nowHidden = !this.labelList.hidden;
      this.labelList.hidden = nowHidden;
      labelChev.style.transform = nowHidden ? "rotate(-90deg)" : "";
      this.setCollapsedPref("ff-labels-collapsed", nowHidden);
    });
    // Phase 2: Collections — collapsible, mirroring Labels exactly.
    // Right-click the header to make a new one.
    const collHead = document.createElement("button");
    collHead.className = "side-head side-collapse";
    collHead.setAttribute("aria-label", "Toggle Collections section");
    const collHeadText = document.createElement("span");
    collHeadText.textContent = "Collections";
    const collChev = document.createElement("span");
    collChev.className = "side-chev";
    collChev.textContent = "▾";
    collHead.append(collHeadText, collChev);
    this.collectionChevEl = collChev;
    collHead.title = "Right-click to create a collection";
    collHead.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      e.stopPropagation();
      this.ctxMenu.open(e.clientX, e.clientY, [
        {
          label: "New Collection…",
          action: () => void this.createCollectionFlow(),
        },
      ]);
    });
    this.collectionList = document.createElement("div");
    this.collectionList.className = "side-list";
    const collsCollapsed = this.prefCollapsed("ff-collections-collapsed");
    this.collectionList.hidden = collsCollapsed;
    collChev.style.transform = collsCollapsed ? "rotate(-90deg)" : "";
    collHead.addEventListener("click", () => {
      const nowHidden = !this.collectionList.hidden;
      this.collectionList.hidden = nowHidden;
      collChev.style.transform = nowHidden ? "rotate(-90deg)" : "";
      this.setCollapsedPref("ff-collections-collapsed", nowHidden);
    });
    // File Auditor (Dickles, 2026-10-15): a special section — one
    // entry row with the open-flag count as a calm badge. The
    // section itself is the overlay review surface (auditor.ts).
    const auditorHead = document.createElement("h2");
    auditorHead.className = "side-head";
    auditorHead.textContent = "File Auditor";
    const auditorRow = document.createElement("button");
    auditorRow.className = "side-item";
    const auditorGlyph = sectionGlyph("auditor");
    const auditorLabel = document.createElement("span");
    auditorLabel.textContent = "Review findings";
    const auditorBadge = document.createElement("span");
    auditorBadge.className = "side-count";
    auditorBadge.textContent = "";
    this.auditorBadgeEl = auditorBadge;
    auditorRow.append(auditorGlyph, auditorLabel, auditorBadge);
    auditorRow.title =
      "Audit folders and drives for typos, pattern breaks, corruption, duplicates, junk and drift — findings wait here for your verdict";
    auditorRow.addEventListener("click", () => this.openAuditor());
    const smartHead = document.createElement("h2");
    smartHead.className = "side-head";
    smartHead.textContent = "Smart Folders";
    this.smartList = document.createElement("div");
    this.smartList.className = "side-list";
    // Round 7.1: collapsible, mirroring the Labels section exactly —
    // chevron, list visibility toggle, persisted ff-roms-collapsed.
    // The right-click Add-a-System menu is preserved.
    // ROM Home (2026-10-15): the Roms Collection section is now a
    // single door into the room — the per-system tree retired. The
    // right-click Add-a-System menu is preserved on the door.
    const romsHead = document.createElement("button");
    romsHead.className = "side-head rom-door";
    romsHead.setAttribute("aria-label", "Open ROM Home");
    const romsGlyph = document.createElement("span");
    romsGlyph.className = "rom-door-glyph";
    romsGlyph.innerHTML = `<svg viewBox="0 0 48 48" aria-hidden="true">${SECTION_ICON_SVG.roms}</svg>`;
    const romsHeadText = document.createElement("span");
    romsHeadText.className = "rom-door-label";
    romsHeadText.textContent = "ROMs";
    const romsCount = document.createElement("span");
    romsCount.className = "rom-door-count";
    const romsDot = document.createElement("span");
    romsDot.className = "rom-door-dot";
    romsDot.title = "Some games are missing media";
    romsDot.hidden = true;
    const romsPill = document.createElement("span");
    romsPill.className = "rom-door-pill";
    romsPill.hidden = true;
    const romsChev = document.createElement("span");
    romsChev.className = "side-chev";
    romsChev.hidden = true;
    romsHead.append(romsGlyph, romsHeadText, romsCount, romsDot, romsPill, romsChev);
    this.romsChevEl = romsChev;
    this.romsDoorCount = romsCount;
    this.romsDoorDot = romsDot;
    this.romsFleetPill = romsPill;
    // v2.1: the pill follows the fleet (prep progress + update
    // findings) live, not just when the door's counts refresh.
    void onEmuFleetChanged(() => void this.refreshRomDoor()).catch(() => {});
    romsHead.title = "ROM Home — right-click to add a system";
    romsHead.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      e.stopPropagation();
      this.ctxMenu.open(e.clientX, e.clientY, [
        { label: "Add a System\u2026", action: () => this.openRomEditor(null) },
      ]);
    });
    romsHead.addEventListener("click", () => this.enterRomHome());
    this.romsHead = romsHead;
    this.romsList = document.createElement("div");
    this.romsList.className = "side-list";
    this.romsList.hidden = true;
    sidebar.append(
      favHead,
      this.favList,
      labelHead,
      this.labelList,
      collHead,
      this.collectionList,
      auditorHead,
      auditorRow,
      smartHead,
      this.smartList,
      this.romsHead,
      this.romsList,
      locHead,
      this.sidebarList,
    );
    void this.refreshAuditorBadge();

    const main = document.createElement("main");
    main.className = "main";
    this.tabBar = document.createElement("div");
    this.tabBar.className = "tabbar";
    this.tabBar.setAttribute("role", "tablist");
    this.viewport = document.createElement("div");
    this.viewport.className = "viewport";
    main.append(this.tabBar, this.viewport, this.termPanel.el);
    // Jobs dock (UI round, Dickles' screenshot): the panel's real
    // home is the bottom of the main column, directly above the
    // status bar — in-flow chrome, never a floating overlay, so it
    // cannot cover the status/indexing readouts at any window size.
    // (appendChild relocates it from the shell root, where the
    // constructor parked it with the dialogs.)
    main.append(this.jobsPanel.el);

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
    b.className = "icon-btn nav-btn";
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
    if (tab === this.activeTab && this.romHome?.active) this.exitRomHome();
    // Round 7 (Gemini audit): capture a load token; if a newer load for
    // this tab starts while we await, this result is stale — discard it
    // silently instead of overwriting the current view.
    const seq = (this.loadSeqByTab.get(tab) ?? 0) + 1;
    this.loadSeqByTab.set(tab, seq);
    const isCurrent = (): boolean => this.loadSeqByTab.get(tab) === seq;
    let items: Item[];
    let gone = false;
    let collectionMissing: string[] = [];
    try {
      if (dir.dirId.startsWith("label:")) {
        items = await api.listItemsByLabel(dir.dirId.slice("label:".length));
      } else if (dir.dirId.startsWith("collection:")) {
        const contents = await api.collectionItems(
          dir.dirId.slice("collection:".length),
        );
        items = contents.items;
        collectionMissing = contents.missing;
      } else if (dir.dirId === "starred:") {
        items = await api.starredItems();
      } else if (dir.dirId.startsWith("archive:")) {
        items = await this.archiveItems(dir.dirId);
      } else {
        items = await api.listDir(dir.locationId, dir.dirId);
      }
    } catch (err) {
      if (!isCurrent()) return;
      // Round 7 (Copilot audit): a failed archive open must not strand
      // the user in an empty tab — toast and keep the previous view.
      if (dir.dirId.startsWith("archive:")) {
        Toast.error(`Couldn't open that archive: ${errText(err)}`);
        return;
      }
      // Fix block item 4: a vanished folder renders a humane "this
      // folder is gone" state, never a silent empty void.
      gone = true;
      items = [];
    }
    tab.dirGone = gone;
    if (!isCurrent()) return; // stale load finished late — discard it
    tab.items = items;
    // Per-location view memory: entering a location restores its
    // remembered view mode + sort before anything renders.
    await this.applyViewProfile(tab, dir);
    if (!isCurrent()) return;
    if (tab === this.activeTab) {
      this.renderActiveView();
      this.renderCrumbs();
      this.renderNavButtons();
      this.renderStatus();
      if (collectionMissing.length > 0) {
        Toast.info(
          `${collectionMissing.length} item${collectionMissing.length === 1 ? " in this collection no longer exists" : "s in this collection no longer exist"} — right-click the collection in the sidebar to clean up.`,
          5000,
        );
      }
    }
  }

  private navigateTo(dir: { locationId: string; dirId: string; crumbs: { dirId: string; name: string }[] }): void {
    if (this.searchView.active) this.searchView.clear();
    if (this.romHome?.active) this.exitRomHome();
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
      if (
        dir.dirId.startsWith("label:") ||
        dir.dirId.startsWith("collection:") ||
        dir.dirId === "starred:"
      ) {
        // A folder in a cross-location view opens at its real location.
        this.navigateTo({
          locationId: item.location,
          dirId: item.id,
          crumbs: [{ dirId: item.id, name: item.name }],
        });
      } else if (
        item.location === dir.locationId &&
        tab.items.some((i) => i.id === item.id)
      ) {
        // A true child of the directory being browsed: descend.
        this.navigateTo(descend(tab, item));
      } else {
        // A search hit / cross-location / rule-view folder: appending
        // crumbs here FABRICATED paths (…/richard/Videos/Videos/…,
        // 2026-10-14). Walk its real path from the location root.
        void this.openFolderByPath(item);
      }
    } else if (isPlayableName(item.name) && item.provider_key) {
      // Double-click law (fix batch item 7): playable media pops
      // out into the Fable player window, playing — in EVERY
      // view, no per-surface exceptions. Non-media keeps the OS
      // default below.
      this.popOutItem(item);
    } else if (Shell.isArchiveName(item.name)) {
      this.openArchive(item);
    } else {
      // Double-click / Enter opens with the OS default app; Spacebar is Quick Look.
      void api
        .openItem(item.id)
        .catch(() => void this.quickLook.open(item));
    }
  }

  /** Pop a playable item out into its player window, playing
   * (fix batch item 7) — the one door behind double-click in the
   * file views and in search results alike. */
  popOutItem(item: Item): void {
    if (!item.provider_key) return;
    void api
      .openPlayerWindow(item.provider_key, item.name, {
        kind: isVideoName(item.name) ? "video" : "audio",
        start: 0,
        autoplay: true,
      })
      .then(() => this.quickLook.notePoppedOut(item))
      .catch((err) =>
        Toast.error(`Could not open the player: ${errText(err)}`),
      );
  }

  /** Walk from a location's root to `targetDirPath`, building the
   * TRUE crumb chain (each level resolved by listing the parent).
   * Returns null when the path can't be walked (folder moved, drive
   * unmounted, different root). Shared by Open in New Tab and by
   * search-hit navigation (fix block item 4). */
  private async crumbsForPath(
    loc: LocationSummary,
    targetDirPath: string,
  ): Promise<{ dirId: string; crumbs: { dirId: string; name: string }[] } | null> {
    const rootPath = await api.getItemPath(loc.root_item).catch(() => "");
    if (!rootPath) return null;
    if (targetDirPath === rootPath) {
      return {
        dirId: loc.root_item,
        crumbs: [{ dirId: loc.root_item, name: loc.display_name }],
      };
    }
    if (!targetDirPath.startsWith(rootPath + "/")) return null;
    const segments = targetDirPath
      .slice(rootPath.length + 1)
      .split("/")
      .filter((s) => s.length > 0);
    const crumbs: { dirId: string; name: string }[] = [
      { dirId: loc.root_item, name: loc.display_name },
    ];
    let curDirId = loc.root_item;
    for (const seg of segments) {
      const entries = await api.listDir(loc.id, curDirId).catch(() => null);
      if (!entries) return null;
      const next = entries.find((e) => e.kind === "folder" && e.name === seg);
      if (!next) return null; // path moved on
      crumbs.push({ dirId: next.id, name: next.name });
      curDirId = next.id;
    }
    return { dirId: curDirId, crumbs };
  }

  /** Open a folder that is NOT a child of the current directory
   * (search hit, cross-location jump) at its real path, with real
   * crumbs — never by appending to the current path. */
  private async openFolderByPath(item: Item): Promise<void> {
    const loc =
      this.locations.find((l) => l.id === item.location) ?? this.locations[0];
    if (!loc || !item.provider_key) {
      this.navigateTo({
        locationId: item.location,
        dirId: item.id,
        crumbs: [{ dirId: item.id, name: item.name }],
      });
      return;
    }
    const walked = await this.crumbsForPath(loc, item.provider_key);
    if (walked) {
      this.navigateTo({
        locationId: loc.id,
        dirId: walked.dirId,
        crumbs: walked.crumbs,
      });
    } else {
      this.navigateTo({
        locationId: item.location,
        dirId: item.id,
        crumbs: [{ dirId: item.id, name: item.name }],
      });
    }
  }

  /**
   * Open in New Tab (Round 7.1): folders open a tab AT the folder;
   * files open a tab at their parent folder with the file selected.
   * Item ids are path-stable but there is no path→id lookup, so the
   * target directory is reached by walking from the location root,
   * listing each level and building real crumbs on the way down.
   */
  private async openItemInNewTab(item: Item): Promise<void> {
    const loc =
      this.locations.find((l) => l.id === item.location) ?? this.locations[0];
    if (!loc) return;
    const tab = this.newTab(loc);
    if (!item.provider_key) return; // non-local item: root tab is the best available
    const targetDirPath =
      item.kind === "folder"
        ? item.provider_key
        : dirname(item.provider_key);
    const walked = await this.crumbsForPath(loc, targetDirPath);
    if (walked && walked.dirId !== loc.root_item) {
      navigate(tab, { locationId: loc.id, dirId: walked.dirId, crumbs: walked.crumbs });
      tab.title = walked.crumbs[walked.crumbs.length - 1].name;
    }
    await this.loadDir(tab);
    if (item.kind !== "folder" && tab.items.some((i) => i.id === item.id)) {
      tab.selection.clear();
      tab.selection.add(item.id);
      if (tab === this.activeTab) this.renderActiveView();
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
    if (entries) {
      // LRU: refresh recency on hit (Map preserves insertion order).
      this.archiveCache.delete(archiveId);
      this.archiveCache.set(archiveId, entries);
    } else {
      entries = await api.listArchive(archiveId);
      this.archiveCache.set(archiveId, entries);
      // Cap the cache: evict least-recently-used listings (Round 7).
      while (this.archiveCache.size > ARCHIVE_CACHE_LIMIT) {
        const oldest = this.archiveCache.keys().next().value;
        if (oldest === undefined) break;
        this.archiveCache.delete(oldest);
      }
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
      Toast.error(`Could not open ${item.name}: ${errText(err)}`);
    }
  }

  private onSelect(item: Item | null): void {
    this.renderStatus();
    this.renderCrumbs();
    // Quick Look attachment (Dickles 2026-10-15): every browse
    // view funnels selection through this hook — mouse clicks
    // and keyboard navigation alike — so an open attached
    // preview follows the selection from here. Detached media
    // stays pinned inside QuickLook; a closed pane ignores it.
    this.quickLook.followSelection(item);
  }

  private togglePreview(): void {
    // Space while media Quick Look is open (fix batch item 1):
    // play/pause. Space still closes the preview when what it
    // shows is not playing media; Esc always closes.
    if (this.quickLook.isOpen && this.quickLook.toggleMediaPlayback()) {
      return;
    }
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

  /** Track a transfer job in the panel and register what should
   * happen when it reaches a terminal state (inc-2 §3.3). */
  private trackTransfer(
    jobId: string,
    label: string,
    onDone: (error: string | null) => Promise<void>,
  ): void {
    this.transferContinuations.set(jobId, onDone);
    this.jobsPanel.trackJob(jobId, label);
  }

  /** Unwrap a start_* result (UI round double-submit guard): a
   * fresh job yields its id; a submission the backend folded into
   * an identical active job yields null — the caller abandons its
   * own tracking (the original door's continuation still owns the
   * finish line), the user is told plainly, and the panel focuses
   * the one job that is actually running. `noun` is the operation
   * word for the toast: copy / move / delete / trash. */
  private acceptJobStart(start: JobStart, noun: string): string | null {
    if (!start.alreadyRunning) return start.jobId;
    Toast.info(`That ${noun} is already running`);
    this.jobsPanel.focusJob(start.jobId);
    return null;
  }

  /** Outcomes of a finished job; [] when unavailable (unknown job /
   * backend error) — undo simply offers less in that case. */
  private async jobOutcomes(jobId: string): Promise<JobOutcome[]> {
    try {
      return await api.getJobOutcomes(jobId);
    } catch {
      return [];
    }
  }

  /** Feed the undo stack from a finished trash job's outcomes. */
  private pushTrashUndo(
    outcomes: JobOutcome[],
    nameByPath: Map<string, string>,
  ): number {
    let pushed = 0;
    for (const o of outcomes) {
      if (!o.trash_name) continue;
      this.undoStack.push({
        kind: "trash",
        trashName: o.trash_name,
        label: nameByPath.get(o.src_path) ?? o.src_path.split("/").pop() ?? o.src_path,
      });
      pushed++;
    }
    return pushed;
  }

  /** Move an item to the Trash as a tracked job (inc-2: replaces the
   * direct trash_item call; undo is fed by job outcomes at
   * completion). */
  private async deleteItem(item: Item): Promise<void> {
    try {
      const start = await api.startDelete([item.id], true);
      const jobId = this.acceptJobStart(start, "trash");
      if (!jobId) return;
      this.quickLook.close();
      this.trackTransfer(jobId, `Trash ${item.name}`, async (error) => {
        if (error) {
          Toast.error(`Could not delete “${item.name}”: ${errText(error)}`);
        } else {
          const outcomes = await this.jobOutcomes(jobId);
          this.pushTrashUndo(
            outcomes,
            new Map([[item.provider_key ?? "", item.name]]),
          );
          Toast.success(`Moved “${item.name}” to Trash. Super+Z to undo.`);
        }
        const tab = this.activeTab;
        if (tab) await this.loadDir(tab);
      });
    } catch (err) {
      Toast.error(`Could not delete “${item.name}”: ${errText(err)}`);
    }
  }

  /** Move a whole selection to the Trash as ONE tracked job (inc-2;
   * replaces the per-item trash_item loop — failures are now
   * per-job and surface via the panel + toast at completion). */
  private async trashItems(items: Item[]): Promise<void> {
    if (items.length === 0) return;
    try {
      const start = await api.startDelete(
        items.map((i) => i.id),
        true,
      );
      const jobId = this.acceptJobStart(start, "trash");
      if (!jobId) return;
      this.quickLook.close();
      const nameByPath = new Map(
        items.map(
          (i) => [i.provider_key ?? "", i.name] as [string, string],
        ),
      );
      const label =
        items.length === 1
          ? `Trash ${items[0].name}`
          : `Trash ${items.length} items`;
      this.trackTransfer(jobId, label, async (error) => {
        // Partial failure is a real outcome now (the executor attempts
        // every item and names the ones that failed): whatever DID
        // trash still earns its undo entries, and the toast tells the
        // whole truth — moved X of N, plus the failure summary (the
        // panel row carries the same text with a Copy button).
        const outcomes = await this.jobOutcomes(jobId);
        const ok = this.pushTrashUndo(outcomes, nameByPath);
        if (error && ok > 0 && ok < items.length) {
          Toast.error(
            `Moved ${ok} of ${items.length} items to Trash. ${errText(error)}`,
          );
        } else if (error) {
          Toast.error(`Could not trash items: ${errText(error)}`);
        } else if (ok === 1)
          Toast.success(
            `Moved “${items[0].name}” to Trash. Super+Z to undo.`,
          );
        else if (ok > 1)
          Toast.success(`Moved ${ok} items to Trash. Super+Z to undo.`);
        const t = this.activeTab;
        if (t) await this.loadDir(t);
        if (this.searchView.active) this.searchView.refresh();
      });
    } catch (err) {
      Toast.error(`Could not trash items: ${errText(err)}`);
    }
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
      Toast.error(`Rename failed: ${errText(err)}`);
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
    // Every mutation funnels through here (fix batch item 2):
    // cached smart-folder / search views are DATA-stale now and
    // re-gather on their next visit. Navigation alone never
    // invalidates them — changes do.
    this.searchView.invalidateViewCache();
    const t = this.activeTab;
    if (t) void this.loadDir(t);
  }

  /** Manual Refresh (path-bar ↻ button / F5 / Ctrl+R): re-read what
   * the user is looking at, in place — the folder from disk, the
   * search / smart-folder view by re-running it, ROM Home by its soft
   * refresh. Cheap and local: NO catalog re-index, NO navigation.
   * (Dickles, 2026-10-16: there was no way to do this at all — the
   * only ↻ in the chrome rebuilt the whole index.) */
  private refreshCurrentView(): void {
    // ROM Home is a room, not a tab: its own soft refresh reloads the
    // shelves and keeps the selection.
    if (this.romHome?.active) {
      this.romHome.invalidate();
      return;
    }
    // Search results and smart folders ARE the visible view: re-run
    // whatever they are showing (query, rule, ROM shelf).
    if (this.searchView.active) {
      this.searchView.refreshCurrentView();
      return;
    }
    const tab = this.activeTab;
    if (!tab) return;
    // FileView.render() returns the scroller to the top — right for
    // navigation, wrong for a refresh in place. Snapshot and restore.
    const scroller =
      this.fileView.el.querySelector<HTMLElement>(".fileview-scroller");
    const fileTop = scroller?.scrollTop ?? 0;
    const colTop = this.columnsView.el.scrollTop;
    const galTop = this.galleryView.el.scrollTop;
    void this.loadDir(tab).then(() => {
      if (tab !== this.activeTab) return;
      // Files that vanished leave ghost ids in the selection; prune
      // them so the status line never counts the dead as selected.
      if (tab.selection.size > 0) {
        const alive = new Set(tab.items.map((i) => i.id));
        for (const id of [...tab.selection]) {
          if (!alive.has(id)) tab.selection.delete(id);
        }
        this.renderStatus();
      }
      requestAnimationFrame(() => {
        if (scroller) scroller.scrollTop = fileTop;
        this.columnsView.el.scrollTop = colTop;
        this.galleryView.el.scrollTop = galTop;
      });
      this.flashStatus("Refreshed");
    });
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
      Toast.error(`Clipboard failed: ${errText(err)}`);
    }
  }

  private async pasteIntoCurrentDir(): Promise<void> {
    const tab = this.activeTab;
    if (!tab) return;
    const dir = currentDir(tab);
    if (
      dir.dirId.startsWith("label:") ||
      dir.dirId.startsWith("collection:") ||
      dir.dirId === "starred:" ||
      dir.dirId.startsWith("archive:")
    ) {
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
      // One job per paste, not per path (inc-2 §3.3). The planner
      // owns the same-folder cut no-op and the copy-into-same-folder
      // suffix idiom now.
      const paths = clip.paths;
      const start = clip.cut
        ? await api.startMoveFromPaths(paths, destDir)
        : await api.startCopyFromPaths(paths, destDir);
      const jobId = this.acceptJobStart(start, clip.cut ? "move" : "copy");
      if (!jobId) return;
      this.trackTransfer(
        jobId,
        clip.cut ? "Move" : "Copy",
        async (error) => {
          if (error) {
            Toast.error(`Paste failed: ${errText(error)}`);
          } else if (clip.cut) {
            const outcomes = await this.jobOutcomes(jobId);
            let moved = 0;
            for (const o of outcomes) {
              if (!o.result_path) continue;
              this.undoStack.push({
                kind: "move",
                newPath: o.result_path,
                fromDir: dirname(o.src_path),
                label: o.src_path.split("/").pop() ?? o.src_path,
              });
              moved++;
            }
            if (moved > 0) {
              Toast.success(
                `Pasted ${moved} item${moved === 1 ? "" : "s"} (moved)`,
              );
            }
          } else {
            Toast.success(
              `Pasted ${paths.length} item${paths.length === 1 ? "" : "s"}`,
            );
          }
          this.refreshActiveDir();
        },
      );
    } catch (err) {
      Toast.error(`Paste failed: ${errText(err)}`);
    }
  }

  private async duplicateItems(items: Item[]): Promise<void> {
    // One copy job per parent folder (inc-2 §3.3; a selection lives
    // in one folder in practice, but grouping keeps odd selections
    // honest).
    const byParent = new Map<string, string[]>();
    for (const item of items) {
      if (!item.provider_key) continue;
      const parent = dirname(item.provider_key);
      const group = byParent.get(parent) ?? [];
      group.push(item.provider_key);
      byParent.set(parent, group);
    }
    if (byParent.size === 0) return;
    for (const [parent, paths] of byParent) {
      const count = paths.length;
      try {
        const start = await api.startCopyFromPaths(paths, parent);
        const jobId = this.acceptJobStart(start, "copy");
        if (!jobId) continue;
        this.trackTransfer(jobId, "Duplicate", async (error) => {
          if (error) Toast.error(`Duplicate failed: ${errText(error)}`);
          else {
            Toast.success(
              `Duplicated ${count} item${count === 1 ? "" : "s"}`,
            );
          }
          this.refreshActiveDir();
        });
      } catch (err) {
        Toast.error(`Could not duplicate: ${errText(err)}`);
      }
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
    try {
      if (mode === "move") {
        const start = await api.startMove(
          items.map((i) => i.id),
          dest,
        );
        const jobId = this.acceptJobStart(start, "move");
        if (!jobId) return;
        const nameByPath = new Map(
          items.map(
            (i) => [i.provider_key ?? "", i.name] as [string, string],
          ),
        );
        this.trackTransfer(jobId, "Move", async (error) => {
          if (error) {
            Toast.error(`Could not move items: ${errText(error)}`);
          } else {
            const outcomes = await this.jobOutcomes(jobId);
            let moved = 0;
            for (const o of outcomes) {
              if (!o.result_path) continue;
              const fromDir = dirname(o.src_path);
              // Parity with the old direct flow: no undo entry for
              // a no-op move (source already in the destination).
              if (!fromDir || dirname(o.result_path) === fromDir) continue;
              this.undoStack.push({
                kind: "move",
                newPath: o.result_path,
                fromDir,
                label:
                  nameByPath.get(o.src_path) ??
                  o.src_path.split("/").pop() ??
                  o.src_path,
              });
              moved++;
            }
            if (moved > 0) {
              Toast.success(
                `Moved ${moved} item${moved === 1 ? "" : "s"}. Super+Z to undo.`,
              );
            }
          }
          this.refreshActiveDir();
        });
      } else {
        const paths = items
          .map((i) => i.provider_key)
          .filter((p): p is string => !!p);
        if (paths.length === 0) return;
        const start = await api.startCopyFromPaths(paths, dest);
        const jobId = this.acceptJobStart(start, "copy");
        if (!jobId) return;
        this.trackTransfer(jobId, "Copy", async (error) => {
          if (error) Toast.error(`Could not copy items: ${errText(error)}`);
          else {
            Toast.success(
              `Copied ${paths.length} item${paths.length === 1 ? "" : "s"}`,
            );
          }
          this.refreshActiveDir();
        });
      }
    } catch (err) {
      Toast.error(`Could not ${mode} items: ${errText(err)}`);
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
        Toast.error(`Label failed for ${item.name}: ${errText(err)}`);
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
      Toast.error(`Extract failed: ${errText(err)}`);
    }
  }

  private openLabel(name: string): void {
    if (this.searchView.active) this.searchView.clear();
    const dirId = `label:${name}`;
    this.navigateTo({
      locationId: "",
      dirId,
      crumbs: [{ dirId, name: `🏷 ${name}` }],
    });
  }

  // ------------------------------------------------ Phase 2: collections

  private openCollection(id: string, name: string): void {
    if (this.searchView.active) this.searchView.clear();
    const dirId = `collection:${id}`;
    this.navigateTo({
      locationId: "",
      dirId,
      crumbs: [{ dirId, name }],
    });
  }

  private openStarred(): void {
    if (this.searchView.active) this.searchView.clear();
    const dirId = "starred:";
    this.navigateTo({
      locationId: "",
      dirId,
      crumbs: [{ dirId, name: "★ Starred" }],
    });
  }

  /** The collection the active tab is browsing, if any. */
  private currentCollectionId(): string | null {
    const tab = this.activeTab;
    if (!tab) return null;
    const dir = currentDir(tab);
    return dir.dirId.startsWith("collection:")
      ? dir.dirId.slice("collection:".length)
      : null;
  }

  private async createCollectionFlow(): Promise<{
    id: string;
    name: string;
  } | null> {
    const name = await promptDialog(
      "New Collection",
      "e.g. Tax documents",
      "",
    );
    if (!name) return null;
    try {
      const id = await api.createCollection(name);
      void this.buildSidebar();
      Toast.success(`Collection “${name}” created.`);
      return { id, name };
    } catch (err) {
      Toast.error(`Couldn't create collection: ${errText(err)}`);
      return null;
    }
  }

  private async renameCollectionFlow(
    c: import("./api").CollectionInfo,
  ): Promise<void> {
    const name = await promptDialog("Rename Collection", "", c.name);
    if (!name || name === c.name) return;
    try {
      await api.renameCollection(c.id, name);
      void this.buildSidebar();
      Toast.success(`Renamed to “${name}”.`);
    } catch (err) {
      Toast.error(`Rename failed: ${errText(err)}`);
    }
  }

  private async deleteCollectionFlow(
    c: import("./api").CollectionInfo,
  ): Promise<void> {
    const ok = await confirmDialog(
      `Delete collection “${c.name}”?`,
      "The collection goes away. The files in it are not touched.",
      "Delete Collection",
      true,
    );
    if (!ok) return;
    try {
      await api.deleteCollection(c.id);
      // If the active tab is browsing it, fall back to the first location.
      if (this.currentCollectionId() === c.id && this.locations[0]) {
        const loc = this.locations[0];
        this.navigateTo({
          locationId: loc.id,
          dirId: loc.root_item,
          crumbs: [{ dirId: loc.root_item, name: loc.display_name }],
        });
      }
      void this.buildSidebar();
      Toast.success(`Collection “${c.name}” deleted.`);
    } catch (err) {
      Toast.error(`Delete failed: ${errText(err)}`);
    }
  }

  private async cleanCollectionMissing(
    c: import("./api").CollectionInfo,
  ): Promise<void> {
    try {
      const contents = await api.collectionItems(c.id);
      if (contents.missing.length > 0) {
        await api.removeCollectionPaths(c.id, contents.missing);
      }
      void this.buildSidebar();
      if (this.currentCollectionId() === c.id) this.refreshActiveDir();
      Toast.success(
        `Removed ${contents.missing.length} missing entr${contents.missing.length === 1 ? "y" : "ies"} from “${c.name}”.`,
      );
    } catch (err) {
      Toast.error(`Cleanup failed: ${errText(err)}`);
    }
  }

  private async addItemsToCollection(
    items: Item[],
    id: string,
    name: string,
  ): Promise<void> {
    try {
      await api.addToCollection(
        id,
        items.map((i) => i.id),
      );
      void this.buildSidebar();
      Toast.success(
        `Added ${items.length} item${items.length === 1 ? "" : "s"} to “${name}”.`,
      );
    } catch (err) {
      Toast.error(`Couldn't add to collection: ${errText(err)}`);
    }
  }

  /** Item-menu star toggle: set semantics built on the backend
   * toggle (only items whose state differs are flipped). */
  private async toggleStarItems(items: Item[], on: boolean): Promise<void> {
    try {
      for (const item of items) {
        if (item.starred === on) continue;
        item.starred = await api.toggleStar(item.id);
      }
      this.refreshActiveViewOnly();
    } catch (err) {
      Toast.error(`Star failed: ${errText(err)}`);
      this.refreshActiveDir();
    }
  }

  /** Re-render the active view in place (star/label state changed,
   * no reload needed). */
  private refreshActiveViewOnly(): void {
    if (this.activeTab && !this.searchView.active) this.renderActiveView();
    if (this.searchView.active) this.searchView.refresh();
  }

  // ------------------------------------------- Phase 2: view profiles

  /** Apply a location's remembered view profile on ENTRY (once per
   * location per tab): view mode + sort, restored from the catalog. */
  private async applyViewProfile(tab: Tab, dir: DirState): Promise<void> {
    if (!dir.locationId || dir.dirId.includes(":")) return;
    if (this.profileLocByTab.get(tab.id) === dir.locationId) return;
    this.profileLocByTab.set(tab.id, dir.locationId);
    const profile = await api
      .getViewProfile(dir.locationId)
      .catch(() => null);
    if (!profile) return;
    const modes = ["grid", "list", "columns", "gallery"] as const;
    if ((modes as readonly string[]).includes(profile.view_mode)) {
      tab.view = profile.view_mode as Tab["view"];
    }
    if (["name", "size", "modified"].includes(profile.sort_key)) {
      tab.sort = profile.sort_key as Tab["sort"];
    }
    tab.sortDir = profile.sort_dir < 0 ? -1 : 1;
  }

  /** Persist the active tab's view state as its location's profile.
   * Only real location dirs have profiles — pseudo views (labels,
   * collections, search) never overwrite a location's memory. */
  private saveViewProfile(): void {
    const tab = this.activeTab;
    if (!tab) return;
    const dir = currentDir(tab);
    if (!dir.locationId || dir.dirId.includes(":")) return;
    void api
      .setViewProfile(dir.locationId, {
        view_mode: tab.view,
        sort_key: tab.sort,
        sort_dir: tab.sortDir,
        show_extensions: true,
      })
      .catch(() => undefined);
  }

  /** Prepopulated rule folders (Dickles, 2026-10-13). Music keeps
   * only clips longer than 30 s; Movies only features over 70 min;
   * Documents excludes .md/.txt by his explicit order. */
  private static readonly DEFAULT_RULE_FOLDERS: SmartFolder[] = [
    {
      name: "Audiobooks",
      query: "",
      type: null,
      rule: { extensions: ["m4b"], minDurationSecs: null },
    },
    {
      name: "Music",
      query: "",
      type: null,
      rule: {
        extensions: ["mp3", "aac", "flac", "ogg", "oga", "opus", "wav", "m4a", "aif", "aiff"],
        minDurationSecs: 30,
      },
    },
    {
      name: "Movies",
      query: "",
      type: null,
      rule: {
        extensions: ["mkv", "mp4", "avi", "mov", "m4v", "webm", "mpg", "mpeg", "wmv"],
        minDurationSecs: 70 * 60,
      },
    },
    {
      name: "Documents",
      query: "",
      type: null,
      rule: {
        extensions: ["doc", "docx", "odt", "ods", "odp", "pdf", "rtf", "xls", "xlsx", "ppt", "pptx"],
        minDurationSecs: null,
      },
    },
  ];

  private static loadSmartFolders(): SmartFolder[] {
    let list: SmartFolder[] = [];
    try {
      const raw = localStorage.getItem("ff-smart-folders");
      if (raw) {
        const parsed = JSON.parse(raw) as SmartFolder[];
        if (Array.isArray(parsed)) list = parsed;
      }
    } catch {
      // no stored folders
    }
    // Merge the prepopulated rule folders exactly once; if he deletes
    // them they stay deleted.
    let seeded = false;
    try { seeded = !!localStorage.getItem("ff-smart-v55"); } catch { seeded = true; }
    if (!seeded) {
      for (const d of Shell.DEFAULT_RULE_FOLDERS) {
        if (!list.some((f) => f.name === d.name)) list.push({ ...d });
      }
      try { localStorage.setItem("ff-smart-v55", "1"); } catch { /* ignore */ }
      Shell.saveSmartFolders(list);
    }
    try {
      if (!localStorage.getItem("ff-smart-v56")) {
        if (!list.some((f) => f.name === "Spreadsheets")) {
          list.push({ name: "Spreadsheets", query: "", type: null, rule: { extensions: ["csv", "tsv", "xls", "xlsx", "ods", "numbers"] } });
          Shell.saveSmartFolders(list);
        }
        localStorage.setItem("ff-smart-v56", "1");
      }
    } catch { /* ignore */ }
    return list;
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

  // ---- Phase 6: plugins + folder templates ----

  /** Menu snapshots (sync by FileViewHooks design): runnable
   *  plugins (valid + enabled + wired) and folder templates.
   *  Refreshed at boot and after every run/apply. */
  private pluginCache: { key: string; name: string }[] = [];
  private templateCache: { id: string; name: string }[] = [];

  private async refreshPluginTemplateCaches(): Promise<void> {
    try {
      const plugins = await api.listPlugins();
      this.pluginCache = plugins
        .filter((p) => p.valid && p.enabled && p.wired)
        .map((p) => ({ key: p.key, name: p.name }));
    } catch {
      /* keep the last good snapshot */
    }
    try {
      const templates = await api.listFolderTemplates();
      this.templateCache = templates.map((t) => ({ id: t.id, name: t.name }));
    } catch {
      /* keep the last good snapshot */
    }
  }

  /** Run a plugin on one item and show what it said / proposed.
   *  A proposal becomes a job only when the user confirms it in
   *  the result dialog — never from here. */
  private async runPluginFlow(
    item: import("./api").Item,
    key: string,
  ): Promise<void> {
    const path =
      item.provider_key ?? (await api.getItemPath(item.id).catch(() => null));
    if (!path) {
      Toast.error("That item has no local path to run a plugin on.");
      return;
    }
    const known = this.pluginCache.find((p) => p.key === key);
    try {
      const run = await api.runPluginAction(key, path);
      showPluginRunDialog(
        run.proposals[0]?.pluginName ?? known?.name ?? "Plugin",
        run,
      );
    } catch (err) {
      Toast.error(`Plugin failed: ${errText(err)}`);
    }
    void this.refreshPluginTemplateCaches();
  }

  /** Background menu → New Folder from Template: name it, then the
   *  backend plans it as a journaled job. */
  private async newFolderFromTemplateFlow(templateId: string): Promise<void> {
    const tab = this.activeTab;
    if (!tab) return;
    const dir = currentDir(tab);
    if (
      dir.dirId.startsWith("label:") ||
      dir.dirId.startsWith("archive:") ||
      dir.dirId.startsWith("collection:")
    ) {
      Toast.error("Can't create a folder here — this is a virtual view.");
      return;
    }
    const parentPath = await api.getItemPath(dir.dirId).catch(() => "");
    if (!parentPath) {
      Toast.error("Couldn't find this folder on disk.");
      return;
    }
    const template = this.templateCache.find((t) => t.id === templateId);
    const name = await promptDialog(
      "New Folder from Template",
      "Folder name",
      template?.name ?? "Untitled Folder",
    );
    if (!name) return;
    try {
      const start = await api.applyFolderTemplate(templateId, parentPath, name);
      if (start.alreadyRunning) {
        Toast.show("That job is already running.");
      } else {
        Toast.success(`Creating \u201c${name}\u201d from the template — watch it in Jobs.`);
      }
      // Template jobs are small; refresh shortly so the new folder
      // appears without a manual reload.
      setTimeout(() => this.refreshActiveDir(), 1500);
    } catch (err) {
      Toast.error(`Couldn't apply the template: ${errText(err)}`);
    }
    void this.refreshPluginTemplateCaches();
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
      Toast.error(`Create folder failed: ${errText(err)}`);
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
      Toast.error(`Create file failed: ${errText(err)}`);
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

  /** Effective collapse state for a sidebar section (Round 7.2): the
   * durable backend prefs win; localStorage is only the pre-paint
   * fast path the layout build reads before boot finishes. */
  private prefCollapsed(key: string, dflt = false): boolean {
    const v = this.uiPrefs[key];
    if (typeof v === "boolean") return v;
    try {
      const raw = localStorage.getItem(key);
      if (raw !== null) return raw === "1";
    } catch {
      return dflt;
    }
    return dflt;
  }

  /** The Roms Collection section COLLAPSES BY DEFAULT (Dickles,
   * 2026-10-14: the full list is overwhelming). Only an explicitly
   * saved state — his own toggle — overrides the default. */
  private romsCollapsedPref(): boolean {
    return this.prefCollapsed("ff-roms-collapsed", true);
  }

  /** Persist a sidebar collapse toggle to BOTH stores: localStorage
   * for the next pre-paint read, the backend sidecar for durability
   * (localStorage alone lost this state across sessions in 7.1 QA). */
  private setCollapsedPref(key: string, hidden: boolean): void {
    this.uiPrefs[key] = hidden;
    try {
      localStorage.setItem(key, hidden ? "1" : "0");
    } catch {
      // fast path only — the sidecar below is the durable copy
    }
    void api.setUiPrefs({ ...this.uiPrefs }).catch(() => undefined);
  }

  /** Re-apply collapse prefs to the built sidebar sections (called
   * from boot once the durable prefs have loaded). */
  private applyCollapsePrefs(): void {
    const labels = this.prefCollapsed("ff-labels-collapsed");
    this.labelList.hidden = labels;
    if (this.labelChevEl) {
      this.labelChevEl.style.transform = labels ? "rotate(-90deg)" : "";
    }
    const roms = this.romsCollapsedPref();
    this.romsList.hidden = roms;
    if (this.romsChevEl) {
      this.romsChevEl.style.transform = roms ? "rotate(-90deg)" : "";
    }
    const collections = this.prefCollapsed("ff-collections-collapsed");
    if (this.collectionList) this.collectionList.hidden = collections;
    if (this.collectionChevEl) {
      this.collectionChevEl.style.transform = collections
        ? "rotate(-90deg)"
        : "";
    }
  }

  /** Sidebar storage-gauge cache (UI round item 7): statvfs
   * numbers by path, 60 s TTL — buildSidebar runs on ~18 paths and
   * get_disk_info shells out to findmnt, so never pay it per
   * rebuild. */
  private diskGaugeCache = new Map<
    string,
    { at: number; total: number; free: number; mount: string }
  >();

  /** Live sidebar gauge elements by path. buildSidebar replaces
   * the sidebar DOM on every rebuild (and rebuilds fire from dozens
   * of triggers — boot burst, navigation, job/index events), so a
   * paint that resolves after a rebuild must land on the CURRENT
   * elements for the path. Painting only the captured pair was the
   * gray-bar bug (Dickles, 2026-10-15): orphaned paints left fresh
   * rows at width 0 — gray at rest, blue only when a click opened a
   * quiet window, gone again at the next rebuild burst. */
  private gaugeEls = new Map<
    string,
    { gauge: HTMLElement; fill: HTMLElement }
  >();

  /** Apply a gauge reading to the live elements for `path`.
   * COLOR LAW (Dickles): alarm red is reserved for the BOOT drive
   * running low — a nearly-full media drive is a victory, never red. */
  private paintGaugeNow(
    path: string,
    total: number,
    free: number,
    mount: string,
  ): void {
    const els = this.gaugeEls.get(path);
    if (!els || !els.gauge.isConnected) return;
    const usedPct = Math.max(0, Math.min(100, (1 - free / total) * 100));
    els.fill.style.width = `${usedPct}%`;
    const bootLow = mount === "/" && free / total < 0.1;
    els.gauge.classList.toggle("alarm", bootLow);
    els.gauge.title = `${formatBytes(total - free)} used · ${formatBytes(free)} free`;
  }

  /** Repaint every registered gauge from the cache, synchronously.
   * Runs at the end of buildSidebar so the rows it just built are
   * never left unpainted waiting on a fetch. */
  private repaintGaugesFromCache(): void {
    for (const [path, hit] of this.diskGaugeCache) {
      this.paintGaugeNow(path, hit.total, hit.free, hit.mount);
    }
  }

  /** Paint one sidebar location's storage gauge: a slim used bar
   * under the name. Registers the elements, paints instantly from a
   * warm cache, otherwise fetches — and when the fetch resolves it
   * paints whatever elements are live for the path THEN, not the
   * possibly-detached pair captured here. */
  private async paintLocGauge(
    path: string,
    gauge: HTMLElement,
    fill: HTMLElement,
  ): Promise<void> {
    this.gaugeEls.set(path, { gauge, fill });
    const cached = this.diskGaugeCache.get(path);
    if (cached && Date.now() - cached.at <= 60_000) {
      this.paintGaugeNow(path, cached.total, cached.free, cached.mount);
      return;
    }
    try {
      const info = await api.getDiskInfo(path);
      if (info.total_bytes <= 0) {
        // No gauge beats a wrong one — remove only the LIVE gauge;
        // a stale captured element is already gone.
        const els = this.gaugeEls.get(path);
        if (els && els.gauge === gauge) els.gauge.remove();
        return;
      }
      const hit = {
        at: Date.now(),
        total: info.total_bytes,
        free: info.free_bytes,
        mount: info.mount_point,
      };
      this.diskGaugeCache.set(path, hit);
      this.paintGaugeNow(path, hit.total, hit.free, hit.mount);
    } catch {
      // A failed fetch keeps any cached paint; only a never-painted
      // live gauge is removed (no gauge beats a wrong one).
      if (!this.diskGaugeCache.has(path)) {
        const els = this.gaugeEls.get(path);
        if (els && els.gauge === gauge) els.gauge.remove();
      }
    }
  }

  /** Disk Properties (inc-2 item 12): identity + space of the
   * volume backing a sidebar location or favorite. Free space is
   * the headline figure; numbers are statvfs (df-equivalent). */
  private async showDiskProperties(
    name: string,
    path: string,
  ): Promise<void> {
    const overlay = document.createElement("div");
    overlay.className = "confirm-overlay";
    const card = document.createElement("div");
    card.className = "confirm-card disk-card";
    const title = document.createElement("h3");
    title.textContent = `💽 ${name}`;
    const body = document.createElement("div");
    body.className = "disk-body";
    body.textContent = "Reading disk…";
    const actions = document.createElement("div");
    actions.className = "confirm-actions";
    const closeBtn = document.createElement("button");
    closeBtn.className = "pill-btn primary";
    closeBtn.textContent = "Close";
    const dismiss = () => overlay.remove();
    closeBtn.addEventListener("click", dismiss);
    overlay.addEventListener("click", (e) => {
      if (e.target === overlay) dismiss();
    });
    actions.appendChild(closeBtn);
    card.append(title, body, actions);
    overlay.appendChild(card);
    document.body.appendChild(overlay);
    closeBtn.focus();
    try {
      const info = await api.getDiskInfo(path);
      body.textContent = "";
      const freeBig = document.createElement("div");
      freeBig.className = "disk-free-big";
      freeBig.textContent = `${formatBytes(info.free_bytes)} free`;
      const pct = document.createElement("div");
      pct.className = "disk-free-pct";
      pct.textContent = `${info.percent_free.toFixed(1)}% of this volume is free`;
      const barTrack = document.createElement("div");
      barTrack.className = "disk-bar";
      const fill = document.createElement("div");
      fill.className = "disk-bar-fill";
      const usedPct =
        info.total_bytes > 0
          ? (info.used_bytes / info.total_bytes) * 100
          : 0;
      fill.style.width = `${Math.min(100, Math.max(0, usedPct))}%`;
      barTrack.appendChild(fill);
      body.append(freeBig, pct, barTrack);
      const rows: Array<[string, string]> = [
        ["Mount point", info.mount_point],
        ["Filesystem", info.fstype ?? "—"],
        ["Device", info.source ?? "—"],
        ["Total", formatBytes(info.total_bytes)],
        ["Used", formatBytes(info.used_bytes)],
        ["Free", formatBytes(info.free_bytes)],
      ];
      for (const [k, v] of rows) {
        const row = document.createElement("div");
        row.className = "disk-row";
        const kk = document.createElement("span");
        kk.className = "disk-key";
        kk.textContent = k;
        const vv = document.createElement("span");
        vv.className = "disk-val";
        vv.textContent = v;
        row.append(kk, vv);
        body.appendChild(row);
      }
    } catch (err) {
      body.textContent = `Could not read disk info: ${errText(err)}`;
    }
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
      Toast.error(`Mount failed: ${errText(err)}`);
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

  /** Media-server Organize on the folder being browsed (hamburger →
   * Organize): the Resolve dialog scoped to this folder, whose
   * Preview Organize step files resolved movies/episodes into the
   * server layout as a journaled job. */
  private async openMediaOrganizeHere(): Promise<void> {
    const path = (await this.currentDirPath()) || (await this.homePath());
    if (!path) {
      Toast.error("Open a folder first, then organize its movies and shows.");
      return;
    }
    void this.mediaRename.openFolder(path, () => {
      this.refreshActiveDir();
      if (this.searchView.active) this.searchView.refresh();
    });
  }

  /** ROM Organize on the folder being browsed (hamburger →
   * Organize): identified games filed by system + DAT title, as a
   * journaled job, after a full preview. */
  private async openRomOrganizeHere(): Promise<void> {
    const path = (await this.currentDirPath()) || (await this.homePath());
    if (!path) {
      Toast.error("Open a folder first, then organize its games.");
      return;
    }
    void this.romOrganize.openFolder(path, () => this.refreshActiveDir());
  }

  /** Music Completion on the folder being browsed (hamburger →
   * Organize). */
  private async openMusicHere(): Promise<void> {
    const path = (await this.currentDirPath()) || (await this.homePath());
    if (!path) {
      Toast.error("Open a folder first, then complete its music.");
      return;
    }
    void this.musicComplete.openFolder(path, () => this.refreshActiveDir());
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

  /** Connect to Server (Finder): the Network Locations dialog —
   * GIO mounts the server (smb/sftp/ftp/ftps/dav/davs/nfs/afp), the
   * bookmark lands in Locations, and we navigate into it. Prefill
   * comes from an offline bookmark row's click. */
  private connectToServer(prefill?: { uri?: string }): void {
    openConnectDialog({ initialUri: prefill?.uri }, (summary) => {
      void (async () => {
        await this.refreshLocations();
        this.navigateTo({
          locationId: summary.id,
          dirId: summary.root_item,
          crumbs: [{ dirId: summary.root_item, name: summary.display_name }],
        });
      })();
    });
  }

  /** Disconnect a server (Network Locations): the bookmark stays in
   * Locations in its offline state — files still badged in search —
   * until Forget Server removes it. */
  private async disconnectServer(loc: LocationSummary): Promise<void> {
    try {
      await api.netDisconnect(loc.id);
      Toast.success(
        `Disconnected ${loc.display_name}. It stays in Locations — click it to reconnect.`,
      );
    } catch (err) {
      Toast.error(`Disconnect failed: ${errText(err)}`);
    }
    await this.refreshLocations();
  }

  /** Forget a server entirely: disconnect + bookmark + saved
   * password + indexed file list all go. Server files untouched. */
  private async forgetServer(loc: LocationSummary): Promise<void> {
    const ok = await confirmDialog(
      `Forget ${loc.display_name}?`,
      "Fable will disconnect it, remove it from Locations, forget its saved password, and drop its indexed file list. The files on the server aren't touched.",
      "Forget Server",
      true,
    );
    if (!ok) return;
    try {
      await api.netForget(loc.id);
      Toast.success(`Forgot ${loc.display_name}.`);
    } catch (err) {
      Toast.error(`Forget failed: ${errText(err)}`);
    }
    await this.refreshLocations();
  }

  // ------------------------------------------------------------- breadcrumbs

  private renderCrumbs(): void {
    this.crumbBar.innerHTML = "";
    const tab = this.activeTab;
    if (!tab) return;
    // Round 7.1: while a smart folder / search view is up, the path bar
    // shows that view as a single static crumb — the previous folder's
    // breadcrumb lingered and no longer described what was on screen.
    if (this.searchView.active) {
      const s = document.createElement("span");
      s.className = "crumb current";
      s.textContent =
        this.searchView.titleHint ??
        (this.searchView.currentQuery
          ? `Search: ${this.searchView.currentQuery}`
          : "Search");
      this.crumbBar.appendChild(s);
      return;
    }
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

  // ------------------------------------------------ favorites (inc-2 item 14)

  /** User-pinned favorites, durable in the backend sidecar prefs
   * with a localStorage mirror — the same dual-store pattern as the
   * collapse prefs (localStorage alone lost state in 7.1 QA). */
  private loadUserFavorites(): { name: string; path: string }[] {
    const raw = this.uiPrefs["ff-user-favorites"];
    if (Array.isArray(raw)) {
      return raw.filter(
        (f): f is { name: string; path: string } =>
          !!f &&
          typeof f === "object" &&
          typeof (f as { name?: unknown }).name === "string" &&
          typeof (f as { path?: unknown }).path === "string",
      );
    }
    try {
      const ls = localStorage.getItem("ff-user-favorites");
      if (ls) return JSON.parse(ls) as { name: string; path: string }[];
    } catch {
      // no mirror — start empty
    }
    return [];
  }

  private saveUserFavorites(favs: { name: string; path: string }[]): void {
    this.uiPrefs["ff-user-favorites"] = favs;
    try {
      localStorage.setItem("ff-user-favorites", JSON.stringify(favs));
    } catch {
      // mirror only — the sidecar is the durable copy
    }
    void api.setUiPrefs({ ...this.uiPrefs }).catch(() => undefined);
  }

  /** Built-in favorites the user removed. They stay restorable:
   * the context-menu entry is state-aware in both directions. */
  private loadHiddenFavorites(): string[] {
    const raw = this.uiPrefs["ff-fav-hidden"];
    if (Array.isArray(raw)) {
      return raw.filter((x): x is string => typeof x === "string");
    }
    try {
      const ls = localStorage.getItem("ff-fav-hidden");
      if (ls) return JSON.parse(ls) as string[];
    } catch {
      // no mirror — start empty
    }
    return [];
  }

  private saveHiddenFavorites(names: string[]): void {
    this.uiPrefs["ff-fav-hidden"] = names;
    try {
      localStorage.setItem("ff-fav-hidden", JSON.stringify(names));
    } catch {
      // mirror only — the sidecar is the durable copy
    }
    void api.setUiPrefs({ ...this.uiPrefs }).catch(() => undefined);
  }

  /** Live favorite state for the item context menu. */
  private isFavoriteItem(item: Item): boolean {
    if (!item.provider_key) return false;
    return this.favoritePathSet.has(normFavPath(item.provider_key));
  }

  /** Pin a folder to Favorites (shared by the drag door and the
   * context-menu door). Already-favorite folders never duplicate. */
  private async addFavoriteFromItem(item: Item): Promise<void> {
    const path =
      item.provider_key ??
      (await api.getItemPath(item.id).catch(() => ""));
    if (!path) return;
    const n = normFavPath(path);
    if (this.favoritePathSet.has(n)) {
      Toast.success(`“${item.name}” is already in Favorites.`);
      return;
    }
    const builtin = this.builtinFavNames.get(n);
    if (builtin) {
      // A built-in the user had removed: pinning restores it.
      if (this.loadHiddenFavorites().includes(builtin)) {
        this.saveHiddenFavorites(
          this.loadHiddenFavorites().filter((h) => h !== builtin),
        );
        Toast.success(`Added “${item.name}” to Favorites.`);
        void this.buildSidebar();
      }
      return;
    }
    const favs = this.loadUserFavorites();
    if (!favs.some((f) => normFavPath(f.path) === n)) {
      favs.push({ name: item.name, path: n });
      this.saveUserFavorites(favs);
    }
    Toast.success(`Added “${item.name}” to Favorites.`);
    void this.buildSidebar();
  }

  /** The context menu's Add/Remove door: state read live at open. */
  private async toggleFavorite(item: Item): Promise<void> {
    const path =
      item.provider_key ??
      (await api.getItemPath(item.id).catch(() => ""));
    if (!path) return;
    const n = normFavPath(path);
    const favs = this.loadUserFavorites();
    const idx = favs.findIndex((f) => normFavPath(f.path) === n);
    if (idx >= 0) {
      favs.splice(idx, 1);
      this.saveUserFavorites(favs);
      Toast.success(`Removed “${item.name}” from Favorites.`);
      void this.buildSidebar();
      return;
    }
    const builtin = this.builtinFavNames.get(n);
    if (builtin) {
      const hidden = this.loadHiddenFavorites();
      if (hidden.includes(builtin)) {
        this.saveHiddenFavorites(hidden.filter((h) => h !== builtin));
        Toast.success(`Added “${item.name}” to Favorites.`);
      } else {
        this.saveHiddenFavorites([...hidden, builtin]);
        Toast.success(`Removed “${item.name}” from Favorites.`);
      }
      void this.buildSidebar();
      return;
    }
    await this.addFavoriteFromItem(item);
  }

  private removeUserFavorite(path: string): void {
    const n = normFavPath(path);
    this.saveUserFavorites(
      this.loadUserFavorites().filter((f) => normFavPath(f.path) !== n),
    );
    void this.buildSidebar();
  }

  /** Resolve an arbitrary folder path to a browsable target: the
   * location whose root contains it, then a segment walk — the
   * same resolution open-in-new-tab uses. Null when no mounted
   * location contains the path. */
  private async resolvePathTarget(
    path: string,
  ): Promise<FavoriteTarget | null> {
    const n = normFavPath(path);
    for (const loc of this.locations) {
      const root = await api.getItemPath(loc.root_item).catch(() => "");
      if (!root) continue;
      const rn = normFavPath(root);
      if (n === rn) {
        return {
          locationId: loc.id,
          dirId: loc.root_item,
          display: loc.display_name,
        };
      }
      const prefix = rn === "/" ? "/" : `${rn}/`;
      if (!n.startsWith(prefix)) continue;
      const rel = n.slice(prefix.length).split("/").filter(Boolean);
      let dirId = loc.root_item;
      let ok = true;
      for (const seg of rel) {
        const kids = await api.listDir(loc.id, dirId).catch(() => null);
        const kid = kids?.find((k) => k.kind === "folder" && k.name === seg);
        if (!kid) {
          ok = false;
          break;
        }
        dirId = kid.id;
      }
      if (ok) {
        return {
          locationId: loc.id,
          dirId,
          display: rel[rel.length - 1] ?? loc.display_name,
        };
      }
    }
    return null;
  }

  /** Drop an item onto a user-favorite folder: a journaled Move job
   * with outcome-fed undo — the same behavior as dropping onto a
   * built-in favorite. */
  /** Open a sidebar favorite in a fresh tab (fix block item 9). */
  private openFavoriteInNewTab(target: FavoriteTarget, name: string): void {
    const loc =
      this.locations.find((l) => l.id === target.locationId) ??
      this.locations[0];
    if (!loc) return;
    const tab = this.newTab(loc);
    if (target.dirId !== loc.root_item) {
      navigate(tab, {
        locationId: target.locationId,
        dirId: target.dirId,
        crumbs: [{ dirId: target.dirId, name }],
      });
      tab.title = name;
    }
    void this.loadDir(tab);
  }

  /** The effective payload of the in-flight drag: the whole
   * selection when the grabbed item belongs to one (dragState.items,
   * set by FileView with the context menu's targets rule), else the
   * grabbed item alone. Drop targets act on THIS, never on the single
   * item under the cursor (multi-trash fix, 2026-10-16). */
  private draggedItems(grabbed: Item): Item[] {
    const items = dragState.items;
    return items && items.length > 0 ? items : [grabbed];
  }

  /** File a dropped selection into a folder as ONE job (move, or
   * copy with Ctrl). Items already inside the destination are left
   * where they are — only the rest travel. */
  private async fileDraggedIntoFolder(
    draggedItems: Item[],
    target: FavoriteTarget,
    label: string,
    copy = false,
  ): Promise<void> {
    const what =
      draggedItems.length === 1
        ? `“${draggedItems[0].name}”`
        : `${draggedItems.length} items`;
    try {
      const dest = await api.getItemPath(target.dirId);
      if (!dest) return;
      const items = draggedItems.filter(
        (i) => !i.provider_key || dirname(i.provider_key) !== dest,
      );
      if (items.length === 0) return; // everything already lives there
      if (copy) {
        // Ctrl+drop = copy (fix block item 2): the destination parent
        // is the folder's own item id, which is what start_copy wants.
        const start = await api.startCopy(
          items.map((i) => i.id),
          target.dirId,
        );
        const jobId = this.acceptJobStart(start, "copy");
        if (!jobId) return;
        this.trackTransfer(
          jobId,
          items.length === 1 ? `Copy ${items[0].name}` : `Copy ${items.length} items`,
          async (error) => {
            if (error) {
              Toast.error(`Could not copy ${what}: ${errText(error)}`);
            } else {
              Toast.success(`Copied ${what} to ${label}.`);
            }
            const tab = this.activeTab;
            if (tab) await this.loadDir(tab);
          },
        );
        return;
      }
      const start = await api.startMove(
        items.map((i) => i.id),
        dest,
      );
      const jobId = this.acceptJobStart(start, "move");
      if (!jobId) return;
      const nameByPath = new Map(
        items.map((i) => [i.provider_key ?? "", i.name] as [string, string]),
      );
      this.trackTransfer(
        jobId,
        items.length === 1 ? `Move ${items[0].name}` : `Move ${items.length} items`,
        async (error) => {
          if (error) {
            Toast.error(`Could not move ${what}: ${errText(error)}`);
          } else {
            const outcomes = await this.jobOutcomes(jobId);
            for (const o of outcomes) {
              if (!o.result_path) continue;
              const fromDir = dirname(o.src_path);
              if (fromDir) {
                this.undoStack.push({
                  kind: "move",
                  newPath: o.result_path,
                  fromDir,
                  label: nameByPath.get(o.src_path) ?? o.src_path,
                });
              }
            }
            Toast.success(`Moved ${what} to ${label}. Super+Z to undo.`);
          }
          this.quickLook.close();
          const tab = this.activeTab;
          if (tab) await this.loadDir(tab);
        },
      );
    } catch (err) {
      Toast.error(`Could not move ${what}: ${errText(err)}`);
    }
  }

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
    const hiddenFavs = this.loadHiddenFavorites();
    const allPairs = names
      .map((n, i) => ({ name: n, target: targets[i] }))
      .filter((p) => p.target);
    const pairs = allPairs
      .filter((p) => !hiddenFavs.includes(p.name))
      .sort((a, b) => orderIdx(a.name) - orderIdx(b.name));
    // Favorite identity by path (inc-2 item 14): the item context
    // menu reads its Add/Remove state from these, rebuilt each pass.
    this.builtinFavNames.clear();
    this.favoritePathSet.clear();
    for (const p of allPairs) {
      const path = await api.getItemPath(p.target!.dirId).catch(() => "");
      if (path) this.builtinFavNames.set(normFavPath(path), p.name);
    }
    for (const p of pairs) {
      const path = await api.getItemPath(p.target!.dirId).catch(() => "");
      if (path) this.favoritePathSet.add(normFavPath(path));
    }
    for (const pair of pairs) {
      const name = pair.name;
      const target = pair.target!;
      const b = document.createElement("button");
      b.className = "side-item";
      const favLabel = document.createElement("span");
      favLabel.textContent = name;
      b.append(sectionGlyph(favoriteIcon(name)), favLabel);
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
        // Right-click: Disk Properties for the volume behind the
        // favorite (inc-2 item 12). Trash has no volume of its own.
        if (name !== "Trash") {
          b.addEventListener("contextmenu", (e) => {
            e.preventDefault();
            e.stopPropagation();
            void (async () => {
              const p = await api
                .getItemPath(target.dirId)
                .catch(() => "");
              if (!p) return;
              this.ctxMenu.open(e.clientX, e.clientY, [
                {
                  label: "Open in New Tab",
                  action: () => this.openFavoriteInNewTab(target, name),
                },
                {
                  label: "Disk Properties…",
                  action: () => void this.showDiskProperties(name, p),
                },
              ]);
            })();
          });
        }
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
          if (e.dataTransfer) e.dataTransfer.dropEffect = e.ctrlKey ? "copy" : "move";
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
          // The drop acts on the drag's effective payload — the whole
          // selection when the grabbed item belongs to one — never
          // just the item under the cursor (multi-trash fix,
          // 2026-10-16; this handler used to startDelete([dragged.id])).
          const items = this.draggedItems(dragged);
          if (name === "Trash") {
            void this.trashItems(items);
          } else {
            // Ctrl+drop = copy into the favorite (fix block item 2).
            void this.fileDraggedIntoFolder(items, target, name, e.ctrlKey);
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
                danger: true,
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
                      Toast.error(`Failed to empty trash: ${errText(err)}`);
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
                      Toast.error(`Failed to secure-delete: ${errText(err)}`);
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
    // User-pinned favorites (inc-2 item 14): folders pinned by drag
    // or by the item context menu. One whose folder can't be resolved
    // right now (drive not mounted) is OMITTED, never shown disabled
    // — the same rule as the built-ins above.
    for (const fav of this.loadUserFavorites()) {
      const target = await this.resolvePathTarget(fav.path);
      if (!target) continue;
      this.favoritePathSet.add(normFavPath(fav.path));
      const b = document.createElement("button");
      b.className = "side-item";
      const pinLabel = document.createElement("span");
      pinLabel.textContent = fav.name;
      b.append(sectionGlyph("folder"), pinLabel);
      b.title = fav.path;
      b.addEventListener("click", () => {
        this.navigateTo({
          locationId: target.locationId,
          dirId: target.dirId,
          crumbs: [{ dirId: target.dirId, name: fav.name }],
        });
      });
      b.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        e.stopPropagation();
        this.ctxMenu.open(e.clientX, e.clientY, [
          {
            label: "Open in New Tab",
            action: () => this.openFavoriteInNewTab(target, fav.name),
          },
          {
            label: "Disk Properties…",
            action: () => void this.showDiskProperties(fav.name, fav.path),
          },
          {
            label: "Remove from Favorites",
            action: () => this.removeUserFavorite(fav.path),
          },
        ]);
      });
      // File-into drop parity with the built-in favorites.
      b.addEventListener("dragover", (e) => {
        if (dragState.favName || !dragState.item) return;
        e.preventDefault();
        if (e.dataTransfer) e.dataTransfer.dropEffect = e.ctrlKey ? "copy" : "move";
        b.classList.add("drop-hover");
      });
      b.addEventListener("dragleave", () => b.classList.remove("drop-hover"));
      b.addEventListener("drop", (e) => {
        e.preventDefault();
        b.classList.remove("drop-hover");
        const dragged = dragState.item;
        if (!dragged || dragState.favName) return;
        void this.fileDraggedIntoFolder(
          this.draggedItems(dragged),
          target,
          fav.name,
          e.ctrlKey,
        );
      });
      this.favList.appendChild(b);
    }
    for (const loc of this.locations) {
      const b = document.createElement("button");
      const isOfflineServer = loc.provider === "network" && !loc.online;
      b.className = isOfflineServer
        ? "side-item side-loc offline"
        : "side-item side-loc";
      const name = document.createElement("span");
      name.textContent = loc.display_name;
      // Name + storage gauge stack (UI round item 7): a slim
      // gas-tank bar under the name for locations backed by a real
      // path — used/free at a glance, Disk Properties data path.
      const main = document.createElement("span");
      main.className = "side-loc-main";
      main.append(name);
      if (loc.path) {
        const gauge = document.createElement("span");
        gauge.className = "side-gauge";
        const fill = document.createElement("i");
        fill.className = "side-gauge-fill";
        gauge.appendChild(fill);
        main.appendChild(gauge);
        void this.paintLocGauge(loc.path, gauge, fill);
      }
      const prov = document.createElement("span");
      prov.className = "side-provider";
      prov.textContent = loc.provider;
      if (isOfflineServer) {
        // Plain state, not jargon: the server is saved but not
        // connected right now (Library Memory keeps it listed).
        prov.className = "side-offline-tag";
        prov.textContent = "not connected";
      }
      b.append(main, prov);
      // Removable volumes and devices get an inline eject (Round
      // 7.2), the way Nautilus renders it — the right-click Unmount
      // stays as well. Devices eject by GIO root URI: udisks (the
      // volume path) cannot see a GVfs mount.
      if (
        (loc.provider === "volume" || loc.provider === "device") &&
        loc.removable &&
        loc.path
      ) {
        const eject = document.createElement("span");
        eject.className = "side-eject";
        eject.textContent = "⏏";
        eject.title = `Eject ${loc.display_name}`;
        eject.setAttribute("role", "button");
        eject.setAttribute("aria-label", `Eject ${loc.display_name}`);
        eject.addEventListener("click", (e) => {
          e.stopPropagation();
          e.preventDefault();
          void (async () => {
            try {
              if (loc.provider === "device" && loc.uri) {
                await api.unmountDevice(loc.uri);
              } else {
                await api.unmountPath(loc.path!);
              }
              Toast.success(`Ejected ${loc.display_name}.`);
            } catch (err) {
              Toast.error(`Eject failed: ${errText(err)}`);
            }
            await this.refreshLocations();
          })();
        });
        b.appendChild(eject);
      }
      b.addEventListener("click", () => {
        if (isOfflineServer) {
          // Clicking a saved-but-offline server CONNECTS it (the
          // dialog opens prefilled) — never a dead click on an
          // unregistered root.
          this.connectToServer({ uri: loc.uri ?? undefined });
          return;
        }
        this.navigateTo({
          locationId: loc.id,
          dirId: loc.root_item,
          crumbs: [{ dirId: loc.root_item, name: loc.display_name }],
        });
      });
      b.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        e.stopPropagation();
        if (loc.provider === "network") {
          const items: MenuItem[] = [];
          if (loc.online) {
            items.push({
              label: "Open in New Tab",
              action: () => {
                const t = this.newTab(loc);
                void this.loadDir(t);
              },
            });
            items.push({
              label: "Disconnect",
              action: () => void this.disconnectServer(loc),
            });
          } else {
            items.push({
              label: "Connect…",
              action: () => this.connectToServer({ uri: loc.uri ?? undefined }),
            });
          }
          items.push({
            label: "Forget Server…",
            danger: true,
            action: () => void this.forgetServer(loc),
          });
          this.ctxMenu.open(e.clientX, e.clientY, items);
          return;
        }
        if (loc.provider === "device") {
          // Devices get their own short menu: the generic disk menu
          // below drives udisks, which cannot see a GVfs mount.
          const items: MenuItem[] = [];
          items.push({
            label: "Open in New Tab",
            action: () => {
              const t = this.newTab(loc);
              void this.loadDir(t);
            },
          });
          if (loc.uri) {
            const uri = loc.uri;
            items.push({
              label: "Eject",
              action: () => {
                void api
                  .unmountDevice(uri)
                  .then(async () => {
                    Toast.success(`Ejected ${loc.display_name}.`);
                    await this.refreshLocations();
                  })
                  .catch((err) =>
                    Toast.error(`Eject failed: ${errText(err)}`),
                  );
              },
            });
          }
          this.ctxMenu.open(e.clientX, e.clientY, items);
          return;
        }
        void (async () => {
          const rootPath = await api
            .getItemPath(loc.root_item)
            .catch(() => "");
          const menuItems: MenuItem[] = [];
          menuItems.push({
            label: "Open in New Tab",
            action: () => {
              const t = this.newTab(loc);
              void this.loadDir(t);
            },
          });
          if (rootPath) {
            menuItems.push({
              label: "Disk Properties…",
              action: () =>
                void this.showDiskProperties(loc.display_name, rootPath),
            });
          }
          menuItems.push({
            label: "Disk Utility…",
            action: () =>
              void this.diskDialog.open(rootPath || undefined),
          });
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
                  .catch((err) => Toast.error(`Unmount failed: ${errText(err)}`));
              },
            });
          }
          this.ctxMenu.open(e.clientX, e.clientY, menuItems);
        })();
      });
      // Drop target (fix block item 2): drag a file/folder onto a
      // location to move it there (Ctrl = copy), through the jobs
      // engine like the favorites.
      b.addEventListener("dragover", (e) => {
        if (dragState.favName || !dragState.item) return;
        if (isOfflineServer) return; // connect first, then drop
        e.preventDefault();
        if (e.dataTransfer) e.dataTransfer.dropEffect = e.ctrlKey ? "copy" : "move";
        b.classList.add("drop-hover");
      });
      b.addEventListener("dragleave", () => b.classList.remove("drop-hover"));
      b.addEventListener("drop", (e) => {
        e.preventDefault();
        b.classList.remove("drop-hover");
        const dragged = dragState.item;
        if (!dragged || dragState.favName) return;
        if (isOfflineServer) return;
        void this.fileDraggedIntoFolder(
          this.draggedItems(dragged),
          { locationId: loc.id, dirId: loc.root_item, display: loc.path ?? loc.display_name },
          loc.display_name,
          e.ctrlKey,
        );
      });
      this.sidebarList.appendChild(b);
    }
    if (this.locations.length === 0) {
      const p = document.createElement("p");
      p.className = "side-empty";
      p.textContent = "No locations yet.";
      this.sidebarList.appendChild(p);
    }
    // Gauges: repaint the just-built rows from the warm cache NOW —
    // a location bar must read blue at rest, not whenever a fetch
    // happens to win a race against the next rebuild.
    this.repaintGaugesFromCache();
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
            label: `Manage "${def.name}"…`,
            action: () =>
              this.labelManager.open(() => {
                void this.buildSidebar();
                this.refreshActiveDir();
              }),
          },
          {
            label: `Delete "${def.name}"`,
            danger: true,
            action: () => {
              void (async () => {
                const ok = await confirmDialog(
                  `Delete label "${def.name}"?`,
                  "The label is removed from every file that carries it. The files themselves are not touched.",
                  "Delete Label",
                  true,
                );
                if (!ok) return;
                await api.deletePathLabel(def.name).catch(() => undefined);
                saveLabelDefs(
                  loadLabelDefs().filter((d) => d.name !== def.name),
                );
                void this.buildSidebar();
                this.refreshActiveDir();
                Toast.success(`Label "${def.name}" deleted.`);
              })();
            },
          },
          { separator: true, label: "", action: () => {} },
          {
            label: "Delete All Labels\u2026",
            danger: true,
            action: () => void this.deleteAllLabels(),
          },
        ]);
      });
      this.labelList.appendChild(b);
    }
    // Phase 2: Collections — ★ Starred first, then manual
    // collections with member counts. Rows are drop doors.
    this.collectionList.innerHTML = "";
    {
      const b = document.createElement("button");
      b.className = "side-item";
      const label = document.createElement("span");
      label.textContent = "Starred";
      b.append(sectionGlyph("starred"), label);
      b.addEventListener("click", () => this.openStarred());
      this.collectionList.appendChild(b);
    }
    const collections = await api
      .listCollections()
      .catch(() => this.collectionsCache);
    this.collectionsCache = collections;
    const manual = collections.filter((c) => c.kind === "manual");
    if (manual.length === 0) {
      const p = document.createElement("p");
      p.className = "side-empty";
      p.textContent =
        "Group any files together — right-click a file, or this heading.";
      this.collectionList.appendChild(p);
    }
    for (const c of manual) {
      const b = document.createElement("button");
      b.className = "side-item";
      const glyph = sectionGlyph("folder");
      const label = document.createElement("span");
      label.textContent = c.name;
      b.append(glyph, label);
      if (c.member_count > 0) {
        const count = document.createElement("span");
        count.className = "side-count";
        count.textContent = String(c.member_count);
        b.appendChild(count);
      }
      if (c.missing_count > 0) {
        const miss = document.createElement("span");
        miss.className = "side-missing";
        miss.textContent = `${c.missing_count} missing`;
        miss.title =
          "These members' files moved or were deleted outside Fable.";
        b.appendChild(miss);
      }
      b.addEventListener("click", () => this.openCollection(c.id, c.name));
      b.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        e.stopPropagation();
        const entries: Parameters<typeof this.ctxMenu.open>[2] = [
          {
            label: `Rename “${c.name}”…`,
            action: () => void this.renameCollectionFlow(c),
          },
        ];
        if (c.missing_count > 0) {
          entries.push({
            label: `Remove ${c.missing_count} missing entr${c.missing_count === 1 ? "y" : "ies"}`,
            action: () => void this.cleanCollectionMissing(c),
          });
        }
        entries.push(
          { separator: true, label: "", action: () => {} },
          {
            label: `Delete “${c.name}”`,
            danger: true,
            action: () => void this.deleteCollectionFlow(c),
          },
        );
        this.ctxMenu.open(e.clientX, e.clientY, entries);
      });
      // Drop door: drag a file or folder onto the row to add it.
      b.addEventListener("dragover", (e) => {
        if (!dragState.item) return;
        e.preventDefault();
        if (e.dataTransfer) e.dataTransfer.dropEffect = "copy";
        b.classList.add("drop-hover");
      });
      b.addEventListener("dragleave", () =>
        b.classList.remove("drop-hover"),
      );
      b.addEventListener("drop", (e) => {
        e.preventDefault();
        b.classList.remove("drop-hover");
        const it = dragState.item;
        if (it)
          void this.addItemsToCollection(this.draggedItems(it), c.id, c.name);
      });
      this.collectionList.appendChild(b);
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
      const sfLabel = document.createElement("span");
      sfLabel.textContent = sf.name;
      b.append(sectionGlyph(smartFolderIcon(sf)), sfLabel);
      b.title = sf.query || sf.type || "";
      b.addEventListener("click", () => {
        // The path bar names the view while it is up (Round 7.1);
        // runRule sets the hint itself, saved queries need it set here.
        this.searchView.titleHint = sf.name;
        if (sf.rule) void this.searchView.runRule(sf.name, sf.rule);
        else void this.searchView.runExternal(sf.query, sf.type);
      });
      b.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        e.stopPropagation();
        this.ctxMenu.open(e.clientX, e.clientY, [
          {
            label: `Manage "${sf.name}"\u2026`,
            action: () => this.openSmartEditor(sf),
          },
          {
            label: `Delete "${sf.name}"`,
            danger: true,
            action: () => {
              void (async () => {
                const ok = await confirmDialog(
                  `Delete "${sf.name}"?`,
                  "The smart folder is removed from the sidebar. Your files are not touched.",
                  "Delete",
                  true,
                );
                if (!ok) return;
                Shell.saveSmartFolders(
                  Shell.loadSmartFolders().filter((f) => f.name !== sf.name),
                );
                void this.buildSidebar();
                Toast.success(`Deleted smart folder "${sf.name}".`);
              })();
            },
          },
        ]);
      });
      this.smartList.appendChild(b);
    }
    this.renderRomsSection();
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

  /** Adjust UI font scale (0.7 to 2.0). UI round: hosted by the
   * Preferences screen now, so it persists in the durable sidecar
   * like the other preferences (was session-only before). */
  private setFontScale(scale: number): void {
    this.fontScale = Math.min(2.0, Math.max(0.7, Math.round(scale * 10) / 10));
    document.documentElement.style.setProperty("--ufm-font-scale", String(this.fontScale));
    // Drive the scale from the root font size so ALL rem-sized text
    // follows — sidebar, dialogs, and every file/folder name in every
    // view. (The old approach scaled only #app + two selectors, which
    // is why file labels ignored Ctrl +/-.)
    document.documentElement.style.fontSize = `${16 * this.fontScale}px`;
    this.uiPrefs["ff-font-scale"] = this.fontScale;
    void api.setUiPrefs({ ...this.uiPrefs }).catch(() => undefined);
  }

  /** Grid icon size in px (UI round item 3a, the Zorin/Nautilus
   * zoom stepper): one preference drives the grid's icon, photo
   * thumbnail and cell pitch through CSS vars. */
  private iconSize = 48;
  private static readonly ICON_STEPS = [32, 40, 48, 56, 64, 80, 96];

  private applyIconSize(): void {
    const px = this.iconSize;
    const root = document.documentElement.style;
    root.setProperty("--fv-icon", `${px}px`);
    root.setProperty("--fv-thumb", `${px + 8}px`);
    root.setProperty("--fv-cell", `${px + 56}px`);
  }

  private setIconSize(px: number): void {
    this.iconSize = Math.min(96, Math.max(32, Math.round(px)));
    this.applyIconSize();
    this.uiPrefs["ff-icon-size"] = this.iconSize;
    void api.setUiPrefs({ ...this.uiPrefs }).catch(() => undefined);
  }

  private stepIconSize(dir: 1 | -1): void {
    const steps = Shell.ICON_STEPS;
    const cur = steps.reduce(
      (best, s) => (Math.abs(s - this.iconSize) < Math.abs(best - this.iconSize) ? s : best),
      steps[0],
    );
    const next =
      steps[Math.min(steps.length - 1, Math.max(0, steps.indexOf(cur) + dir))];
    this.setIconSize(next);
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
    this.saveViewProfile();
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
      Toast.error(`Copy failed: ${errText(err)}`);
    }
  }

  /** The hamburger menu: views, sort, text size, theme, help. */
  /** Re-index every location (Round 7.1): files downloaded while the
   * app runs become visible to smart folders/search without a restart.
   * Progress rides the status bar via IndexTracker; completion refreshes
   * the visible smart folder/search view. */
  private reindexLibrary(): void {
    if (this.locations.length === 0) return;
    Toast.show("Re-indexing the library…", "info");
    // Offline server bookmarks can't be walked — Connect indexes them.
    // Devices stay out even here: a phone is never library memory.
    void this.indexTracker.reindexAll(
      this.locations
        .filter((l) => l.online && l.provider !== "device")
        .map((l) => l.id),
    );
  }

  /** File Auditor: the sidebar badge carries the open-flag count
   * (proactive, calm — a number, never a red alert). Refreshed when
   * the sidebar builds, when the panel closes, and whenever the
   * panel reports a change. */
  private async refreshAuditorBadge(): Promise<void> {
    try {
      const n = await api.auditorOpenCount();
      if (this.auditorBadgeEl) {
        this.auditorBadgeEl.textContent = n > 0 ? String(n) : "";
      }
    } catch {
      // The badge is a courtesy — a failed count stays blank.
    }
  }

  private openAuditor(): void {
    this.auditorPanel.open({
      pickFolder: async () => this.picker.pick("Choose a folder", [], null),
      refreshBadge: () => void this.refreshAuditorBadge(),
    });
  }

  /** Preferences (UI round item 1): the settings surface. Every
   * control writes through to the stores the rest of the app
   * already reads — the ui-prefs sidecar, the theme manager, the
   * ROM store — so there is one source of truth per setting. */
  private openPreferences(page?: PrefsPage): void {
    this.preferencesDialog.open({
      pickFolder: async (_initial: string | null) =>
        this.picker.pick("Choose a folder", [], null),
      rebuildVolume: async (fsUuid: string) => {
        // Backend purges the volume's rows + clears resume state;
        // then refresh locations (the volume's location id is
        // path-derived) and queue a fresh full walk for it.
        const mountPoint = await api.rebuildVolume(fsUuid);
        this.locations = await api
          .listLocations()
          .catch(() => this.locations);
        const loc = this.locations.find((l) => l.path === mountPoint);
        if (loc) void this.indexTracker.reindexAll([loc.id]);
      },
      backendThemes: () => this.themeManager.list,
      activeTheme: () => this.themeManager.active,
      resolvedEngine: () => {
        const id = this.themeManager.resolved;
        const e = id ? DISTRO_ENGINES.find((x) => x.id === id) : undefined;
        return e ? { name: e.name, preview: e.preview } : null;
      },
      applyTheme: (id) => this.themeManager.apply(id),
      fontScale: () => this.fontScale,
      setFontScale: (v) => this.setFontScale(v),
      iconSize: () => this.iconSize,
      stepIconSize: (dir) => this.stepIconSize(dir),
      romsOn: () => romsEnabled(),
      setRomsOn: (on) => {
        setRomsEnabled(on);
        void this.buildSidebar();
      },
      romIconSet: () => loadRomIconSet(),
      setRomIconSet: (set) => {
        saveRomIconSet(set);
        this.renderRomsSection();
      },
      sectionCollapsed: (key) =>
        this.prefCollapsed(key, key === "ff-roms-collapsed"),
      setSectionCollapsed: (key, collapsed) => {
        this.setCollapsedPref(key, collapsed);
        this.applyCollapsePrefs();
      },
      showShortcuts: () => this.showShortcuts(),
    }, page);
  }

  /** Keyboard shortcuts overlay (UI round item 3b, Ctrl+?): the
   * app's REAL bindings, read off onGlobalKey and the player key
   * handlers. If a binding changes there, change it here in the
   * same commit — an invented shortcut in this list is a lie. */
  private showShortcuts(): void {
    this.closeShortcuts();
    const overlay = document.createElement("div");
    overlay.className = "props-overlay";
    overlay.addEventListener("mousedown", (e) => {
      if (e.target === overlay) this.closeShortcuts();
    });
    const card = document.createElement("div");
    card.className = "shortcuts-card";
    card.setAttribute("role", "dialog");
    card.setAttribute("aria-label", "Keyboard shortcuts");
    const head = document.createElement("div");
    head.className = "prefs-head";
    const title = document.createElement("h2");
    title.className = "prefs-title";
    title.textContent = "Keyboard Shortcuts";
    const closeBtn = document.createElement("button");
    closeBtn.className = "icon-btn prefs-close";
    closeBtn.textContent = "✕";
    closeBtn.title = "Close";
    closeBtn.addEventListener("click", () => this.closeShortcuts());
    head.append(title, closeBtn);
    card.appendChild(head);
    const groups: Array<[string, Array<[string, string]>]> = [
      [
        "Files",
        [
          ["Ctrl X / Ctrl C", "Cut / copy the selection"],
          ["Ctrl V", "Paste into the current folder"],
          ["Ctrl A", "Select all"],
          ["Delete / Backspace", "Move the selection to Trash"],
          ["Ctrl Z", "Undo the last trash or move"],
          ["F2", "Rename"],
          ["Ctrl E", "Make the selected file executable"],
        ],
      ],
      [
        "Navigation & windows",
        [
          ["F5 / Ctrl R", "Refresh the current view"],
          ["Ctrl T", "New tab"],
          ["Ctrl W", "Close tab"],
          ["Alt ↑", "Up one folder"],
          ["Space", "Quick Look preview"],
          ["Esc", "Close menu / Quick Look / this list"],
          ["Ctrl K", "Command palette"],
          ["Ctrl `", "Terminal"],
          ["Ctrl ?", "This list"],
        ],
      ],
      [
        "View",
        [
          ["Ctrl + / Ctrl −", "Text larger / smaller"],
          ["Ctrl 0", "Reset text size"],
        ],
      ],
      [
        "Players",
        [
          ["Space / K", "Play / pause (video)"],
          ["← / →", "Seek 5 seconds (video & music)"],
          ["F", "Fullscreen (video)"],
          ["M", "Mute (video)"],
        ],
      ],
    ];
    for (const [name, rows] of groups) {
      const h = document.createElement("h3");
      h.className = "prefs-section";
      h.textContent = name;
      card.appendChild(h);
      for (const [key, label] of rows) {
        const row = document.createElement("div");
        row.className = "shortcuts-row";
        const k = document.createElement("kbd");
        k.className = "shortcuts-key";
        k.textContent = key;
        const lab = document.createElement("span");
        lab.textContent = label;
        row.append(k, lab);
        card.appendChild(row);
      }
    }
    overlay.appendChild(card);
    document.body.appendChild(overlay);
    this.shortcutsEl = overlay;
    closeBtn.focus();
  }

  private closeShortcuts(): void {
    if (this.shortcutsEl) {
      this.shortcutsEl.remove();
      this.shortcutsEl = null;
    }
  }

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
    // The main menu (Dickles, 2026-10-15): named groups as flyout
    // submenus — no "Other"/"Miscellaneous" junk drawer; every
    // group name says what its items do. Every one of the old flat
    // menu's 29 items has a named home below.
        this.ctxMenu.open(rect.left - 140, rect.bottom + 6, [
      {
        label: "View",
        action: () => {},
        submenu: [
          viewItem("Grid view", "grid"),
          viewItem("List view", "list"),
          viewItem("Columns view", "columns"),
          viewItem("Gallery view", "gallery"),
          { separator: true, label: "", action: () => {} },
          sortItem("Name", "name"),
          sortItem("Size", "size"),
          sortItem("Date", "modified"),
          { separator: true, label: "", action: () => {} },
          { label: "Larger text", shortcut: "Ctrl +", action: () => this.setFontScale(this.fontScale + 0.1) },
          { label: "Smaller text", shortcut: "Ctrl \u2212", action: () => this.setFontScale(this.fontScale - 0.1) },
          { label: "Larger icons", action: () => this.stepIconSize(1) },
          { label: "Smaller icons", action: () => this.stepIconSize(-1) },
          { separator: true, label: "", action: () => {} },
          { label: "Theme\u2026", action: () => this.themeManager.openMenu(this.menuBtn) },
        ],
      },
      {
        label: "Organize",
        action: () => {},
        submenu: [
          {
            label: "Duplicate Finder\u2026",
            action: () => void this.openDupHere(),
          },
          {
            label: "Synchronize Folders\u2026",
            action: () => void this.openSyncHere(),
          },
          {
            label: "Complete Music Tags & Lyrics\u2026",
            action: () => void this.openMusicHere(),
          },
          {
            label: "Organize Movies & TV\u2026",
            action: () => void this.openMediaOrganizeHere(),
          },
          {
            label: "Organize Video Games\u2026",
            action: () => void this.openRomOrganizeHere(),
          },
          {
            label: "Manage Labels\u2026",
            action: () =>
              this.labelManager.open(() => {
                void this.buildSidebar();
                this.refreshActiveDir();
              }),
          },
          {
            label: "Auto-Label Rules\u2026",
            action: () =>
              this.labelManager.openRules(() => {
                void this.buildSidebar();
                this.refreshActiveDir();
              }),
          },
          {
            label: "Refresh",
            shortcut: "F5",
            action: () => this.refreshCurrentView(),
          },
          { label: "Re-index Library", action: () => this.reindexLibrary() },
        ],
      },
      {
        label: "Connect",
        action: () => {},
        submenu: [
          { label: "Connect to Server\u2026", action: () => void this.connectToServer() },
          { label: "Mount Cloud Drive\u2026", action: () => void this.cloudMount.open() },
        ],
      },
      {
        label: "Storage",
        action: () => {},
        submenu: [
          { label: "Disk Utility\u2026", action: () => void this.diskDialog.open() },
          { label: "Burn Studio\u2026", action: () => this.burnStudio.open() },
          { label: "App Deleter\u2026", action: () => void this.appDeleter.open(() => this.refreshActiveDir()) },
        ],
      },
      {
        label: romsEnabled() ? "Roms Collection \u2713" : "Roms Collection",
        action: () => {
          const on = !romsEnabled();
          setRomsEnabled(on);
          void this.buildSidebar();
          Toast.success(on ? "Roms Collection is on." : "Roms Collection is off. Your systems are kept.");
        },
      },
      { label: "Terminal", shortcut: "Ctrl `", action: () => void this.toggleTerminal() },
      { label: "Preferences\u2026", action: () => this.openPreferences() },
      {
        label: "Help & Feedback",
        action: () => {},
        submenu: [
          {
            label: "Report a Bug\u2026",
            action: () =>
              void api.openUrl(
                "https://github.com/garrisonr/fable-files/issues/new?title=" +
                  encodeURIComponent("[Bug] ") +
                  "&labels=bug"
              ),
          },
          {
            label: "Request a Feature\u2026",
            action: () =>
              void api.openUrl(
                "https://github.com/garrisonr/fable-files/issues/new?title=" +
                  encodeURIComponent("[Feature] ") +
                  "&labels=enhancement"
              ),
          },
        ],
      },
      { label: "About Fable Files", action: () => this.aboutDialog.open() },
      { label: "Restore Defaults\u2026", danger: true, action: () => void this.restoreDefaults() },
    ]);
  }

  /** Delete EVERY label + its file assignments (irreversible). */
  private async deleteAllLabels(): Promise<void> {
    const defs = loadLabelDefs();
    if (!defs.length) {
      Toast.info("No labels to delete.");
      return;
    }
    const ok = await confirmDialog(
      "Delete ALL labels?",
      `${defs.length} labels will be removed, and every file loses its label assignments. This is irreversible.`,
      "Delete All Labels",
      true,
    );
    if (!ok) return;
    for (const d of defs) {
      try {
        await api.deletePathLabel(d.name);
      } catch {
        /* keep going: partial is still progress */
      }
    }
    saveLabelDefs([]);
    saveLabelRules([]);
    void this.buildSidebar();
    this.refreshActiveDir();
    Toast.success("All labels deleted.", 6000);
  }

  /** Factory reset of app settings (never touches files). */
  private async restoreDefaults(): Promise<void> {
    const ok = await confirmDialog(
      "Restore defaults?",
      "This resets Fable Files to factory defaults:\n\n" +
        "\u2022 Theme, text size and view settings\n" +
        "\u2022 Sidebar labels and auto-label rules (label names are removed; files keep their catalog data)\n" +
        "\u2022 Smart folders (the starter set comes back)\n" +
        "\u2022 Saved sync jobs \u2014 their scheduled timers are stopped\n\n" +
        "Your files are never touched. This can\u2019t be undone.",
      "Restore Defaults",
      true,
    );
    if (!ok) return;
    try {
      const schedules = await api.listSyncSchedules();
      for (const s of schedules) {
        try {
          await api.deleteSyncSchedule(s.id);
        } catch {
          /* timer may already be gone */
        }
      }
    } catch {
      /* schedules unavailable */
    }
    const keys: string[] = [];
    for (let i = 0; i < localStorage.length; i++) {
      const k = localStorage.key(i);
      if (k && (k.startsWith("ff-") || k.startsWith("fable"))) keys.push(k);
    }
    for (const k of keys) localStorage.removeItem(k);
    Toast.success("Defaults restored \u2014 restarting\u2026", 2500);
    setTimeout(() => location.reload(), 800);
  }

  // ------------------------------------------------------------ ROM Home

  /** Enter the room: the browse views hide, the room paints from
   * the catalog alone. Leaving restores this view exactly (SPEC
   * §5 — the room is a place you visit, state intact both ways). */
  private enterRomHome(target?: { kind: "system"; name: string }): void {
    if (this.searchView.active) this.searchView.clear();
    this.quickLook.close();
    this.fileView.el.hidden = true;
    this.columnsView.el.hidden = true;
    this.galleryView.el.hidden = true;
    this.searchView.el.hidden = true;
    this.romsHead?.classList.add("selected");
    void this.romHome.open(target);
  }

  private exitRomHome(): void {
    if (!this.romHome) return;
    const wasActive = this.romHome.active;
    this.romHome.close();
    this.romHome.el.hidden = true;
    this.romsHead?.classList.remove("selected");
    if (wasActive) {
      this.showFileView();
      this.renderCrumbs();
      void this.refreshRomDoor();
    }
  }

  /** The door's numbers — straight from the catalog, never a scan. */
  private async refreshRomDoor(): Promise<void> {
    const counts = await api.romHomeCounts().catch(() => null);
    if (!counts) return;
    if (this.romsDoorCount) {
      this.romsDoorCount.textContent =
        counts.total > 0 ? counts.total.toLocaleString() : "";
    }
    if (this.romsDoorDot) this.romsDoorDot.hidden = counts.missingMedia === 0;
    // v2.1 fleet pill: updates waiting outrank the quieter
    // "preparing" state; neither shows when the fleet is settled.
    if (this.romsFleetPill) {
      const badge = await api.emuFleetBadge().catch(() => null);
      const pill = this.romsFleetPill;
      if (badge && badge.updates > 0) {
        pill.hidden = false;
        pill.textContent = `${badge.updates} update${badge.updates === 1 ? "" : "s"}`;
        pill.title = "Emulator updates are waiting — Preferences → ROMs";
        pill.classList.add("has-updates");
      } else if (badge && badge.prepping) {
        pill.hidden = false;
        pill.textContent = "Preparing…";
        pill.title = "Fable is preparing your emulators";
        pill.classList.remove("has-updates");
      } else {
        pill.hidden = true;
      }
    }
    if (this.romsHead) {
      this.romsHead.title =
        counts.total > 0
          ? `ROM Home — ${counts.total.toLocaleString()} games${counts.missingMedia > 0 ? `, ${counts.missingMedia.toLocaleString()} missing media` : ""} (right-click to add a system)`
          : "ROM Home — right-click to add a system";
    }
  }

  /** "Add a ROM folder…": register the folder as a location; the
   * indexer + identification fill the shelves from there. */
  private async addRomFolder(): Promise<void> {
    const path = await this.picker.pick("Add a ROM folder…", [], null);
    if (!path) return;
    const name = path.split("/").filter(Boolean).pop() ?? "ROMs";
    try {
      const summary = await api.addPathLocation(name, path);
      await this.refreshLocations();
      void this.indexTracker.reindexAll([summary.id]);
      Toast.success(
        `Added “${name}” — its games join the shelves as they're identified.`,
      );
      this.romHome.invalidate();
      void this.refreshRomDoor();
    } catch (err) {
      Toast.error(`Could not add that folder: ${errText(err)}`);
    }
  }

  /** The inspector's "Show in Files": leave the room, walk the
   * Files view to the game's real folder, select the game. */
  private async revealRomInFiles(item: Item): Promise<void> {
    this.exitRomHome();
    const loc =
      this.locations.find((l) => l.id === item.location) ?? this.locations[0];
    if (!loc || !item.provider_key) {
      Toast.info(`“${item.name}” — ${item.provider_key ?? "no local path"}`);
      return;
    }
    const walked = await this.crumbsForPath(loc, dirname(item.provider_key));
    if (!walked) {
      Toast.info(
        `“${item.name}” lives at ${item.provider_key} — that folder isn't reachable right now.`,
        6000,
      );
      return;
    }
    const tab = this.requireTab();
    navigate(tab, {
      locationId: loc.id,
      dirId: walked.dirId,
      crumbs: walked.crumbs,
    });
    await this.loadDir(tab);
    if (tab.items.some((i) => i.id === item.id)) {
      tab.selection.clear();
      tab.selection.add(item.id);
      this.renderActiveView();
      this.renderCrumbs();
    }
  }

  /** The door's render: no tree anymore — just the visibility gate
   * (Preferences ▸ Sidebar) and the live tallies. */
  private renderRomsSection(): void {
    if (!this.romsHead) return;
    this.romsHead.hidden = !romsEnabled();
    if (this.romsList) {
      this.romsList.innerHTML = "";
      this.romsList.hidden = true;
    }
    if (romsEnabled()) void this.refreshRomDoor();
  }

  /** Add or edit a Roms Collection system. Built-in systems are
   * governed by the ROM Lists (identification, not extensions) and
   * their names are locked to the list keys; every system carries a
   * scope (which drives it draws from). Custom systems keep the
   * extension editor. */
  private openRomEditor(sys: RomSystem | null): void {
    const isDat = sys ? romSystemKey(sys) !== null : false;
    const isPlaceholder = sys ? isRomPlaceholder(sys) : false;
    const overlay = document.createElement("div");
    overlay.className = "picker-overlay";
    overlay.innerHTML = `
      <div class="picker-card labels-card" role="dialog" aria-label="Rom system">
        <h3 class="picker-title">${sys ? "Rom System" : "Add a System"}</h3>
        <label class="smart-lab">Name
          <input class="smart-in rom-name" type="text" placeholder="e.g. Atari Jaguar" />
        </label>
        <div class="rom-ext-wrap">
          <label class="smart-lab">ROM file extensions (comma separated)
            <input class="smart-in rom-exts" type="text" placeholder="e.g. j64, jag" />
          </label>
          <p class="smart-hint">A system gathers files with those extensions from your indexed locations. Shared extensions (iso, bin, zip) can appear under several systems.</p>
        </div>
        <p class="smart-hint rom-dat-note" hidden>This system is governed by the ROM lists (No-Intro / Redump / MAME): a file appears only when its contents match a known game exactly — extensions don't decide, so there is nothing to edit here.</p>
        <label class="smart-lab">Look for ROMs on
          <select class="smart-in rom-scope">
            <option value="all">All drives</option>
            <option value="boot">Boot drive only</option>
            <option value="drives">Choose drives…</option>
          </select>
        </label>
        <div class="rom-drives"></div>
        <div class="picker-actions">
          <button class="rom-cancel">Cancel</button>
          <button class="rom-save primary">Save</button>
        </div>
      </div>`;
    document.body.appendChild(overlay);
    const q = <T extends HTMLElement>(sel: string) => overlay.querySelector(sel) as T;
    const scopeSel = q<HTMLSelectElement>(".rom-scope");
    const drivesEl = q<HTMLDivElement>(".rom-drives");
    const renderDrives = () => {
      drivesEl.innerHTML = "";
      if (scopeSel.value !== "drives") return;
      const chosen = new Set(sys?.scope?.drives ?? []);
      for (const loc of this.locations) {
        const lab = document.createElement("label");
        lab.className = "rom-drive";
        const cb = document.createElement("input");
        cb.type = "checkbox";
        cb.value = loc.id;
        cb.checked = chosen.has(loc.id);
        const span = document.createElement("span");
        span.textContent = loc.display_name;
        lab.append(cb, span);
        drivesEl.appendChild(lab);
      }
      if (this.locations.length === 0) {
        drivesEl.textContent = "No drives are available right now.";
      }
    };
    scopeSel.addEventListener("change", renderDrives);
    if (sys) {
      q<HTMLInputElement>(".rom-name").value = sys.name;
      q<HTMLInputElement>(".rom-exts").value = sys.extensions.join(", ");
      scopeSel.value = sys.scope?.mode ?? "all";
    }
    if (isDat) {
      // The name is the dataset key — renaming would orphan the system.
      q<HTMLInputElement>(".rom-name").disabled = true;
      q<HTMLInputElement>(".rom-name").title =
        "Built-in system names come from the ROM lists and can't be renamed.";
      q<HTMLDivElement>(".rom-ext-wrap").hidden = true;
      q<HTMLParagraphElement>(".rom-dat-note").hidden = false;
    }
    if (isPlaceholder) {
      // A placeholder gathers nothing and identifies nothing —
      // extensions would be a false promise, so they're not
      // editable; the name is likewise the registry identity.
      q<HTMLInputElement>(".rom-name").disabled = true;
      q<HTMLInputElement>(".rom-name").title =
        "Placeholder systems keep their registry name.";
      q<HTMLDivElement>(".rom-ext-wrap").hidden = true;
      const note = q<HTMLParagraphElement>(".rom-dat-note");
      note.textContent =
        "This is a placeholder system: it holds a shelf for a machine whose emulator cores and verified game lists don't exist yet. It gathers no files and fetches no media — when the real support lands, the shelf wakes up on its own.";
      note.hidden = false;
    }
    renderDrives();
    q<HTMLButtonElement>(".rom-cancel").addEventListener("click", () => overlay.remove());
    overlay.addEventListener("pointerdown", (e) => {
      if (e.target === overlay) overlay.remove();
    });
    q<HTMLButtonElement>(".rom-save").addEventListener("click", () => {
      const name = q<HTMLInputElement>(".rom-name").value.trim();
      const exts =
        isDat || isPlaceholder
          ? [...(sys?.extensions ?? [])]
          : q<HTMLInputElement>(".rom-exts")
              .value.split(/[,\s]+/)
              .map((s) => s.replace(/^\./, "").toLowerCase())
              .filter(Boolean);
      if (!name) {
        Toast.error("Give the system a name.");
        return;
      }
      if (!isDat && !isPlaceholder && exts.length === 0) {
        Toast.error("Add at least one ROM extension.");
        return;
      }
      const mode = scopeSel.value as "all" | "boot" | "drives";
      const drives =
        mode === "drives"
          ? [...drivesEl.querySelectorAll<HTMLInputElement>("input:checked")].map(
              (cb) => cb.value,
            )
          : [];
      if (mode === "drives" && drives.length === 0) {
        Toast.error("Pick at least one drive, or choose All drives.");
        return;
      }
      const scope = { mode, drives };
      const list = loadRomSystems();
      if (sys) {
        const idx = list.findIndex((s) => s.name === sys.name);
        if (idx >= 0)
          list[idx] = {
            name,
            extensions: exts,
            scope,
            // The kind is identity, not an edit — saving the system
            // never silently standardizes a placeholder.
            ...(sys.kind ? { kind: sys.kind } : {}),
          };
      } else {
        if (list.some((s) => s.name === name)) {
          Toast.error(`"${name}" is already in the collection.`);
          return;
        }
        list.push({ name, extensions: exts, scope });
        list.sort((a, b) => a.name.localeCompare(b.name));
      }
      saveRomSystems(list);
      void this.buildSidebar();
      overlay.remove();
      Toast.success(`Saved system \u201c${name}\u201d.`);
    });
  }

  /** Manage a smart folder: edit its name and what it gathers. */
  private openSmartEditor(sf: SmartFolder): void {
    const overlay = document.createElement("div");
    overlay.className = "picker-overlay";
    const isRule = !!sf.rule;
    const typeOpts = [
      ["", "All types"],
      ["image", "Images"],
      ["video", "Videos"],
      ["audio", "Audio"],
      ["document", "Documents"],
      ["archive", "Archives"],
    ]
      .map(
        ([v, l]) =>
          `<option value="${v}"${(sf.type ?? "") === v ? " selected" : ""}>${l}</option>`,
      )
      .join("");
    overlay.innerHTML = `
      <div class="picker-card labels-card" role="dialog" aria-label="Manage smart folder">
        <h3 class="picker-title">Smart Folder</h3>
        <label class="smart-lab">Name
          <input class="smart-in smart-name" type="text" />
        </label>
        ${
          isRule
            ? `<label class="smart-lab">File extensions it gathers (comma separated)
                 <input class="smart-in smart-exts" type="text" placeholder="e.g. mkv, mp4" />
               </label>
               <label class="smart-lab">Only files longer than this many minutes (optional)
                 <input class="smart-in smart-mins" type="number" min="0" placeholder="Leave blank to match any length" />
               </label>
               <p class="smart-hint">The minutes box is optional. Fill it in and only media LONGER than that matches \u2014 that\u2019s how Movies skips short clips.</p>`
            : `<label class="smart-lab">Search text
                 <input class="smart-in smart-q" type="text" />
               </label>
               <label class="smart-lab">File type
                 <select class="smart-in smart-type">${typeOpts}</select>
               </label>`
        }
        <div class="picker-actions">
          <button class="smart-cancel">Cancel</button>
          <button class="smart-save primary">Save</button>
        </div>
      </div>`;
    document.body.appendChild(overlay);
    const q = <T extends HTMLElement>(sel: string) =>
      overlay.querySelector(sel) as T;
    q<HTMLInputElement>(".smart-name").value = sf.name;
    if (isRule) {
      q<HTMLInputElement>(".smart-exts").value = (sf.rule!.extensions ?? []).join(", ");
      const mins = sf.rule!.minDurationSecs;
      q<HTMLInputElement>(".smart-mins").value = mins ? String(Math.round(mins / 60)) : "";
    } else {
      q<HTMLInputElement>(".smart-q").value = sf.query ?? "";
    }
    q<HTMLButtonElement>(".smart-cancel").addEventListener("click", () => overlay.remove());
    overlay.addEventListener("pointerdown", (e) => {
      if (e.target === overlay) overlay.remove();
    });
    q<HTMLButtonElement>(".smart-save").addEventListener("click", () => {
      const name = q<HTMLInputElement>(".smart-name").value.trim();
      if (!name) {
        Toast.error("Give the smart folder a name.");
        return;
      }
      const list = Shell.loadSmartFolders();
      const idx = list.findIndex((f) => f.name === sf.name);
      if (idx < 0) {
        overlay.remove();
        return;
      }
      const next: SmartFolder = { ...list[idx], name };
      if (isRule) {
        const exts = q<HTMLInputElement>(".smart-exts")
          .value.split(/[,\s]+/)
          .map((s) => s.replace(/^\./, "").toLowerCase())
          .filter(Boolean);
        const mins = parseInt(q<HTMLInputElement>(".smart-mins").value, 10);
        next.rule = {
          extensions: exts,
          minDurationSecs: Number.isFinite(mins) && mins > 0 ? mins * 60 : null,
        };
      } else {
        next.query = q<HTMLInputElement>(".smart-q").value.trim();
        next.type = q<HTMLSelectElement>(".smart-type").value || null;
      }
      list[idx] = next;
      Shell.saveSmartFolders(list);
      void this.buildSidebar();
      overlay.remove();
      Toast.success(`Saved smart folder \u201c${name}\u201d.`);
    });
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
      Toast.error(`Undo failed: ${errText(err)}`);
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
    if ((e.ctrlKey || e.metaKey) && e.key === "?") {
      // Ctrl+? — the shortcuts cheat sheet (UI round item 3b).
      e.preventDefault();
      if (this.shortcutsEl) this.closeShortcuts();
      else this.showShortcuts();
      return;
    }
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
    if (e.key === "F5") {
      // F5: refresh the current view in place — never a webview
      // reload. Unbound before 2026-10-16; the instinct is universal,
      // so the app honors it (Dickles).
      e.preventDefault();
      this.refreshCurrentView();
      return;
    }
    if (
      (e.ctrlKey || e.metaKey) &&
      e.key.toLocaleLowerCase() === "r" &&
      !inField
    ) {
      // Ctrl+R: the same refresh. Was unbound; nothing is stolen.
      e.preventDefault();
      this.refreshCurrentView();
      return;
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
      // Search results carry their own selection — trash it there.
      // (Reaching here means focus is NOT in a text field: `if
      // (inField) return` above keeps Backspace for query editing.)
      if (this.searchView.active && this.searchView.tryTrashSelection()) {
        e.preventDefault();
        return;
      }
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
      if (this.shortcutsEl) this.closeShortcuts();
      else if (!this.menuLayer.hidden) this.closeMenu();
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
