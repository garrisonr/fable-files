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
  openPlayerWindow: (path: string, name: string) =>
    invoke<void>("open_player_window", { path, name }),
  closePlayerWindow: (path: string) =>
    invoke<void>("close_player_window", { path }),
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
  startIndexing: (location: string) =>
    invoke<unknown>("start_indexing", { location }),
  indexStatus: () => invoke<IndexState>("index_status"),
  openItem: (item: string) => invoke<void>("open_item", { item }),
  setExecutable: (item: string, executable: boolean) => invoke<void>("set_executable", { item, executable }),
};

export function onIndexProgress(cb: (s: IndexState) => void): Promise<UnlistenFn> {
  return listen<IndexState>("index-progress", (e) => cb(e.payload));
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
