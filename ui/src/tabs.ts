/**
 * Tab state: tab bar, per-tab navigation history (back/forward), view mode
 * and sort kept per tab. The DOM lives in shell.ts; this module owns data.
 */
import type { Item, LocationSummary } from "./api";

export type ViewMode = "grid" | "list" | "columns" | "gallery";
export type SortKey = "name" | "size" | "modified";
export type SortDir = 1 | -1;

export interface Crumb {
  dirId: string;
  name: string;
}

export interface DirState {
  locationId: string;
  dirId: string;
  crumbs: Crumb[];
}

export interface Tab {
  id: number;
  history: DirState[];
  hIndex: number;
  view: ViewMode;
  sort: SortKey;
  sortDir: SortDir;
  items: Item[];
  selection: Set<string>;
  anchorIndex: number;
  title: string;
}

let nextTabId = 1;

export function makeTab(loc: LocationSummary): Tab {
  const dirId = loc.root_item;
  return {
    id: nextTabId++,
    history: [{ locationId: loc.id, dirId, crumbs: [{ dirId, name: loc.display_name }] }],
    hIndex: 0,
    view: "grid",
    sort: "name",
    sortDir: 1,
    items: [],
    selection: new Set(),
    anchorIndex: -1,
    title: loc.display_name,
  };
}

export function currentDir(tab: Tab): DirState {
  return tab.history[tab.hIndex];
}

/** Push a new location onto the tab's history (truncates forward branch). */
export function navigate(tab: Tab, next: DirState): void {
  tab.history = tab.history.slice(0, tab.hIndex + 1);
  tab.history.push(next);
  tab.hIndex = tab.history.length - 1;
  tab.selection.clear();
  tab.anchorIndex = -1;
  tab.title = next.crumbs[next.crumbs.length - 1].name;
}

export function canGoBack(tab: Tab): boolean {
  return tab.hIndex > 0;
}

export function canGoForward(tab: Tab): boolean {
  return tab.hIndex < tab.history.length - 1;
}

export function goBack(tab: Tab): boolean {
  if (!canGoBack(tab)) return false;
  tab.hIndex--;
  tab.selection.clear();
  tab.anchorIndex = -1;
  tab.title = currentDir(tab).crumbs[currentDir(tab).crumbs.length - 1].name;
  return true;
}

export function goForward(tab: Tab): boolean {
  if (!canGoForward(tab)) return false;
  tab.hIndex++;
  tab.selection.clear();
  tab.anchorIndex = -1;
  tab.title = currentDir(tab).crumbs[currentDir(tab).crumbs.length - 1].name;
  return true;
}

/** Navigate to a crumb within the current history entry. */
export function goToCrumb(tab: Tab, crumbIndex: number): DirState | null {
  const dir = currentDir(tab);
  if (crumbIndex < 0 || crumbIndex >= dir.crumbs.length) return null;
  const crumbs = dir.crumbs.slice(0, crumbIndex + 1);
  const next: DirState = {
    locationId: dir.locationId,
    dirId: crumbs[crumbs.length - 1].dirId,
    crumbs,
  };
  navigate(tab, next);
  return next;
}

/** Descend into a folder item from the current dir. */
export function descend(tab: Tab, item: Item): DirState {
  const dir = currentDir(tab);
  const next: DirState = {
    // The hit's own location, not the tab's: a search hit may belong to a
    // different location than the tab is browsing.
    locationId: item.location,
    dirId: item.id,
    crumbs: [...dir.crumbs, { dirId: item.id, name: item.name }],
  };
  navigate(tab, next);
  return next;
}

/** Go up one level; returns null when already at the location root. */
export function goUp(tab: Tab): DirState | null {
  const dir = currentDir(tab);
  if (dir.crumbs.length <= 1) return null;
  return goToCrumb(tab, dir.crumbs.length - 2);
}

export function sortedItems(tab: Tab): Item[] {
  const dir = tab.sortDir;
  const key = tab.sort;
  const val = (it: Item): string | number => {
    if (key === "name") return it.name.toLocaleLowerCase();
    if (key === "size") return it.size_bytes ?? -1;
    return it.modified ? Date.parse(it.modified) : -1;
  };
  const foldersFirst = (a: Item, b: Item) =>
    (a.kind === "folder" ? 0 : 1) - (b.kind === "folder" ? 0 : 1);
  return [...tab.items].sort((a, b) => {
    const f = foldersFirst(a, b);
    if (f !== 0) return f;
    const va = val(a);
    const vb = val(b);
    if (va < vb) return -dir;
    if (va > vb) return dir;
    return 0;
  });
}
