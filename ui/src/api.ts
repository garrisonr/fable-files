/**
 * Typed bridge to the Tauri backend (src-tauri/src/commands.rs).
 * Shapes follow the backend contract; every command is thin and typed.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** A mounted root: local dir, cloud account, archive, search… */
export interface LocationSummary {
  id: string;
  provider: string;
  display_name: string;
  /** Item id of the location's root — pass as `dir` to `listDir` to open it.
   *  Real UUID minted by the backend; never synthesize ids on the shell side. */
  root_item: string;
}

export type ItemKind = "file" | "folder" | string;

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

export const api = {
  listLocations: () => invoke<LocationSummary[]>("list_locations"),
  listDir: (location: string, dir: string) =>
    invoke<Item[]>("list_dir", { location, dir }),
  search: (query_text: string, scope: string | null, limit = 50) =>
    invoke<SearchHit[]>("search", { query_text, scope, limit }),
  listThemes: () => invoke<ThemeInfo[]>("list_themes"),
  applyTheme: (theme: string) => invoke<string>("apply_theme", { theme }),
  preview: (item: string) => invoke<PreviewKind>("preview", { item }),
  startIndexing: (location: string) =>
    invoke<unknown>("start_indexing", { location }),
  indexStatus: () => invoke<IndexState>("index_status"),
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
