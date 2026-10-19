// roomnav.ts — the shell's navigation composition rules as pure
// functions (no DOM, no Tauri imports), so the decisions can be
// unit-tested directly (ui/tests/roomnav.test.mjs runs them under
// node --test in the gates job). shell.ts executes these decisions;
// it does not re-derive them.
//
// Why this module exists (2026-10-18, crew Dewey): the Books room
// survived a sidebar click on Movies because every navigation path
// hand-listed which rooms to exit, and two paths forgot Books.
// Hand-listed exits are the bug class; the rule lives here, once.

/** The full-screen rooms that can cover the file views. */
export type RoomKey = "books" | "rom" | "homeBase";

/** Every room there is. File navigation and search results must
 * leave ALL of them — a room is a place you are, and navigating
 * anywhere else means you are not there anymore. */
export const ALL_ROOMS: readonly RoomKey[] = ["books", "rom", "homeBase"];

/** Rooms to evict when the user navigates to a file location
 * (sidebar locations, favorites, crumbs, history). */
export function roomsToExitForFileNavigation(): readonly RoomKey[] {
  return ALL_ROOMS;
}

/** Rooms to evict when search results (a smart folder, a typed
 * query) take the screen. Same law: results replace the room. */
export function roomsToExitForSearchResults(): readonly RoomKey[] {
  return ALL_ROOMS;
}

/**
 * Sidebar law (Dickles, 2026-10-18): each location appears exactly
 * once in the sidebar — the pinned section wins, and any location
 * rendered there is suppressed from FAVORITES. Dedupe is by
 * location identity (the resolved, normalized path), never by
 * name; the surviving favorites keep their order. A favorite
 * whose path could not be resolved (undefined) is kept: identity
 * unproven is not identity duplicated.
 */
export function suppressPinnedFavorites<T>(
  favorites: readonly T[],
  pathOf: (fav: T) => string | undefined,
  pinnedPaths: ReadonlySet<string>
): T[] {
  if (pinnedPaths.size === 0) return [...favorites];
  return favorites.filter((fav) => {
    const p = pathOf(fav);
    return p === undefined || !pinnedPaths.has(p);
  });
}
