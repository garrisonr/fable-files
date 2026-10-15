/**
 * Typed bridge to the Tauri backend (src-tauri/src/commands.rs).
 * Shapes follow the backend contract; every command is thin and typed.
 */
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

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
   *  volumes carry it so the sidebar can eject by path). */
  path: string | null;
  /** Removable media (USB / SD) — the sidebar offers eject. */
  removable: boolean;
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
  /** Library Memory: volume badges for `items`, by item id. */
  badges: ItemVolumeBadge[];
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
  applyLabelRules: (rules: LabelRule[]) =>
    invoke<RuleOutcome[]>("apply_label_rules", { rules }),
  addPathLocation: (name: string, path: string) =>
    invoke<LocationSummary>("add_path_location", { name, path }),
  removePathLocation: (location: string) =>
    invoke<void>("remove_path_location", { location }),
  mountServer: (url: string) => invoke<string>("mount_server", { url }),
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
  romCollectionItems: (system: string, scope: RomScope) =>
    invoke<RomHit[]>("rom_collection_items", { system, scope }),
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
  // Media Resolution (film/TV → Plex/Emby/Jellyfin): TMDB key lives
  // in the OS keyring — only its presence ever comes back. Scan is
  // parse + match + compliance; Organize is a journaled job planned
  // from the same deterministic plan the preview shows.
  getMediaresSettings: () =>
    invoke<MediaresSettingsView>("mediares_get_settings"),
  setMediaresKey: (value: string | null) =>
    invoke<string>("mediares_set_key", { value }),
  validateMediaresKey: () =>
    invoke<MediaresValidate>("mediares_validate_key"),
  mediaresScan: (req: MediaresScanRequest) =>
    invoke<MediaresScanResult>("mediares_scan", { req }),
  mediaresConfirm: (req: MediaresConfirmRequest) =>
    invoke<MediaresIdentity>("mediares_confirm", { req }),
  mediaresPlanOrganize: (req: MediaresScanRequest) =>
    invoke<MediaresOrganizePlan>("mediares_plan_organize", { req }),
  mediaresStartOrganize: (req: MediaresScanRequest) =>
    invoke<JobStart>("mediares_start_organize", { req }),
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
  castScan: () =>
    invoke<Array<{ name: string; kind: string; address: string }>>("cast_scan"),
  castPlay: (device: { name: string; kind: string; address: string }, url: string) =>
    invoke<void>("cast_play", { device, url }),
  castStop: (device: { name: string; kind: string; address: string }) =>
    invoke<void>("cast_stop", { device }),
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
