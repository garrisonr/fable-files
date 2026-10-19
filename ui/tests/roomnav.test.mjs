// roomnav.test.mjs — regression pins for the shell's navigation
// composition rules (ui/src/roomnav.ts), run under node --test by
// the gates job. The gates job compiles roomnav.ts into
// ui/.test-build/ first (the project's own tsc; the module is
// dependency-free, so no DOM or Tauri types are involved).
//
// Defect pinned (2026-10-18, Dickles live): "If I click books and
// then movies, books doesn't get its ass out of the way" — the
// Books room survived sidebar navigation because each navigation
// path hand-listed its room exits and two paths forgot Books.
// And: "Home is on there twice. If home is pinned, it doesn't
// need to be a favorite."
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  ALL_ROOMS,
  roomsToExitForFileNavigation,
  roomsToExitForSearchResults,
  suppressPinnedFavorites,
} from "../.test-build/roomnav.js";

test("file navigation evicts EVERY room, Books included", () => {
  assert.deepEqual(
    [...roomsToExitForFileNavigation()],
    ["books", "rom", "homeBase"]
  );
});

test("search results evict EVERY room, Books included", () => {
  assert.deepEqual([...roomsToExitForSearchResults()], ["books", "rom", "homeBase"]);
});

test("the eviction sets are the whole room set (no room can be forgotten)", () => {
  assert.deepEqual([...roomsToExitForFileNavigation()], [...ALL_ROOMS]);
  assert.deepEqual([...roomsToExitForSearchResults()], [...ALL_ROOMS]);
  assert.ok(ALL_ROOMS.includes("books"));
});

test("pinned location is suppressed from favorites; survivors keep their order", () => {
  const favs = [
    { name: "Home", path: "/home/richard" },
    { name: "Documents", path: "/home/richard/Documents" },
    { name: "Downloads", path: "/home/richard/Downloads" },
  ];
  const out = suppressPinnedFavorites(
    favs,
    (f) => f.path,
    new Set(["/home/richard"])
  );
  assert.deepEqual(
    out.map((f) => f.name),
    ["Documents", "Downloads"]
  );
});

test("suppression is by location identity (path), never by name", () => {
  // A different folder that merely SHARES the pinned entry's name
  // is a different location and stays.
  const favs = [{ name: "Home", path: "/mnt/elsewhere/home" }];
  const out = suppressPinnedFavorites(
    favs,
    (f) => f.path,
    new Set(["/home/richard"])
  );
  assert.equal(out.length, 1);
});

test("a user-pinned favorite duplicating the pinned location is suppressed too", () => {
  const favs = [
    { name: "My Home", path: "/home/richard" },
    { name: "Pics", path: "/home/richard/Pictures" },
  ];
  const out = suppressPinnedFavorites(
    favs,
    (f) => f.path,
    new Set(["/home/richard"])
  );
  assert.deepEqual(
    out.map((f) => f.name),
    ["Pics"]
  );
});

test("Dell case: every location in PINNED is suppressed from FAVORITES, whatever it is", () => {
  // Dickles' Dell sidebar, 2026-10-18: Files · Videos · Pictures ·
  // Music appeared in BOTH sections — they are built-in favorites
  // AND user pins. The pinned identity set the shell composes is
  // the Home door's path UNION every user pin's path; suppression
  // is generic over that set, never a Home special case.
  const favorites = [
    { name: "Home", path: "/home/richard" },
    { name: "Documents", path: "/home/richard/Documents" },
    { name: "Downloads", path: "/home/richard/Downloads" },
    { name: "Videos", path: "/home/richard/Videos" },
    { name: "Pictures", path: "/home/richard/Pictures" },
    { name: "Music", path: "/home/richard/Music" },
    { name: "Trash", path: "special:trash" },
  ];
  const pinned = new Set([
    "/home/richard", // the Home door
    "/home/richard/Videos", // user pins (the PINNED section)
    "/home/richard/Pictures",
    "/home/richard/Music",
  ]);
  const out = suppressPinnedFavorites(favorites, (f) => f.path, pinned);
  assert.deepEqual(
    out.map((f) => f.name),
    ["Documents", "Downloads", "Trash"]
  );
});

test("a favorite whose path could not be resolved is kept (identity unproven)", () => {
  const favs = [{ name: "Home", path: undefined }];
  const out = suppressPinnedFavorites(
    favs,
    (f) => f.path,
    new Set(["/home/richard"])
  );
  assert.equal(out.length, 1);
});

test("an empty pinned set changes nothing", () => {
  const favs = [
    { name: "Home", path: "/home/richard" },
    { name: "Music", path: "/home/richard/Music" },
  ];
  const out = suppressPinnedFavorites(favs, (f) => f.path, new Set());
  assert.deepEqual(
    out.map((f) => f.name),
    ["Home", "Music"]
  );
});
