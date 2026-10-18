/**
 * Typed bridge to the Tauri backend (src-tauri/src/commands.rs).
 * Shapes follow the backend contract; every command is thin and typed.
 */
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { CensusReport, LocationScopeWire } from "./addscope";

/** A mounted root: local dir, cloud account, archive, search… */
export interface DiskPart {
  name: string;
  path: string;
  size_bytes: number;
  fstype: string | null;
  label: string | null;
  mountpoints: string[];
  locked: boolean;
  lock_reason: string | null;
}

export interface DiskInfo {
  path: string;
  model: string;
  size_bytes: number;
  removable: boolean;
  transport: string;
  partitions: DiskPart[];
  locked: boolean;
  lock_reason: string | null;
}

export interface ResizeInfo {
  size_bytes: number;
  free_after_bytes: number;
  can_grow: boolean;
  can_shrink: boolean;
  note: string;
}

export interface TorrentOutcome {
  torrent_path: string;
  payload_path: string;
  washed_files: number;
  cleaned_images: number;
}

export interface PrinterInfo {
  name: string;
  is_default: boolean;
  state: string;
}

export interface MetaTag {
  group: string;
  tag: string;
  value: string;
}

/** What a start_* transfer command returns (UI round): the job id,
 * plus alreadyRunning when the submission was folded into an
 * identical active job by the double-submit guard. */
export interface JobStart {
  jobId: string;
  alreadyRunning: boolean;
}

/** One registered job, as `list_jobs` reports it (Phase 1 inc-2). */
export interface JobSummary {
  job_id: string;
  title: string;
  state: string;
  steps_done: number;
  steps_total: number;
  bytes_done: number;
  bytes_total: number;
  /** UI round (packed transfer): show the card's packed label. */
  packed: boolean;
}

/** Per-source record of what a finished job actually did — the undo
 * feed (Phase 1 inc-2 §2.2). Mirrors ufm_jobs::JobOutcome. */
/** Disk space for the volume backing a path (inc-2 item 12) —
 * statvfs numbers (df-equivalent) + findmnt mount metadata. */
export interface DiskInfo {
  path: string;
  mount_point: string;
  source: string | null;
  fstype: string | null;
  total_bytes: number;
  used_bytes: number;
  /** f_bavail — what an unprivileged user can actually write. */
  free_bytes: number;
  percent_free: number;
}

export interface JobOutcome {
  step_id: string;
  src_path: string;
  result_path: string | null;
  trash_name: string | null;
}

export interface ArchiveEntry {
  path: string;
  size: number;
  is_dir: boolean;
}

export interface LocationSummary {
  id: string;
  provider: string;
  display_name: string;
  /** Item id of the location's root — pass as `dir` to `listDir` to open it.
   *  Real UUID minted by the backend; never synthesize ids on the shell side. */
  root_item: string;
  /** Absolute root path when the backend knows it (Round 7.2: mounted
   *  volumes carry it so the sidebar can eject by path). Null for a
   *  network server that isn't connected right now. */
  path: string | null;
  /** Removable media (USB / SD) and devices (phone/camera) — the
   *  sidebar offers eject. */
  removable: boolean;
  /** Network Locations: the normalized server URI (provider
   *  "network"); device locations: the GIO root URI, e.g.
   *  "mtp://[usb:001,007]/" (provider "device"). Never carries a
   *  password. */
  uri?: string;
  /** False only for a bookmarked network server that isn't
   *  currently connected — it stays listed (Library Memory) with
   *  Connect as its action. */
  online: boolean;
}

export type ItemKind = "file" | "folder" | string;

export interface ItemProperties {
  name: string;
  path: string;
  is_dir: boolean;
  size_bytes: number;
  modified: string;
  created: string;
  accessed: string;
  permissions: string;
  mode_octal: string;
  executable: boolean;
}

export interface Item {
  id: string;
  /** Location this item belongs to — the backend always sends it. */
  location: string;
  name: string;
  kind: ItemKind;
  size_bytes: number | null;
  modified: string | null;
  provider_key: string | null;
  labels: string[];
  starred: boolean;
}

export interface SearchHit {
  item: Item;
  rank: number;
  /** Library Memory: the volume this file lives on, when known.
   *  Files on disconnected drives stay in the results, badged. */
  volume_label?: string | null;
  volume_offline?: boolean;
  /** RFC 3339: when that volume was last seen mounted. */
  volume_last_seen?: string | null;
}

/** Phase 2: one collection as the sidebar lists it. */
export interface CollectionInfo {
  id: string;
  name: string;
  /** manual | saved_search | smart */
  kind: string;
  member_count: number;
  /** Members whose path no longer resolves to a file. */
  missing_count: number;
}

/** Phase 2: a collection's resolved contents plus its missing paths. */
export interface CollectionContents {
  items: Item[];
  missing: string[];
}

/** Phase 2: per-location remembered view state. */
export interface ViewProfile {
  view_mode: string;
  sort_key: string;
  /** 1 ascending, -1 descending. */
  sort_dir: number;
  show_extensions: boolean;
}

/** One item's volume badge (rule/smart-folder results). */
export interface ItemVolumeBadge {
  item_id: string;
  volume_label: string;
  offline: boolean;
  last_seen?: string | null;
}

/** Phase 6 — one plugin folder, as Preferences lists it. Mirrors
 *  plugins.rs PluginView (camelCase over the wire). */
export interface PluginInfo {
  key: string;
  name: string;
  author: string;
  version: string;
  description: string;
  capabilities: string[];
  permissions: string[];
  enabled: boolean;
  valid: boolean;
  error: string | null;
  /** v1 dispatches `action.context`; other capabilities are
   *  declared and displayed, not wired. */
  wired: boolean;
}

export interface PluginProposal {
  pluginKey: string;
  pluginName: string;
  name: string;
  contents: string;
  targetDir: string;
}

export interface PluginRun {
  logs: string[];
  proposals: PluginProposal[];
}

/** Phase 6 — one folder template, as Preferences lists it. */
export interface FolderTemplateInfo {
  id: string;
  name: string;
  starter: boolean;
  folders: number;
  files: number;
  bytes: number;
}

export interface CreateTemplateResult {
  template: FolderTemplateInfo;
  skippedLargeFiles: number;
}

/** Phase 6 — the reader's opening handshake for one book. */
export interface BookOpen {
  title: string | null;
  author: string | null;
  chapterCount: number;
  drmProtected: boolean;
  note: string | null;
  startChapter: number;
  fontScale: number;
  /** Ebook P2 — decoded chapter titles in spine order. */
  chapterTitles: (string | null)[];
  /** Ebook P2 — saved within-chapter place for startChapter. */
  startScrollFrac: number | null;
}

export interface ChapterView {
  index: number;
  title: string | null;
  text: string;
  /** Ebook P2 — sanitized publisher XHTML; null → render text. */
  html: string | null;
  /** Ebook P2 — scoped publisher CSS for the chapter. */
  css: string | null;
  chapterCount: number;
}

/** PDF Reader — the opening handshake for one document. */
export interface PdfOutlineEntry {
  title: string;
  /** 1-based page the bookmark points at. */
  page: number;
  depth: number;
}

export interface PdfOpen {
  title: string | null;
  author: string | null;
  pageCount: number;
  encrypted: boolean;
  note: string | null;
  outline: PdfOutlineEntry[];
  startPage: number;
  zoomMode: string;
  pageWidthPts: number | null;
  pageHeightPts: number | null;
}

export interface PdfPageView {
  index: number;
  image: string;
  width: number;
  height: number;
}

/** Library Memory settings (Preferences): how long Fable remembers
 *  a disconnected drive's files. null = Forever. */
export interface LibrarySettings {
  retention_days: number | null;
  overrides: Record<string, number | null>;
}

/** A remembered volume, as Preferences lists it. */
export interface VolumeSummary {
  fs_uuid: string;
  label: string;
  online: boolean;
  mount_point: string | null;
  /** RFC 3339. */
  last_seen: string;
  last_indexed: string | null;
  item_count: number;
  /** The root filesystem: always remembered, never pruned. */
  is_system: boolean;
  /** Retention in force for this volume (null = Forever). */
  retention_days: number | null;
  has_override: boolean;
  override_days: number | null;
}

export interface SyncSchedule {
  id: string;
  a: string;
  b: string;
  direction: string; // "a2b" | "b2a" | "both"
  interval_min: number;
}

export interface ThemeInfo {
  id: string;
  name: string;
  dark: boolean;
}

export interface IndexState {
  running: boolean;
  dirs_done: number;
  files_done: number;
  bytes_done: number;
  /** Indexer v2 (incremental): entries verified in place, entries
   *  rewritten, catalog rows removed — see indexing.ts. */
  files_unchanged: number;
  files_updated: number;
  files_removed: number;
}

/** PreviewKind from ufm-preview (serialized snake_case). */
export type PreviewKind =
  | "native"
  | "media"
  | "document"
  | "archive"
  | "metadata"
  | "hex";

export const PREVIEW_LABELS: Record<PreviewKind, string> = {
  native: "Native",
  media: "Media",
  document: "Document",
  archive: "Archive",
  metadata: "Metadata",
  hex: "Hex",
};

/**
 * Root-dir addressing: the backend mints a real ItemId for each location root
 * and ships it as `LocationSummary.root_item`. The shell must use that value
 * verbatim — ItemIds are UUIDs and cannot be synthesized client-side.
 */

/** Local file path → URL the WebView can load (asset protocol). */
export function fileSrc(path: string): string {
  return convertFileSrc(path);
}

export interface LabelRule {
  label: string;
  extensions: string[];
  min_duration_secs: number | null;
}

export interface RuleItemsResult {
  items: Item[];
  probed: number;
  remaining: number;
  /** Total members in the stored result (items may be truncated). */
  total_members: number;
  /** Unprobed candidates on offline volumes (never probed; they
   * join when the volume returns). */
  pending_offline: number;
  /** True when no online probing remains for this rule. */
  sweep_complete: boolean;
  /** Library Memory: volume badges for `items`, by item id. */
  badges: ItemVolumeBadge[];
}

export interface RuleProbeResult {
  probed: number;
  /** Unprobed candidates left on mounted volumes. */
  remaining: number;
  /** Unprobed candidates on offline volumes. */
  remainingOffline: number;
  /** Files this batch newly proved to be members. */
  newMembers: Item[];
  /** Library Memory: volume badges for `newMembers`, by item id. */
  badges: ItemVolumeBadge[];
  /** The batch stopped early (the view was left); probed progress
   * is persisted and resumes next visit. */
  cancelled: boolean;
  /** Probing itself is unavailable (e.g. ffprobe missing). */
  probeError: string | null;
}

export interface RuleOutcome {
  label: string;
  matched: number;
  added: number;
  remaining: number;
}

/**
 * Media playback URL: the app's loopback media server (HTTP Range).
 * WebKit's media stack plays plain HTTP reliably; the asset protocol
 * fails on real-world video (proven 2026-10-13). Falls back to the
 * asset protocol if the server cannot start.
 */
export async function mediaSrc(path: string): Promise<string> {
  try {
    return await api.mediaUrlForPath(path);
  } catch {
    return fileSrc(path);
  }
}

/**
 * Streaming-transcode URL (Round 7.1): the same media server serves
 * /transcode/<token>?t=<startSecs> — ffmpeg re-encodes on the fly to
 * H.264 for formats WebKit can't render (HEVC 10-bit etc.). Returns
 * null when the media server URL is unavailable (asset-protocol
 * fallback has no transcode route).
 */
export async function transcodeSrc(
  path: string,
  startSecs: number,
): Promise<string | null> {
  try {
    const url = await api.mediaUrlForPath(path);
    if (!url.includes("/cast/")) return null;
    const t = Math.max(0, Math.floor(startSecs));
    return url.replace("/cast/", "/transcode/") + `?t=${t}`;
  } catch {
    return null;
  }
}

/** Subscribe to pop-out player windows closing. Returns an unlisten fn. */
export async function onPlayerClosed(
  cb: (path: string) => void,
): Promise<() => void> {
  return listen<string>("player-closed", (e) => cb(e.payload));
}

/** Playback state handed between the main window and a detach
 * (pop-out) window (UI round): position in seconds + whether it was
 * playing. `player-state` streams from the detach window while it
 * plays; `player-return` is the final handoff when it comes home. */
export interface PlayerState {
  path: string;
  t: number;
  playing: boolean;
  /** The detach window finished the track (audio queue advance). */
  ended?: boolean;
}

export async function onPlayerState(
  cb: (s: PlayerState) => void,
): Promise<() => void> {
  return listen<PlayerState>("player-state", (e) => cb(e.payload));
}

export async function onPlayerReturn(
  cb: (s: PlayerState) => void,
): Promise<() => void> {
  return listen<PlayerState>("player-return", (e) => cb(e.payload));
}

export interface SyncPair {
  id: string;
  name: string;
  root_a: string;
  root_b: string;
}

export interface SyncPairView {
  pair: SyncPair;
  aAvailable: boolean;
  bAvailable: boolean;
  lastSummary: string | null;
}

export interface SyncPlanActionView {
  kind: string;
  rel: string;
  line: string;
}

export interface SyncConflictNote {
  rel: string;
  winner: string;
  loser_kept: string;
  reason: string;
}

export interface SyncPlanView {
  pairId: string;
  actions: SyncPlanActionView[];
  conflicts: SyncConflictNote[];
  skipped: Array<{ side: string; rel: string; reason: string }>;
  unchanged: number;
  copiesAToB: number;
  copiesBToA: number;
  deletions: number;
  summary: string;
}

export interface SyncReport {
  pair_id: string;
  pair_name: string;
  started: string;
  finished: string | null;
  copied_a_to_b: string[];
  copied_b_to_a: string[];
  deleted_from_a: string[];
  deleted_from_b: string[];
  restored: string[];
  conflicts: SyncConflictNote[];
  failures: string[];
}

export const api = {
  listLocations: () => invoke<LocationSummary[]>("list_locations"),
  /** Durable UI prefs (Round 7.2): backend sidecar, survives where
   *  WebKit localStorage did not. Values are plain JSON. */
  getUiPrefs: () => invoke<Record<string, unknown>>("get_ui_prefs"),
  setUiPrefs: (prefs: Record<string, unknown>) =>
    invoke<void>("set_ui_prefs", { prefs }),
  listDir: (location: string, dir: string) =>
    invoke<Item[]>("list_dir", { location, dir }),
  search: (query_text: string, scope: string | null, limit = 50, typeFilter?: string | null, filters?: { pathPrefix?: string | null; modifiedAfter?: string | null; modifiedBefore?: string | null }) =>
    invoke<SearchHit[]>("search", { queryText: query_text, scope, limit, typeFilter: typeFilter ?? null, pathPrefix: filters?.pathPrefix ?? null, modifiedAfter: filters?.modifiedAfter ?? null, modifiedBefore: filters?.modifiedBefore ?? null }),
  listThemes: () => invoke<ThemeInfo[]>("list_themes"),
  applyTheme: (theme: string) => invoke<string>("apply_theme", { theme }),
  preview: (item: string) => invoke<PreviewKind>("preview", { item }),
  trashPath: () => invoke<string>("trash_path"),
  trashItem: (item: string) => invoke<string>("trash_item", { item }),
  untrashItem: (trashName: string) => invoke<string>("untrash_item", { trashName }),
  renameItem: (item: string, newName: string) =>
    invoke<string>("rename_item", { item, newName }),
  copyPathTo: (srcPath: string, destDir: string) =>
    invoke<string>("copy_path_to", { srcPath, destDir }),
  compressItems: (items: string[]) =>
    invoke<string>("compress_items", { items }),
  emailItems: (items: string[]) => invoke<void>("email_items", { items }),
  localsendAvailable: () => invoke<boolean>("localsend_available"),
  sendViaLocalsend: (item: string) =>
    invoke<void>("send_via_localsend", { item }),
  clipboardWrite: (paths: string[], cut: boolean) =>
    invoke<void>("clipboard_write", { paths, cut }),
  clipboardRead: () =>
    invoke<{ paths: string[]; cut: boolean }>("clipboard_read"),
  /** Open the detach (pop-out) player window. UI round: the
   * window is an always-on-top mini with transport controls, and it
   * receives the current playback state so playback continues from
   * `start` seconds instead of restarting (bidirectional handoff —
   * state comes back via player-state / player-return events). */
  openPlayerWindow: (
    path: string,
    name: string,
    opts?: {
      kind?: "video" | "audio";
      start?: number;
      autoplay?: boolean;
      /** Cover-art data URL (fix batch item 3): stashed backend-
       * side for the mini window to fetch — never in the URL. */
      art?: string | null;
    },
  ) =>
    invoke<void>("open_player_window", {
      path,
      name,
      kind: opts?.kind ?? "video",
      start: opts?.start ?? 0,
      autoplay: opts?.autoplay ?? false,
      art: opts?.art ?? null,
    }),
  closePlayerWindow: (path: string) =>
    invoke<void>("close_player_window", { path }),
  /** Tombstone guard (fix batch item 4): does this path's detach
   * window actually still exist? */
  playerWindowOpen: (path: string) =>
    invoke<boolean>("player_window_open", { path }),
  /** Artwork stashed for this path's detach window at open time. */
  getPlayerArt: (path: string) =>
    invoke<string | null>("get_player_art", { path }),
  makeAlias: (item: string) => invoke<string>("make_alias", { item }),
  convertImage: (item: string, format: string) =>
    invoke<string>("convert_image", { item, format }),
  rotateImageLeft: (item: string) =>
    invoke<void>("rotate_image_left", { item }),
  createPdf: (items: string[]) => invoke<string>("create_pdf", { items }),
  setWallpaper: (item: string) => invoke<void>("set_wallpaper", { item }),
  openPath: (path: string) => invoke<void>("open_path", { path }),
  listArchive: (item: string) =>
    invoke<ArchiveEntry[]>("list_archive", { item }),
  extractArchive: (item: string, destDir?: string) =>
    invoke<string>("extract_archive", { item, destDir }),
  extractArchiveEntry: (archivePath: string, entryPath: string) =>
    invoke<string>("extract_archive_entry", { archivePath, entryPath }),
  setPathLabel: (path: string, label: string, on: boolean) =>
    invoke<void>("set_path_label", { path, label, on }),
  renamePathLabel: (oldLabel: string, newLabel: string) =>
    invoke<void>("rename_path_label", { oldLabel, newLabel }),
  deletePathLabel: (label: string) =>
    invoke<void>("delete_path_label", { label }),
  listItemsByLabel: (label: string) =>
    invoke<Item[]>("list_items_by_label", { label }),
  // Phase 2 — tags/stars by item, collections, view profiles.
  tagItem: (item: string, label: string) =>
    invoke<void>("tag_item", { item, label }),
  untagItem: (item: string, label: string) =>
    invoke<void>("untag_item", { item, label }),
  toggleStar: (item: string) => invoke<boolean>("toggle_star", { item }),
  starredItems: () => invoke<Item[]>("starred_items"),
  listCollections: () => invoke<CollectionInfo[]>("list_collections"),
  getCollection: (id: string) =>
    invoke<CollectionInfo>("get_collection", { id }),
  createCollection: (name: string) =>
    invoke<string>("create_collection", { name }),
  renameCollection: (id: string, name: string) =>
    invoke<void>("rename_collection", { id, name }),
  deleteCollection: (id: string) =>
    invoke<void>("delete_collection", { id }),
  addToCollection: (id: string, items: string[]) =>
    invoke<void>("add_to_collection", { id, items }),
  removeFromCollection: (id: string, items: string[]) =>
    invoke<void>("remove_from_collection", { id, items }),
  removeCollectionPaths: (id: string, paths: string[]) =>
    invoke<void>("remove_collection_paths", { id, paths }),
  collectionItems: (id: string) =>
    invoke<CollectionContents>("collection_items", { id }),
  getViewProfile: (location: string) =>
    invoke<ViewProfile | null>("get_view_profile", { location }),
  setViewProfile: (location: string, profile: ViewProfile) =>
    invoke<void>("set_view_profile", { location, profile }),
  mediaUrlForPath: (path: string) =>
    invoke<string>("media_url_for_path", { path }),
  ruleItems: (
    extensions: string[],
    minDurationSecs: number | null,
    limit?: number,
    probeBudget?: number,
  ) =>
    invoke<RuleItemsResult>("rule_items", {
      extensions,
      minDurationSecs,
      limit,
      probeBudget,
    }),
  /** Reconcile a rule with the catalog (deltas only) — the instant
   * snapshot paint never waits for this. */
  ruleRefresh: (
    extensions: string[],
    minDurationSecs: number | null,
    limit?: number,
  ) =>
    invoke<RuleItemsResult>("rule_refresh", {
      extensions,
      minDurationSecs,
      limit,
    }),
  /** One background batch of the duration sweep. */
  ruleProbe: (
    extensions: string[],
    minDurationSecs: number | null,
    budget?: number,
  ) =>
    invoke<RuleProbeResult>("rule_probe", {
      extensions,
      minDurationSecs,
      budget,
    }),
  /** Stop the running duration sweep; partial progress persists. */
  ruleCancelProbe: () => invoke<void>("rule_cancel_probe"),
  applyLabelRules: (rules: LabelRule[]) =>
    invoke<RuleOutcome[]>("apply_label_rules", { rules }),
  addPathLocation: (name: string, path: string) =>
    invoke<LocationSummary>("add_path_location", { name, path }),
  removePathLocation: (location: string) =>
    invoke<void>("remove_path_location", { location }),
  /** Safety gate: census a would-be location (metadata-only,
   * read-only) before anything is registered or indexed. */
  censusLocation: (path: string) =>
    invoke<CensusReport>("census_location", { path }),
  /** Safety gate: register a location and record the scope the
   * census sheet chose; indexing walks only what the scope admits. */
  addScopedLocation: (
    name: string,
    path: string,
    scope: LocationScopeWire,
  ) =>
    invoke<LocationSummary>("add_scoped_location", { name, path, scope }),
  /** Network Locations: connect a server via GIO (smb/ftp/ftps/
   *  sftp/dav/davs/nfs/afp). The password reaches the OS keyring
   *  only when rememberPassword is set and the mount succeeds. */
  netConnect: (args: {
    uri: string;
    username?: string;
    password?: string;
    domain?: string;
    anonymous: boolean;
    rememberPassword: boolean;
  }) =>
    invoke<LocationSummary>("net_connect", {
      uri: args.uri,
      username: args.username ?? null,
      password: args.password ?? null,
      domain: args.domain ?? null,
      anonymous: args.anonymous,
      rememberPassword: args.rememberPassword,
    }),
  netDisconnect: (location: string) =>
    invoke<void>("net_disconnect", { location }),
  netForget: (location: string) => invoke<void>("net_forget", { location }),
  transcode: (item: string, preset: string) =>
    invoke<string>("transcode", { item, preset }),
  cancelTranscode: (id: string) => invoke<void>("cancel_transcode", { id }),
  // Transfer jobs (Phase 1 inc-1/inc-2): journaled, pausable,
  // resumable copy/move/delete through the ufm-jobs engine. All
  // start_* commands return a JobStart; progress arrives as
  // `job-progress` events (see jobs.ts). UI round: an identical
  // submission while a job is active folds into it — the backend
  // answers with the EXISTING job's id and alreadyRunning=true
  // (double-submit guard; doors toast + focus instead of tracking
  // a twin).
  startCopy: (sources: string[], dstParent: string) => {
    const req: CopyRequest = { sources, dstParent };
    return invoke<JobStart>("start_copy", { req });
  },
  startCopyFromPaths: (sources: string[], dstDir: string) => {
    const req: CopyPathsRequest = { sources, dstDir };
    return invoke<JobStart>("start_copy_from_paths", { req });
  },
  startMove: (sources: string[], dstDir: string) => {
    const req: MoveRequest = { sources, dstDir };
    return invoke<JobStart>("start_move", { req });
  },
  startMoveFromPaths: (sources: string[], dstDir: string) => {
    const req: MovePathsRequest = { sources, dstDir };
    return invoke<JobStart>("start_move_from_paths", { req });
  },
  startDelete: (sources: string[], useTrash: boolean) => {
    const req: DeleteRequest = { sources, useTrash };
    return invoke<JobStart>("start_delete", { req });
  },
  startDeleteFromPaths: (paths: string[], useTrash: boolean) => {
    const req: DeletePathsRequest = { paths, useTrash };
    return invoke<JobStart>("start_delete_from_paths", { req });
  },
  // ROM Lists: DAT-based identification + collection membership.
  identifyRoms: () => invoke<RomIdentifyStats>("identify_roms"),
  /** EmulationStation import (Hyperbase #5): attach gamelist
   * metadata (descriptions, artwork, play history) to the
   * catalog by path. No root = every registered location root. */
  esImport: (root?: string) =>
    invoke<EsImportReport>("es_import", { root: root ?? null }),
  /** Ebook P1 — "The Library": identify books (single-flight;
   * a second call while one runs reports alreadyRunning). */
  booksIdentify: (locationId?: string) =>
    invoke<BooksIdentifyReport>("books_identify", {
      locationId: locationId ?? null,
    }),
  /** The whole Books Home room in one payload. */
  booksHome: () => invoke<BooksHomePayload>("books_home"),
  /** Set a book's reading state (want | reading | read); null clears. */
  booksSetState: (path: string, bookState: string | null) =>
    invoke<void>("books_set_state", { path, bookState }),
  /** PC Games P1: Playnite import (preview = parse + plan, no
   * writes; the import itself is idempotent) and the folder-scan
   * fallback for loose PC game folders. */
  pcImportPreview: (root?: string) =>
    invoke<PcImportPreview>("pc_import_preview", { root: root ?? null }),
  pcImportPlaynite: (root?: string) =>
    invoke<PcImportReport>("pc_import_playnite", { root: root ?? null }),
  pcScanFolder: (root: string) =>
    invoke<PcScanReport>("pc_scan_folder", { root }),
  pcGameSheet: (providerKey: string) =>
    invoke<PcGameSheet | null>("pc_game_sheet", { providerKey }),
  pcSetLaunchBadge: (providerKey: string, badge: string, note?: string) =>
    invoke<boolean>("pc_set_launch_badge", {
      providerKey,
      badge,
      note: note ?? null,
    }),
  romCollectionItems: (system: string, scope: RomScope) =>
    invoke<RomHit[]>("rom_collection_items", { system, scope }),
  // ROM identification Phase 2: identities are the banner's and the
  // context-menu door's truth source; the Needs-ID shelf's entries;
  // and the collection reset (Preferences → ROMs).
  romIdentityMap: (itemIds: string[]) =>
    invoke<RomIdentityHit[]>("rom_identity_map", { itemIds }),
  romHomeNeedsId: () => invoke<RomNeedsIdEntry[]>("rom_home_needs_id"),
  romResetCollection: () => invoke<RomResetStats>("rom_reset_collection"),
  // ROM media enrichment: settings + keyring secrets (presence only
  // ever comes back), validation, the download job, the report.
  getRomMediaSettings: () =>
    invoke<RomMediaSettingsView>("get_rom_media_settings"),
  setRomMediaSettings: (settings: RomMediaSettings) =>
    invoke<void>("set_rom_media_settings", { settings }),
  setRomMediaSecret: (kind: string, value: string | null) =>
    invoke<string>("set_rom_media_secret", { kind, value }),
  validateRomMediaSource: (kind: string) =>
    invoke<RomMediaValidate>("validate_rom_media_source", { kind }),
  startRomMediaDownload: (req: RomMediaDownloadRequest) =>
    invoke<JobStart>("start_rom_media_download", { req }),
  romMediaReport: (system: string | null) =>
    invoke<RomMediaReportData>("rom_media_report", { system }),
  confirmRomMediaMatch: (req: RomMediaConfirmRequest) =>
    invoke<string>("confirm_rom_media_match", { req }),
  romMediaArt: (itemIds: string[]) =>
    invoke<Record<string, string>>("rom_media_art", { itemIds }),
  // ROM Home: shelf queries + per-game personal state. All reads
  // are catalog-only; writes are path-keyed so they survive re-index.
  romHomeGames: (platform?: string | null) =>
    invoke<RomHomeGame[]>("rom_home_games", { platform: platform ?? null }),
  romHomeCounts: () => invoke<RomHomeCounts>("rom_home_counts"),
  romHomeShelfWindow: (spec: RomHomeShelfSpec) =>
    invoke<RomHomeShelfPage>("rom_home_shelf_window", { spec }),
  romHomeMissingPairs: (spec: RomHomeShelfSpec) =>
    invoke<RomHomeMissingPair[]>("rom_home_missing_pairs", { spec }),
  romHomeGenreFacets: () =>
    invoke<Array<[string, number]>>("rom_home_genre_facets"),
  romHomeSetRating: (providerKey: string, rating: number | null) =>
    invoke<void>("rom_home_set_rating", { providerKey, rating }),
  romHomeSetFavorite: (providerKey: string, on: boolean) =>
    invoke<void>("rom_home_set_favorite", { providerKey, on }),
  romHomeSetCompleted: (providerKey: string, on: boolean) =>
    invoke<void>("rom_home_set_completed", { providerKey, on }),
  romHomeSetOverrides: (
    providerKey: string,
    titleOverride: string | null,
    genre: string | null,
    players: string | null,
    notes: string | null,
  ) =>
    invoke<void>("rom_home_set_overrides", {
      providerKey,
      titleOverride,
      genre,
      players,
      notes,
    }),
  romHomeNotePlay: (providerKey: string) =>
    invoke<RomGameState>("rom_home_note_play", { providerKey }),
  romHomeState: (providerKey: string) =>
    invoke<RomGameState>("rom_home_state", { providerKey }),
  romCompleteness: (formatPrefs: Record<string, string>) =>
    invoke<RomCompleteness>("rom_completeness", { formatPrefs }),
  romMissingList: (system: string, formatPrefs: Record<string, string>) =>
    invoke<RomMissingList>("rom_missing_list", { system, formatPrefs }),
  romCompletenessExport: (
    scope: string | null,
    format: string,
    formatPrefs: Record<string, string>,
  ) =>
    invoke<RomExportResult>("rom_completeness_export", {
      scope,
      format,
      formatPrefs,
    }),
  romHomeSmartLists: () => invoke<RomSmartList[]>("rom_home_smart_lists"),
  romHomeSaveSmartList: (name: string, queryJson: string) =>
    invoke<string>("rom_home_save_smart_list", { name, queryJson }),
  romHomeUpdateSmartList: (id: string, queryJson: string) =>
    invoke<void>("rom_home_update_smart_list", { id, queryJson }),
  // Emulation (SPEC-AMENDMENT, Architecture A): Play resolves
  // (system → emulator/core) in the backend and launches the REAL
  // catalog path. First Play with anything missing returns the
  // ask-once sheet's data instead of launching.
  emuPlay: (providerKey: string, platform: string) =>
    invoke<EmuPlayOutcome>("emu_play", { providerKey, platform }),
  emuSheet: (providerKey: string, platform: string) =>
    invoke<EmuSheet>("emu_sheet", { providerKey, platform }),
  emuInstallAndPlay: (
    providerKey: string,
    platform: string,
    optionId: string,
    remember: boolean,
  ) =>
    invoke<EmuPlayOutcome>("emu_install_and_play", {
      providerKey,
      platform,
      optionId,
      remember,
    }),
  emuGrantAccessAndPlay: (
    providerKey: string,
    platform: string,
    optionId: string,
    remember: boolean,
  ) =>
    invoke<EmuPlayOutcome>("emu_grant_access_and_play", {
      providerKey,
      platform,
      optionId,
      remember,
    }),
  emuRemoveOverride: (appId: string, filesystem: string) =>
    invoke<void>("emu_remove_override", { appId, filesystem }),
  emuEsdeState: () => invoke<EsDeState>("emu_esde_state"),
  emuEsdeInstall: () => invoke<EsDeState>("emu_esde_install"),
  emuEsdeOpen: () => invoke<void>("emu_esde_open"),
  emuOpenEmulator: (platform: string) =>
    invoke<void>("emu_open_emulator", { platform }),
  emuBiosState: () => invoke<EmuBiosSystem[]>("emu_bios_state"),
  emuBiosAdd: (systemId: string) =>
    invoke<EmuBiosAddResult>("emu_bios_add", { systemId }),
  emuPrefs: () => invoke<EmuPrefs>("emu_prefs"),
  emuFleetBadge: () => invoke<EmuFleetBadge>("emu_fleet_badge"),
  emuApplyUpdate: (key: string) => invoke<void>("emu_apply_update", { key }),
  emuApplyAllUpdates: () => invoke<string[]>("emu_apply_all_updates"),
  emuSetChoice: (platform: string, optionId: string) =>
    invoke<void>("emu_set_choice", { platform, optionId }),
  emuSetView: (enabled: boolean, root: string | null) =>
    invoke<EmuViewState>("emu_set_view", { enabled, root }),
  emuRebuildView: () => invoke<EmuViewState>("emu_rebuild_view"),
  // Media Resolution (film/TV → Plex/Emby/Jellyfin): TMDB key lives
  // in the OS keyring — only its presence ever comes back. Scan is
  // parse + match + compliance; Organize is a journaled job planned
  // from the same deterministic plan the preview shows.
  getMediaresSettings: () =>
    invoke<MediaresSettingsView>("mediares_get_settings"),
  setMediaresKey: (value: string | null) =>
    invoke<string>("mediares_set_key", { value }),
  validateMediaresKey: (key?: string | null) =>
    invoke<MediaresValidate>("mediares_validate_key", { key: key ?? null }),
  mediaresScan: (req: MediaresScanRequest) =>
    invoke<MediaresScanResult>("mediares_scan", { req }),
  mediaresConfirm: (req: MediaresConfirmRequest) =>
    invoke<MediaresIdentity>("mediares_confirm", { req }),
  mediaresPlanOrganize: (req: MediaresScanRequest) =>
    invoke<MediaresOrganizePlan>("mediares_plan_organize", { req }),
  mediaresStartOrganize: (req: MediaresScanRequest) =>
    invoke<JobStart>("mediares_start_organize", { req }),
  // ROM Organize (Organize parity, 2026-10-16): same plan/start
  // shape as the media-server organize; the plan is the preview.
  romPlanOrganize: (req: MediaresScanRequest) =>
    invoke<RomOrganizePlan>("rom_plan_organize", { req }),
  romStartOrganize: (req: MediaresScanRequest) =>
    invoke<JobStart>("rom_start_organize", { req }),
  // Media Completion: artwork + OpenSubtitles subtitles + the
  // background scheduler. Secrets go in one-way (only presence and
  // validation results come back); runs are the morning reports.
  getMcSettings: () =>
    invoke<McSettings>("mediacomplete_get_settings"),
  saveMcSettings: (settings: McSettings) =>
    invoke<McSettings>("mediacomplete_save_settings", { settings }),
  getMcStatus: () => invoke<McStatus>("mediacomplete_get_status"),
  setMcAgent: (enabled: boolean) =>
    invoke<string>("mediacomplete_set_agent", { enabled }),
  opensubsSetSecret: (kind: string, value: string | null) =>
    invoke<OpensubsCreds>("opensubs_set_secret", { kind, value }),
  opensubsValidate: () =>
    invoke<OpensubsAccount>("opensubs_validate"),
  mcFetchArtwork: (req: McArtworkRequest) =>
    invoke<McFetchResult>("mediacomplete_fetch_artwork_now", { req }),
  mcFetchSubtitle: (req: McSubtitleRequest) =>
    invoke<McFetchResult>("mediacomplete_fetch_subtitle_now", { req }),
  mcRunNow: (scheduleId: string) =>
    invoke<McRun>("mediacomplete_run_now", { scheduleId }),
  mcListRuns: () => invoke<McRun[]>("mediacomplete_list_runs"),
  mcUndoRun: (runId: string) =>
    invoke<string>("mediacomplete_undo_run", { runId }),
  mcConfirmPending: (
    runId: string,
    path: string,
    choice: MediaresConfirmRequest["choice"],
  ) =>
    invoke<MediaresIdentity>("mediacomplete_confirm_pending", {
      runId,
      path,
      choice,
    }),
  // Music Completion: MusicBrainz tag enrichment + cover art +
  // LRCLIB lyrics, journaled runs, and schedules that ride the
  // same background agent as Media Completion.
  getMuSettings: () =>
    invoke<MuSettings>("musiccomplete_get_settings"),
  saveMuSettings: (settings: MuSettings) =>
    invoke<MuSettings>("musiccomplete_save_settings", { settings }),
  getMuStatus: () => invoke<MuStatus>("musiccomplete_get_status"),
  setMuAcoustidKey: (value: string | null) =>
    invoke<MuStatus>("musiccomplete_set_acoustid_key", { value }),
  muInstallFpcalc: () => invoke<void>("musiccomplete_install_fpcalc"),
  muScan: (req: MuScanRequest) =>
    invoke<MuScanView>("musiccomplete_scan", { req }),
  muApply: (req: MuApplyRequest) =>
    invoke<MuRun>("musiccomplete_apply", { req }),
  muLyricsGet: (path: string, fetch: boolean) =>
    invoke<MuLyricsView>("musiccomplete_lyrics_get", { path, fetch }),
  muLyricsSave: (path: string, text: string | null) =>
    invoke<string>("musiccomplete_lyrics_save", { path, text }),
  muRunNow: (scheduleId: string) =>
    invoke<MuRun>("musiccomplete_run_now", { scheduleId }),
  muListRuns: () => invoke<MuRun[]>("musiccomplete_list_runs"),
  muUndoRun: (runId: string) =>
    invoke<string>("musiccomplete_undo_run", { runId }),
  muConfirmPending: (runId: string, path: string) =>
    invoke<string>("musiccomplete_confirm_pending", { runId, path }),
  // File Auditor: settings + schedules, runs (summaries), flags
  // (the checklist), resolutions, and the memory managers
  // (dismissals / standing rules / taught words).
  getAuditorSettings: () =>
    invoke<AuditorSettings>("auditor_get_settings"),
  saveAuditorSettings: (settings: AuditorSettings) =>
    invoke<AuditorSettings>("auditor_save_settings", { settings }),
  auditorRunNow: (target: string, scheduleId: string | null) =>
    invoke<AuditorRun>("auditor_run_now", { target, scheduleId }),
  auditorListRuns: () => invoke<AuditorRun[]>("auditor_list_runs"),
  auditorListFlags: (stateFilter: string | null) =>
    invoke<AuditorFlag[]>("auditor_list_flags", { stateFilter }),
  auditorOpenCount: () => invoke<number>("auditor_open_count"),
  auditorResolve: (
    flagId: string,
    resolution: string,
    scope: string | null,
  ) =>
    invoke<string>("auditor_resolve", { flagId, resolution, scope }),
  auditorClearFindings: () => invoke<string>("auditor_clear_findings"),
  auditorListDismissals: () =>
    invoke<AuditorDismissal[]>("auditor_list_dismissals"),
  auditorDeleteDismissal: (path: string, family: string) =>
    invoke<void>("auditor_delete_dismissal", { path, family }),
  auditorListRules: () => invoke<AuditorRule[]>("auditor_list_rules"),
  auditorDeleteRule: (classKey: string) =>
    invoke<void>("auditor_delete_rule", { classKey }),
  auditorListUserWords: () =>
    invoke<string[]>("auditor_list_user_words"),
  auditorAddUserWord: (word: string) =>
    invoke<string[]>("auditor_add_user_word", { word }),
  pauseJob: (job: string) => invoke<void>("pause_job", { job }),
  resumeJob: (job: string) => invoke<void>("resume_job", { job }),
  cancelJob: (job: string) => invoke<void>("cancel_job", { job }),
  listJobs: () => invoke<JobSummary[]>("list_jobs"),
  getJobOutcomes: (job: string) =>
    invoke<JobOutcome[]>("get_job_outcomes", { job }),
  /** Disk space for the volume backing a path (inc-2 item 12). */
  getDiskInfo: (path: string) => invoke<DiskInfo>("get_disk_info", { path }),
  readMetadata: (item: string) =>
    invoke<MetaTag[]>("read_metadata", { item }),
  editMetadata: (item: string, edits: Array<[string, string]>) =>
    invoke<void>("edit_metadata", { item, edits }),
  washMetadata: (item: string, copy: boolean) =>
    invoke<{ path: string; tags_before: number; tags_after: number }>(
      "wash_metadata",
      { item, copy },
    ),
  cleanPhotoCopy: (item: string) =>
    invoke<string>("clean_photo_copy", { item }),
  mountIso: (item: string) => invoke<string>("mount_iso", { item }),
  mountIsoPath: (path: string) =>
    invoke<string>("mount_iso_path", { path }),
  unmountPath: (path: string) => invoke<void>("unmount_path", { path }),
  /** Eject a device location (phone/camera) by its GIO root URI —
   * the udisks path above cannot see GVfs mounts. */
  unmountDevice: (uri: string) => invoke<void>("unmount_device", { uri }),
  listBlockDevices: () =>
    invoke<Array<{ path: string; size: string; model: string; removable: boolean; usb: boolean }>>("list_block_devices"),
  buildIso: (paths: string[], destIso: string, volumeLabel: string) =>
    invoke<string>("build_iso", { paths, destIso, volumeLabel }),
  verifyIso: (isoPath: string, sources: string[]) =>
    invoke<{ checked: number; ok: number; failed: string[] }>("verify_iso", {
      isoPath,
      sources,
    }),
  writeUsb: (isoPath: string, device: string) =>
    invoke<string>("write_usb", { isoPath, device }),
  burnDisc: (isoPath: string) => invoke<string>("burn_disc", { isoPath }),
  trashFilePath: (path: string) =>
    invoke<string>("trash_file_path", { path }),
  checksumItem: (item: string, algo: string) =>
    invoke<string>("checksum_item", { item, algo }),
  createChecksumFile: (item: string, algo: string) =>
    invoke<string>("create_checksum_file", { item, algo }),
  verifyChecksumFile: (item: string) =>
    invoke<Array<{ file: string; ok: boolean; expected: string; actual: string }>>("verify_checksum_file", { item }),
  splitFile: (item: string, partBytes: number) =>
    invoke<string[]>("split_file", { item, partBytes }),
  joinFiles: (item: string) => invoke<string>("join_files", { item }),
  compareDirs: (a: string, b: string) =>
    invoke<Array<{ rel: string; state: string; size_a: number; size_b: number }>>("compare_dirs", { a, b }),
  syncCopy: (srcRoot: string, dstRoot: string, rels: string[]) =>
    invoke<number>("sync_copy", { srcRoot, dstRoot, rels }),
  findDuplicates: (root: string, minSize: number) =>
    invoke<Array<{ size: number; hash: string; paths: string[] }>>("find_duplicates", { root, minSize }),
  rcloneRemotes: () => invoke<string[]>("rclone_remotes"),
  rcloneCreateRemote: (name: string, backend: string, params: Array<[string, string]>, obscure: string[]) =>
    invoke<void>("rclone_create_remote", { name, backend, params, obscure }),
  rcloneDeleteRemote: (name: string) =>
    invoke<void>("rclone_delete_remote", { name }),
  rcloneMount: (name: string, mountPoint: string) =>
    invoke<void>("rclone_mount", { name, mountPoint }),
  rcloneUnmount: (name: string, mountPoint: string) =>
    invoke<void>("rclone_unmount", { name, mountPoint }),
  castRegisterFile: (item: string) =>
    invoke<string>("cast_register_file", { item }),
  castRegisterPath: (path: string) =>
    invoke<string>("cast_register_path", { path }),
  castScan: () =>
    invoke<Array<{ name: string; kind: string; address: string }>>("cast_scan"),
  castPlay: (device: { name: string; kind: string; address: string }, url: string) =>
    invoke<void>("cast_play", { device, url }),
  castStop: (device: { name: string; kind: string; address: string }) =>
    invoke<void>("cast_stop", { device }),
  castPause: (device: { name: string; kind: string; address: string }) =>
    invoke<void>("cast_pause", { device }),
  castResume: (device: { name: string; kind: string; address: string }) =>
    invoke<void>("cast_resume", { device }),
  castSeek: (device: { name: string; kind: string; address: string }, seconds: number) =>
    invoke<void>("cast_seek", { device, seconds }),
  castStatus: (device: { name: string; kind: string; address: string }) =>
    invoke<{ state: string; position: number; duration: number }>("cast_status", { device }),
  castBackends: () => invoke<string[]>("cast_backends"),
  castSetup: () => invoke<string[]>("cast_setup"),
  // Playback setup (Gauntlet G1): the GStreamer plugin set's
  // state, and the door's one install action. `repoName` is
  // "RPM Fusion" / "Packman" on the families whose full codec
  // set lives in a third-party source.
  playbackSetupStatus: () =>
    invoke<{ pluginsPresent: boolean; repoName: string | null; repoEnabled: boolean }>(
      "playback_setup_status",
    ),
  playbackSetupInstall: (withRepo: boolean) =>
    invoke<{ pluginsPresent: boolean; repoName: string | null; repoEnabled: boolean }>(
      "playback_setup_install",
      { withRepo },
    ),
  listInstalledApps: () =>
    invoke<Array<{ name: string; id: string; source: string; desktop_path: string }>>("list_installed_apps"),
  findAppLeftovers: (appId: string, appName: string) =>
    invoke<Array<{ path: string; size: number; category: string }>>("find_app_leftovers", { appId, appName }),
  uninstallApp: (app: { name: string; id: string; source: string; desktop_path: string }, leftovers: string[]) =>
    invoke<string>("uninstall_app", { app, leftovers }),
  listDisks: () => invoke<DiskInfo[]>("list_disks"),
  availableFilesystems: () => invoke<string[]>("available_filesystems"),
  formatPartition: (device: string, fstype: string, label: string) =>
    invoke<void>("format_partition", { device, fstype, label }),
  eraseDisk: (device: string, fstype: string, label: string) =>
    invoke<void>("erase_disk", { device, fstype, label }),
  partitionResizeInfo: (device: string) =>
    invoke<ResizeInfo>("partition_resize_info", { device }),
  resizePartition: (device: string, newBytes: number) =>
    invoke<void>("resize_partition", { device, newBytes }),
  createFolderIn: (dir: string) => invoke<string>("create_folder", { dir }),
  createNewFile: (dir: string, kind: string) =>
    invoke<string>("create_new_file", { dir, kind }),
  listPrinters: () => invoke<PrinterInfo[]>("list_printers"),
  printItem: (item: string, printer: string | null, copies: number) =>
    invoke<string>("print_item", { item, printer, copies }),
  /** Cheat-sheet exports: write base64 bytes to a user-chosen path;
   * resolves to the number of bytes written. */
  writeBinaryFile: (path: string, contentsBase64: string) =>
    invoke<number>("write_binary_file", { path, contentsBase64 }),
  /** Print an arbitrary file (PDF/JPG) through the CUPS flow. */
  printFilePath: (path: string, printer: string | null, copies: number) =>
    invoke<string>("print_file_path", { path, printer, copies }),
  mktorrentStatus: () => invoke<boolean>("mktorrent_status"),
  installMktorrent: () => invoke<void>("install_mktorrent"),
  createTorrent: (
    item: string,
    wash: boolean,
    cleanImages: boolean,
    trackers: string[],
    privateFlag: boolean,
  ) =>
    invoke<TorrentOutcome>("create_torrent", {
      item,
      wash,
      cleanImages,
      trackers,
      privateFlag,
    }),
  movePathTo: (srcPath: string, destDir: string) =>
    invoke<string>("move_path_to", { srcPath, destDir }),
  openUrl: (url: string) => invoke<void>("open_url", { url }),
  getThumbnail: (item: string, fallbackPath?: string | null, large?: boolean) =>
    invoke<string>("get_thumbnail", { item, fallbackPath: fallbackPath ?? null, large: large ?? false }),
  moveItemTo: (item: string, destDir: string) =>
    invoke<string>("move_item_to", { item, destDir }),
  getItemProperties: (item: string) =>
    invoke<ItemProperties>("get_item_properties", { item }),
  emptyTrash: () =>
    invoke<{ deleted: number; items: number; files: number; errors: number }>("empty_trash"),
  secureEmptyTrash: () =>
    invoke<{ deleted: number; items: number; files: number; errors: number }>("secure_empty_trash"),
  trashSummary: () => invoke<{ items: number; files: number }>("trash_summary"),
  distroInfo: () => invoke<{ os_id: string; desktop: string }>("distro_info"),
  listSyncSchedules: () => invoke<SyncSchedule[]>("list_sync_schedules"),
  saveSyncSchedule: (a: string, b: string, direction: string, intervalMin: number) =>
    invoke<SyncSchedule>("save_sync_schedule", { a, b, direction, intervalMin }),
  deleteSyncSchedule: (id: string) => invoke<void>("delete_sync_schedule", { id }),
  // Folder Sync pairs (two-way, manifest engine)
  syncListPairs: () =>
    invoke<SyncPairView[]>("sync_list_pairs"),
  syncSavePair: (pair: SyncPair) =>
    invoke<SyncPairView[]>("sync_save_pair", { pair }),
  syncDeletePair: (id: string) =>
    invoke<SyncPairView[]>("sync_delete_pair", { id }),
  syncPlan: (pairId: string) =>
    invoke<SyncPlanView>("sync_plan", { pairId }),
  syncStart: (pairId: string) =>
    invoke<{ jobId: string; alreadyRunning: boolean }>("sync_start", { pairId }),
  syncLastReport: (pairId: string) =>
    invoke<SyncReport | null>("sync_last_report", { pairId }),
  openWithApps: (item: string) =>
    invoke<{ mime: string; apps: Array<{ id: string; name: string; icon: string | null }>; default: string | null }>("open_with_apps", { item }),
  openWithApp: (item: string, desktopId: string) =>
    invoke<void>("open_with_app", { item, desktopId }),
  setDefaultApp: (mime: string, desktopId: string) =>
    invoke<void>("set_default_app", { mime, desktopId }),
  getItemPath: (item: string) =>
    invoke<string>("get_item_path", { item }),
  getImagePreview: (item: string, fallbackPath?: string | null) =>
    invoke<string>("get_image_preview", { item, fallbackPath: fallbackPath ?? null }),
  getRecentFiles: (limit?: number) =>
    invoke<Array<{ path: string; name: string; modified: string }>>("get_recent_files", { limit }),
  previewText: (item: string, maxLines?: number) =>
    invoke<{ lines: string[]; truncated: boolean }>("preview_text", { item, maxLines }),
  /** Phase 4: rich document preview — rendered first page (PDF) or
   * cover (EPUB), book metadata, opening text, degradation note. */
  previewDocument: (item: string) =>
    invoke<{
      format: string;
      title: string | null;
      author: string | null;
      detail: string | null;
      image: string | null;
      lines: string[];
      truncated: boolean;
      note: string | null;
    }>("preview_document", { item }),
  startIndexing: (location: string) =>
    invoke<unknown>("start_indexing", { location }),
  indexStatus: () => invoke<IndexState>("index_status"),
  /** Library Memory (Indexer v2): remembered volumes + retention. */
  getLibrarySettings: () => invoke<LibrarySettings>("get_library_settings"),
  setLibrarySettings: (settings: LibrarySettings) =>
    invoke<LibrarySettings>("set_library_settings", { settings }),
  listVolumes: () => invoke<VolumeSummary[]>("list_volumes"),
  /** Purge a disconnected drive's remembered files. Resolves with
   *  the number of catalog rows forgotten. */
  forgetVolume: (fsUuid: string) => invoke<number>("forget_volume", { fsUuid }),
  /** Purge a connected volume's rows + reset resume state so the
   *  next walk is a true full walk. Resolves with its mount point. */
  rebuildVolume: (fsUuid: string) => invoke<string>("rebuild_volume", { fsUuid }),
  /** Phase 6 — WASM plugins (spec §§17–18). */
  listPlugins: () => invoke<PluginInfo[]>("list_plugins"),
  setPluginEnabled: (key: string, enabled: boolean) =>
    invoke<void>("set_plugin_enabled", { key, enabled }),
  pluginsFolder: () => invoke<string>("plugins_folder"),
  runPluginAction: (key: string, path: string) =>
    invoke<PluginRun>("run_plugin_action", { key, path }),
  confirmPluginProposal: (
    pluginKey: string,
    pluginName: string,
    targetDir: string,
    name: string,
    contents: string,
  ) =>
    invoke<JobStart>("confirm_plugin_proposal", {
      pluginKey,
      pluginName,
      targetDir,
      name,
      contents,
    }),
  /** Phase 6 — folder templates (spec §22). */
  listFolderTemplates: () => invoke<FolderTemplateInfo[]>("list_folder_templates"),
  createFolderTemplate: (name: string, fromPath: string | null) =>
    invoke<CreateTemplateResult>("create_folder_template", { name, fromPath }),
  renameFolderTemplate: (id: string, name: string) =>
    invoke<void>("rename_folder_template", { id, name }),
  deleteFolderTemplate: (id: string) =>
    invoke<void>("delete_folder_template", { id }),
  applyFolderTemplate: (templateId: string, parentPath: string, name: string) =>
    invoke<JobStart>("apply_folder_template", { templateId, parentPath, name }),
  /** Phase 6 — ebook reader (spec §25, EPUB). */
  openBook: (path: string) => invoke<BookOpen>("open_book", { path }),
  readChapter: (path: string, index: number) =>
    invoke<ChapterView>("read_chapter", { path, index }),
  setReadingProgress: (
    path: string,
    chapter: number,
    fontScale?: number,
    scrollFrac?: number,
  ) =>
    invoke<void>("set_reading_progress", { path, chapter, fontScale, scrollFrac }),
  /** PDF Reader — paginated documents in the same shell. */
  openPdf: (path: string) => invoke<PdfOpen>("open_pdf", { path }),
  readPdfPage: (path: string, page: number, widthPx: number) =>
    invoke<PdfPageView>("read_pdf_page", { path, page, widthPx }),
  setPdfProgress: (path: string, page: number, zoomMode?: string) =>
    invoke<void>("set_pdf_progress", { path, page, zoomMode }),
  openItem: (item: string) => invoke<void>("open_item", { item }),
  setExecutable: (item: string, executable: boolean) => invoke<void>("set_executable", { item, executable }),
};

export function onIndexProgress(cb: (s: IndexState) => void): Promise<UnlistenFn> {
  return listen<IndexState>("index-progress", (e) => cb(e.payload));
}

/** Job request payloads (seam-guarded): these interfaces mirror the
 * Rust request structs field-for-field. The wrappers above build them
 * as annotated literals, so dropping or renaming a key is a tsc
 * compile error here — and the Rust fixture test
 * `seam_job_requests_accept_frontend_camel_case` pins the wire shape
 * on the other side. (2026-10-14: all six start_* commands silently
 * rejected every call when the structs lost their camelCase rename.) */
export interface CopyRequest {
  sources: string[];
  dstParent: string;
}
export interface MoveRequest {
  sources: string[];
  dstDir: string;
}
export interface MovePathsRequest {
  sources: string[];
  dstDir: string;
}
export interface CopyPathsRequest {
  sources: string[];
  dstDir: string;
}
export interface DeleteRequest {
  sources: string[];
  useTrash: boolean;
}
export interface DeletePathsRequest {
  paths: string[];
  useTrash: boolean;
}

/** ROM identification pass result (identify_roms). */
export interface RomIdentifyStats {
  alreadyRunning: boolean;
  candidates: number;
  hashed: number;
  matched: number;
  fresh: number;
  errors: number;
  elapsedMs: number;
  filesPerSec: number;
}

export interface RomIdentifyProgress {
  done: number;
  total: number;
  matched: number;
}

/** EmulationStation import (Hyperbase #5) — mirrors the
 * ufm-catalog Es* report structs field-for-field. */
export interface EsCounters {
  entries: number;
  folderCount: number;
  noPath: number;
  unresolvable: number;
  duplicates: number;
  matchedExact: number;
  matchedCasefold: number;
  matchedOffline: number;
  unmatchedNotIndexed: number;
  unmatchedMissingFile: number;
  datKept: number;
  identityWritten: number;
  identityRefreshed: number;
  identityFilled: number;
  identityNoName: number;
  notRom: number;
  artFilled: number;
  artKept: number;
  stateUpdates: number;
}

export interface EsSystemReport {
  esSystem: string;
  platform: string;
  gamelistPath: string;
  parseError: string | null;
  counts: EsCounters;
}

export interface EsImportReport {
  alreadyRunning: boolean;
  rootsSearched: number;
  rootsWithEs: number;
  systems: EsSystemReport[];
  totals: EsCounters;
}

/** Per-collection scope (Dickles 2026-10-14): all drives (default),
 * the boot drive only, or a chosen set of drives (location ids). */
export interface RomScope {
  mode: "all" | "boot" | "drives";
  drives: string[];
}

/** ROM media enrichment (mirrors rommedia.rs). */
export interface RomMediaTypeSettings {
  enabled: boolean;
  dirOverride: string | null;
}

export interface RomMediaSettings {
  mediaRoot: string;
  retroarchDir: string | null;
  boxart: RomMediaTypeSettings;
  screenshots: RomMediaTypeSettings;
  titles: RomMediaTypeSettings;
  trailers: RomMediaTypeSettings;
  trailerTier: string;
  screenscraperUser: string | null;
  emumoviesUser: string | null;
}

export interface RomMediaSettingsView {
  settings: RomMediaSettings;
  secretPresent: Record<string, boolean>;
  keyringOk: boolean;
  ssDevReady: boolean;
}

export interface RomMediaValidate {
  ok: boolean;
  message: string;
}

export interface RomMediaDownloadRequest {
  systems?: string[] | null;
  systemLabel?: string | null;
  retry?: Array<{ itemId: string; mediaType: string }>;
}

export interface RomMediaConfirmRequest {
  itemId: string;
  mediaType: string;
  source: string;
  url: string;
}

/** Media Resolution (mirrors mediares.rs / ufm-media videoid). */
export interface MediaresSettingsView {
  hasKey: boolean;
  keyringOk: boolean;
}

export interface MediaresValidate {
  ok: boolean;
  message: string;
}

export interface MediaresScanRequest {
  root: string;
  paths: string[];
}

/** videoid::ParsedVideo — serde field names are the Rust ones. */
export interface MediaresParsed {
  kind: "movie" | "episode" | "unknown";
  title: string;
  year: number | null;
  season: number | null;
  episode_first: number | null;
  episode_last: number | null;
  part: string | null;
  episode_title_hint: string | null;
  absolute_episode: number | null;
  air_date: string | null;
  explicit_tmdb_id: number | null;
  explicit_imdb_id: string | null;
  edition: string | null;
  tags: string[];
  flags: string[];
}

export interface MediaresIdentity {
  mediaType: string;
  tmdbId: number | null;
  title: string;
  year: number | null;
  season: number | null;
  episode: number | null;
  episodeEnd: number | null;
  confidence: number | null;
  confirmed: boolean;
  source: string;
  episodeTitles: string[];
}

export interface MediaresCandidate {
  candidate: {
    id: number;
    media_type: string;
    title: string;
    original_title: string;
    year: number | null;
    popularity: number;
  };
  score: number;
  tier: "confident" | "probable" | "unknown";
  reasons: string[];
}

export interface MediaresScanRow {
  path: string;
  name: string;
  itemId: string | null;
  parsed: MediaresParsed;
  state: "ready" | "fixable" | "attention";
  identity: MediaresIdentity | null;
  candidates: MediaresCandidate[];
  targetRel: string | null;
  note: string | null;
}

export interface MediaresScanResult {
  root: string;
  hasKey: boolean;
  rows: MediaresScanRow[];
  ready: number;
  fixable: number;
  attention: number;
  truncated: boolean;
}

// ---------------------------------------------------------------------
// Media Completion types (mirror mediacomplete.rs / opensubs.rs)
// ---------------------------------------------------------------------

export interface McSchedule {
  id: string;
  name: string;
  target: string;
  steps: { organize: boolean; artwork: boolean; subtitles: boolean };
  time: string;
  weekdays: boolean[];
  idleMinutes: number;
  requireOnAc: boolean;
  enabled: boolean;
  lastStarted: string | null;
  lastStatus: string | null;
}

export interface McSettings {
  artworkOnOrganize: boolean;
  fanart: boolean;
  subtitlesOnOrganize: boolean;
  subtitleLanguage: string;
  agentEnabled: boolean;
  schedules: McSchedule[];
}

export interface OpensubsCreds {
  hasApiKey: boolean;
  hasLogin: boolean;
  keyringOk: boolean;
}

export interface OpensubsAccount {
  ok: boolean;
  message: string;
  account: string | null;
  loggedIn: boolean;
}

export interface McStatus {
  agentRunning: boolean;
  agentPid: number | null;
  agentLastTick: string | null;
  agentInstall: string;
  nextRuns: [string, string][];
  opensubs: OpensubsCreds;
}

export interface OpensubsQuota {
  remaining: number | null;
  requests: number | null;
  resetTime: string | null;
  message: string | null;
}

export interface McFetchResult {
  outcome: "done" | "skipped" | "failed" | "quota";
  message: string;
  placed: string[];
  quota: OpensubsQuota | null;
}

export interface McArtworkRequest {
  path: string;
  mediaType: string;
  tmdbId: number;
  season: number | null;
}

export interface McSubtitleRequest {
  path: string;
  mediaType: string;
  tmdbId: number | null;
  season: number | null;
  episode: number | null;
  language: string | null;
}

export interface McRunSummary {
  scanned?: number;
  truncated?: boolean;
  organized?: number;
  artworkPlaced?: number;
  subtitlesPlaced?: number;
  pending?: number;
  failures?: string[];
  error?: string;
}

export interface McPendingItem {
  path: string;
  name: string;
  parsed: MediaresParsed;
  identity: MediaresIdentity | null;
  candidates: MediaresCandidate[];
  note: string | null;
}

export interface McRun {
  id: string;
  scheduleName: string;
  target: string;
  started: string;
  finished: string | null;
  wasLate: boolean;
  status: string;
  summary: McRunSummary;
  pending: McPendingItem[];
  quota: OpensubsQuota | null;
  canUndo: boolean;
}

// ---------------------------------------------------------------------
// Music Completion types (mirror musiccomplete.rs / musicid.rs)
// ---------------------------------------------------------------------

export interface MuSchedule {
  id: string;
  name: string;
  target: string;
  steps: { tags: boolean; lyrics: boolean; artwork: boolean };
  time: string;
  weekdays: boolean[];
  idleMinutes: number;
  requireOnAc: boolean;
  enabled: boolean;
  lastStarted: string | null;
  lastStatus: string | null;
}

export interface MuSettings {
  /** "embedded" | "sidecar" | "both" */
  lyricsTarget: string;
  fetchLyrics: boolean;
  fetchArtwork: boolean;
  schedules: MuSchedule[];
}

export interface MuStatus {
  keyringOk: boolean;
  acoustidKeySaved: boolean;
  fpcalcAvailable: boolean;
  fingerprintingReady: boolean;
}

export interface MuAudioTags {
  title: string | null;
  artist: string | null;
  album: string | null;
  albumArtist: string | null;
  year: string | null;
  track: string | null;
  trackTotal: string | null;
  disc: string | null;
  discTotal: string | null;
  genre: string | null;
  composer: string | null;
}

export interface MuCandidateCore {
  id: string;
  title: string;
  artist: string;
  album: string | null;
  albumArtist: string | null;
  releaseId: string | null;
  releaseGroupId: string | null;
  year: number | null;
  track: number | null;
  trackTotal: number | null;
  disc: number | null;
  discTotal: number | null;
  genre: string | null;
  composer: string | null;
  durationMs: number | null;
  mbScore: number;
}

export interface MuCandidate {
  candidate: MuCandidateCore;
  score: number;
  tier: string;
  evidence: string[];
}

export interface MuTagChange {
  field: string;
  current: string | null;
  proposed: string;
  overwrite: boolean;
}

export interface MuTrackProposal {
  path: string;
  name: string;
  current: MuAudioTags;
  durationSecs: number | null;
  state: string;
  candidate: MuCandidate | null;
  changes: MuTagChange[];
  hasArt: boolean;
  artAvailable: boolean;
  hasLyrics: boolean;
  lyricsAvailable: boolean;
  fetchedPlain: string | null;
  fetchedSynced: string | null;
  needsWork: boolean;
  note: string | null;
}

export interface MuScanRequest {
  root: string;
  paths: string[];
}

export interface MuScanView {
  root: string;
  rows: MuTrackProposal[];
  truncated: boolean;
  fingerprintingReady: boolean;
  fpcalcAvailable: boolean;
  acoustidKeySaved: boolean;
}

export interface MuApplyField {
  field: string;
  value: string | null;
}

export interface MuApplyItem {
  path: string;
  fields: MuApplyField[];
  artReleaseId: string | null;
  artReleaseGroupId: string | null;
  lyricsPlain: string | null;
  lyricsSynced: string | null;
}

export interface MuApplyRequest {
  target: string;
  items: MuApplyItem[];
}

export interface MuLyricsView {
  path: string;
  embeddedPlain: string | null;
  embeddedSynced: string | null;
  sidecarText: string | null;
  sidecarPath: string;
  fetchedPlain: string | null;
  fetchedSynced: string | null;
  fetchedInstrumental: boolean;
  note: string | null;
}

export interface MuRunSummary {
  filesCompleted?: number;
  fieldsWritten?: number;
  artworkEmbedded?: number;
  lyricsSaved?: number;
  pending?: number;
  failures?: string[];
  error?: string;
}

export interface MuPendingItem {
  path: string;
  name: string;
  state: string;
  score: number | null;
  candidate: {
    title: string;
    artist: string;
    album: string | null;
    year: number | null;
  } | null;
  changes: MuTagChange[];
  artAvailable: boolean;
  lyricsAvailable: boolean;
  note: string | null;
}

export interface MuRun {
  id: string;
  scheduleName: string;
  target: string;
  started: string;
  finished: string | null;
  wasLate: boolean;
  status: string;
  summary: MuRunSummary;
  pending: MuPendingItem[];
  canUndo: boolean;
}

export interface AuditorFamilies {
  typo: boolean;
  pattern: boolean;
  corrupt: boolean;
  /** Opt-in: names that can't travel to Windows/macOS. */
  compat: boolean;
  space: boolean;
  junk: boolean;
  clutter: boolean;
  style: boolean;
  editions: boolean;
  cold: boolean;
}

export interface AuditorSchedule {
  id: string;
  name: string;
  target: string;
  time: string;
  weekdays: boolean[];
  idleMinutes: number;
  requireOnAc: boolean;
  enabled: boolean;
  lastStarted: string | null;
  lastStatus: string | null;
}

export interface AuditorSettings {
  families: AuditorFamilies;
  staleDays: number;
  coldYears: number;
  schedules: AuditorSchedule[];
}

export interface AuditorRunSummary {
  scannedFiles?: number;
  scannedDirs?: number;
  truncated?: boolean;
  notes?: string[];
  families?: Record<string, number>;
  flagsNew?: number;
  flagsCarried?: number;
  flagsSuperseded?: number;
  flagsOpen?: number;
}

export interface AuditorRun {
  id: string;
  scheduleName: string;
  target: string;
  started: string;
  finished: string | null;
  wasLate: boolean;
  status: string;
  summary: AuditorRunSummary;
}

export interface AuditorProposal {
  kind: "none" | "rename" | "trash" | "convert";
  to?: string;
  convertKind?: string;
  dstName?: string;
  replace?: boolean;
  estSaving?: number;
}

export interface AuditorFlag {
  id: string;
  runId: string;
  path: string;
  family: string;
  lane: "certain" | "question";
  classKey: string;
  title: string;
  detail: string;
  proposal: AuditorProposal | null;
  state: string;
  created: string;
}

export interface AuditorDismissal {
  path: string;
  family: string;
  created: string;
}

export interface AuditorRule {
  classKey: string;
  decision: string;
  created: string;
}

export interface MediaresConfirmRequest {
  path: string;
  choice: {
    mediaType: string;
    tmdbId: number | null;
    title: string;
    year: number | null;
    season: number | null;
    episode: number | null;
    episodeEnd: number | null;
    episodeTitles: string[];
  };
}

export interface MediaresOrganizePlan {
  actions: Array<{ src: string; dst: string; rel: string }>;
  excluded: Array<{ path: string; reason: string }>;
}

export interface RomOrganizePlan {
  actions: Array<{ src: string; dst: string; rel: string }>;
  excluded: Array<{ path: string; reason: string }>;
  truncated: boolean;
}

export interface RomMediaCandidate {
  source: string;
  title: string;
  url: string;
}

export interface RomMediaMissingRow {
  itemId: string;
  title: string;
  mediaType: string;
  status: string;
  reason: string;
  candidates: RomMediaCandidate[];
}

export interface RomMediaSystemReport {
  system: string;
  display: string;
  totalGames: number;
  coverage: Array<{ mediaType: string; have: number; total: number }>;
  missing: RomMediaMissingRow[];
  truncatedMissing: number;
  diyFolder: string;
  diyPattern: string;
}

export interface RomMediaReportData {
  systems: RomMediaSystemReport[];
  generatedAt: string;
}

/** One ROM collection member: the catalog item plus its DAT name. */
export interface RomHit {
  item: Item;
  datName: string;
  system: string;
}

/** One item's identified platform (dataset key, e.g. "neogeocd"). */
export interface RomIdentityHit {
  itemId: string;
  platform: string;
}

/** ROM Home Needs-ID shelf: an unidentified ROM candidate, grouped
 * by its parsed filename (tag groups stripped). */
export interface RomNeedsIdEntry {
  item: Item;
  group: string;
  reason: string;
  scanned: boolean;
}

/** What a ROM collection reset actually removed. */
export interface RomResetStats {
  identitiesRemoved: number;
  mediaRemoved: number;
}

/** ROM Home: one game's media presence for one type (inspector chips). */
export interface RomGameMedia {
  mediaType: string;
  found: boolean;
  localPath: string | null;
  source: string | null;
}

/** ROM Home: a game's personal state — the user's relationship
 * with it (rating, favorite, completed, play history, overrides). */
export interface RomGameState {
  rating: number | null;
  favorite: boolean;
  completed: boolean;
  playCount: number;
  lastPlayed: string | null;
  titleOverride: string | null;
  genre: string | null;
  players: string | null;
  notes: string | null;
}

/** ROM Home: one game as the room renders it — catalog truth only. */
/** A game's parsed variant descriptor (Phase 3 grouping): region /
 * revision / status / languages / flags from its DAT title, plus
 * the default-pick ranks. Mirrors ufm-media's RomVariant. */
export interface RomVariantInfo {
  region: string | null;
  revision: string | null;
  status: string;
  languages: string | null;
  flags: string[];
  disc: string | null;
  descriptor: string;
  regionRank: number;
  statusRank: number;
  revisionKey: number;
  neverDefault: boolean;
}

export interface RomHomeGame {
  item: Item;
  /** DAT-verified title (display = state.titleOverride ?? this). */
  title: string;
  platform: string;
  alsoPlatforms: string[];
  year: number | null;
  sha1: string | null;
  crc: string | null;
  region: string | null;
  /** Phase 3: the group this game folds into (one group = one
   * shelf card) and its variant descriptor. */
  groupKey: string;
  variant: RomVariantInfo;
  media: RomGameMedia[];
  state: RomGameState;
  auditorFlagged: boolean;
  volumeLabel: string | null;
  volumeOffline: boolean;
  volumeLastSeen: string | null;
  /** PC Games P1: present on pc-game rows only. */
  pc?: PcGameCard | null;
}

/** PC Games P1: the card facts for one PC game (library it came
 * from, storefronts, verified-install state, launch badge). */
export interface PcGameCard {
  sourceKind: string;
  libraryName: string;
  storefronts: string[];
  /** "present" = verified on a mounted volume; "recorded" = the
   * record exists but the install could not be verified here. */
  installState: string;
  /** not-checked | boots | boots-with-notes | wont-boot |
   * windows-only — moves only on an observed launch or a
   * manual mark (the ladder itself is Phase 2). */
  launchBadge: string;
  installSize: number | null;
  version: string | null;
  exeName: string | null;
  mislabelNote: string | null;
}

export interface PcLink {
  name: string;
  url: string;
}

export interface PcInstallRow {
  providerKey: string;
  title: string;
  libraryName: string;
  sourceKind: string;
  present: boolean;
  launchBadge: string;
  exeName: string | null;
  installSize: number | null;
}

/** The game sheet's PC sections (PC Games P1). */
export interface PcGameSheet {
  providerKey: string;
  title: string;
  year: number | null;
  sourceKind: string;
  libraryName: string;
  playniteId: string | null;
  gameGuid: string | null;
  description: string | null;
  genres: string[];
  developers: string[];
  publishers: string[];
  series: string[];
  features: string[];
  ageRatings: string[];
  links: PcLink[];
  storefronts: string[];
  criticScore: number | null;
  communityScore: number | null;
  installSize: number | null;
  version: string | null;
  exeName: string | null;
  exeRel: string | null;
  releaseRaw: string | null;
  mislabelNote: string | null;
  importedAt: string;
  present: boolean;
  launchBadge: string;
  launchNote: string | null;
  checkedAt: string | null;
  installs: PcInstallRow[];
}

export interface PcImportReport {
  alreadyRunning: boolean;
  libraryPath: string;
  root: string;
  recordsSeen: number;
  imported: number;
  refreshed: number;
  upgraded: number;
  recordedNotPresent: number;
  duplicatesMerged: number;
  artAdopted: number;
  artMissing: number;
  orphanMediaFolders: number;
  mislabelNotes: number;
  skippedPrecedence: number;
  errors: string[];
}

export interface PcImportPreview {
  found: boolean;
  libraryPath: string;
  root: string;
  recordsSeen: number;
  wouldImportPresent: number;
  recordedNotPresent: number;
  duplicatesMerged: number;
  artAvailable: number;
  artMissing: number;
  orphanMediaFolders: number;
  notes: string[];
  errors: string[];
}

export interface PcScanReport {
  alreadyRunning: boolean;
  root: string;
  foldersSeen: number;
  imported: number;
  refreshed: number;
  coveredByPlaynite: number;
  noExeFound: number;
  errors: string[];
}

/** One shelved book (Ebook P1). Mirrors src-tauri/src/books.rs. */
export interface BookCard {
  itemId: string;
  path: string;
  name: string;
  title: string;
  authors: string[];
  authorLine: string;
  series: string | null;
  seriesIndex: number | null;
  year: number | null;
  format: string;
  coverPath: string | null;
  state: string | null;
  publisher: string | null;
  language: string | null;
  subjects: string[];
  description: string | null;
}

export interface BookContinueRow {
  card: BookCard;
  chapter: number | null;
  page: number | null;
  total: number | null;
  fraction: number | null;
  updated: string;
  finishCandidate: boolean;
}

export interface BookSeriesShelf {
  series: string;
  books: BookCard[];
  present: number[];
  missing: number[];
  gapLabel: string | null;
}

export interface BookAuthorShelf {
  author: string;
  books: BookCard[];
}

export interface BooksHomePayload {
  counts: {
    identified: number;
    unsorted: number;
    series: number;
    authors: number;
    want: number;
    reading: number;
    read: number;
  };
  continueReading: BookContinueRow[];
  recentlyAdded: BookCard[];
  series: BookSeriesShelf[];
  authors: BookAuthorShelf[];
  all: BookCard[];
  unsorted: Array<{ path: string; name: string }>;
  unsortedTotal: number;
}

export interface BooksIdentifyReport {
  alreadyRunning: boolean;
  total: number;
  identified: number;
  withSeries: number;
  withCover: number;
  failed: number;
  failures: Array<[string, string]>;
}

/** ROM Home tallies (the sidebar door + the rail health line). */

// ---------------------------------------------------------------------
// Collection Completeness (2026-10-17): owned-vs-reference scores,
// the missing-games list, spreadsheet export.
// ---------------------------------------------------------------------

export interface RomSystemSummary {
  key: string;
  display: string;
  hasReference: boolean;
  usesSets: boolean;
  totalGroups: number;
  ownedGroups: number;
  missingGroups: number;
  totalReleases: number;
  ownedUnmatched: number;
}

export interface RomCompleteness {
  systems: RomSystemSummary[];
  formats: Record<string, string>;
  formatPrefs: string[];
  computedMs: number;
}

export interface RomRefRelease {
  name: string;
  region: string | null;
  crc: number;
  size: number;
  setShort: string | null;
  notes: string;
}

export interface RomRefGroup {
  groupKey: string;
  baseTitle: string;
  releases: RomRefRelease[];
}

export interface RomMissingList {
  key: string;
  display: string;
  format: string;
  groups: RomRefGroup[];
}

export interface RomExportResult {
  cancelled: boolean;
  path: string | null;
  rows: number;
  groups: number;
}

export interface RomHomeCounts {
  total: number;
  missingMedia: number;
  /** [platform, games, missing box art] */
  perSystem: Array<[string, number, number]>;
  /** Hyperbase #4: group counts for the rail's Favorites /
   * Recently Played shelves (the shelf queries' definitions). */
  favoriteGroups: number;
  recentGroups: number;
  /** Per-system byte totals + offline truth (session badges). */
  perSystemExtra: RomHomeSystemExtra[];
  /** PC Games P1: [library name, group count] for the PC
   * section's library sub-shelves. */
  pcLibraries: Array<[string, number]>;
}

export interface RomHomeSystemExtra {
  platform: string;
  sizeBytes: number;
  allOffline: boolean;
  volumeLabel: string | null;
  volumeLastSeen: string | null;
}

/** Games Home shelf filters — the room's RoomFilters vocabulary,
 * judged over the full set in Rust (Hyperbase #4). */
export interface RomHomeShelfFilters {
  systems: string[];
  genres: string[];
  players: string[];
  media: string[];
  flags: string[];
  ignoreSystems: boolean;
}

/** One bounded shelf-window request (Hyperbase #4). kind names
 * the shelf: all | favorites | recent | missing | system |
 * search | smart | list. For smart shelves, `filters` IS the
 * list's rule; otherwise it is the room's toolbar filter set. */
export interface RomHomeShelfSpec {
  kind: string;
  platform?: string | null;
  query?: string | null;
  searchPlatforms: string[];
  memberKeys?: string[] | null;
  /** PC Games P1: library sub-shelf (groups claimed by this
   * library, any member). */
  pcSource?: string | null;
  filters: RomHomeShelfFilters;
  sort: string;
  cursor?: string | null;
  limit: number;
}

export interface RomHomeShelfPage {
  /** Groups matching the spec across the FULL set. */
  total: number;
  /** Member rows of this window's groups (whole groups). */
  games: RomHomeGame[];
  nextCursor: string | null;
}

export interface RomHomeMissingPair {
  itemId: string;
  mediaType: string;
}

/** Emulation: one option's verified state on this machine. */
export type EmuOptState =
  | "ready"
  | "needs-retro-arch"
  | "needs-core"
  | "needs-install"
  | "needs-bios"
  | "manual-only"
  | "not-yet";

/** Emulation: an option's honesty class (curated / also-good /
 * specialist, in the amendment's vocabulary). */
export type EmuOptClass = "default-able" | "best-effort" | "manual-only";

/** Emulation: the sheet's BIOS row — a system file this option
 * needs that isn't there yet (shown before any launch, §6). */
export interface EmuBiosGate {
  /** Display names of the missing files. */
  files: string[];
  note: string;
  /** True when the missing files block this launch. */
  blocking: boolean;
}

/** Emulation: one row of the ask-once sheet / Play with… picker. */
export interface EmuSheetOption {
  id: string;
  label: string;
  verdict: string;
  state: EmuOptState;
  stateWord: string;
  needsInstall: boolean;
  class: EmuOptClass;
  installNote: string | null;
  bios: EmuBiosGate | null;
}

/** Emulation: the sheet's full payload, rendered verbatim. */
export interface EmuSheet {
  gameTitle: string;
  providerKey: string;
  platform: string;
  systemId: string;
  systemName: string;
  shortName: string;
  options: EmuSheetOption[];
  preselect: string;
  /** "ask" (first Play) or "picker" (Play with…). */
  mode: string;
}

/** Emulation: what pressing Play (or the sheet's primary) did. */
export type EmuPlayOutcome =
  | {
      kind: "launched";
      state: RomGameState;
      optionId: string;
      playedProviderKey: string;
    }
  | { kind: "sheet"; sheet: EmuSheet }
  | {
      kind: "access-needed";
      optionId: string;
      appId: string;
      appLabel: string;
      filesystem: string;
      message: string;
    }
  | { kind: "unavailable"; message: string };

/** Emulation: the ES-DE companion's state (the door, v2). */
export interface EsDeState {
  installed: boolean;
  version: string | null;
  path: string;
}

/** Emulation: one recorded sandbox drive-access grant. */
export interface EmuOverride {
  appId: string;
  appLabel: string;
  filesystem: string;
}

/** Emulation: one row of Preferences → ROMs → Emulators. */
export interface EmuPrefsOption {
  id: string;
  label: string;
  verdict: string;
  state: EmuOptState;
  stateWord: string;
  class: EmuOptClass;
}

export interface EmuPrefsSystem {
  platform: string;
  systemId: string;
  name: string;
  shortName: string;
  games: number;
  chosen: string;
  options: EmuPrefsOption[];
}

/** Emulation: one alias-keeper reconciliation report. */
export interface EmuKeeperReport {
  identities: number;
  links: number;
  created: number;
  repointed: number;
  removed: number;
  blocked: string[];
  playlists: string[];
  sets: number;
  m3usWritten: number;
  m3usRemoved: number;
  drives: number;
  error: string | null;
}

/** Emulation: one set of system files on the BIOS card. */
export interface EmuBiosSet {
  what: string;
  names: string[];
  /** True when every name is needed (vs any one of them). */
  all: boolean;
  /** The set's corner of the system folder ("" = the folder itself). */
  subdir: string;
  present: boolean;
  missing: string[];
}

/** Emulation: one system's row on Preferences → ROMs → System files. */
export interface EmuBiosSystem {
  systemId: string;
  title: string;
  need: "required" | "recommended" | "optional";
  satisfied: boolean;
  gatePlay: boolean;
  /** Where the files live, home-relative. */
  home: string;
  sets: EmuBiosSet[];
  note: string;
}

/** Emulation: the Add flow's result (picker → filed copies). */
export interface EmuBiosAddResult {
  cancelled: boolean;
  system: EmuBiosSystem;
  filed: { picked: string; filedAs: string }[];
  rejected: { picked: string; reason: string }[];
}

/** Emulation: the Unified view card's state. */
export interface EmuViewState {
  enabled: boolean;
  root: string;
  games: number;
  drives: number;
  linksOnDisk: number;
  retroarchPresent: boolean;
  lastReport: EmuKeeperReport;
}

export interface EmuPrefs {
  systems: EmuPrefsSystem[];
  view: EmuViewState;
  bios: EmuBiosSystem[];
  esde: EsDeState;
  overrides: EmuOverride[];
  prep: EmuPrepState;
  updates: EmuUpdatesState;
}

/** Emulation v2.1: the fleet prep pass's state (Preferences → ROMs). */
export interface EmuPrepState {
  running: boolean;
  /** Unix seconds of the last completed pass (0 = never). */
  lastRun: number;
  total: number;
  ready: number;
  failed: { label: string; error: string }[];
}

/** Emulation v2.1: one waiting emulator update. */
export interface EmuUpdateItem {
  key: string;
  label: string;
  detail: string;
}

/** Emulation v2.1: the last update check's findings. */
export interface EmuUpdatesState {
  checkedAt: number;
  items: EmuUpdateItem[];
}

/** Emulation v2.1: the ROMs-door pill's data. */
export interface EmuFleetBadge {
  updates: number;
  prepping: boolean;
}

/** Emulation: install progress event payload. */
export interface EmuInstallProgress {
  label: string;
  pct: number | null;
}

export function onEmuInstallProgress(
  cb: (p: EmuInstallProgress) => void,
): Promise<UnlistenFn> {
  return listen<EmuInstallProgress>("emu-install-progress", (e) =>
    cb(e.payload),
  );
}

/** Emulation v2.1: prep progress / update findings changed —
 * the ROMs-door pill refreshes off this. */
export function onEmuFleetChanged(cb: () => void): Promise<UnlistenFn> {
  return listen("emu-fleet-changed", () => cb());
}

/** A mount arrived or left (phone plugged in, drive yanked, a server
 * mounted by another app) — the locations list is stale. */
export function onLocationsChanged(cb: () => void): Promise<UnlistenFn> {
  return listen("locations-changed", () => cb());
}

/** A Smart List: a saved filter whose rule renders as a sentence. */
export interface RomSmartList {
  id: string;
  name: string;
  queryJson: string;
}

export function onRomIdentifyProgress(
  cb: (p: RomIdentifyProgress) => void,
): Promise<UnlistenFn> {
  return listen<RomIdentifyProgress>("rom-identify-progress", (e) =>
    cb(e.payload),
  );
}

export function formatBytes(n: number | null): string {
  if (n === null || n === undefined) return "—";
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let u = 0;
  while (v >= 1024 && u < units.length - 1) {
    v /= 1024;
    u++;
  }
  return `${v.toFixed(v >= 100 ? 0 : 1)} ${units[u]}`;
}

export function formatDate(iso: string | null): string {
  if (!iso) return "—";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "—";
  return d.toLocaleString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}
