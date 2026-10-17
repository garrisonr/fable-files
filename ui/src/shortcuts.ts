/**
 * shortcuts.ts — THE shortcut registry. Every keyboard binding in
 * Fable Files is defined exactly once, here. The shell's global key
 * dispatch resolves through this list, the shortcuts overlay renders
 * it, the cheat-sheet exports are drawn from it, and menu labels take
 * their chord hints from it. There is no second list — the previous
 * hand-maintained overlay text was the drift risk this file exists
 * to kill (see ~/workspace/fable-shortcuts/DESIGN.md).
 *
 * Surface-local handlers (file view arrows, the players, ROM Home,
 * the readers, dialogs) keep their own code — they are leaf handlers
 * for bare keys — and are described by entries here naming the
 * handler in DESIGN.md §2. Global bindings are dispatched by
 * resolveGlobalAction below, which mirrors the order and guards of
 * the shell handler it replaced (2026-10-16).
 *
 * Chords are abstract: `primary` is the platform's main modifier —
 * rendered ⌘ on macOS, Ctrl elsewhere — from ONE definition.
 */

export type ModKind = "primary" | "ctrl" | "alt" | "none";

export interface Chord {
  mod: ModKind;
  /** Honored only where declared: true = Shift required,
   * false = Shift forbidden, undefined = don't care. */
  shift?: boolean;
  /** Storage form: single letters lowercase; symbols literal;
   * named keys in KeyboardEvent.key form ("Space", "F2", …). */
  key: string;
}

export type DispatchKind = "shell" | "surface";

export interface ShortcutBinding {
  id: string;
  label: string;
  group: GroupId;
  /** Human surface names, shown on the sheet ("File list", …). */
  surfaces: string[];
  chords: Chord[];
  dispatch: DispatchKind;
  note?: string;
  /** Overrides chord rendering where joining looks wrong
   * (e.g. Delete+Backspace is one ⌫ on a Mac keyboard). */
  display?: { mac: string; other: string };
}

export type GroupId =
  | "files"
  | "navigation"
  | "tabs"
  | "search"
  | "romhome"
  | "players"
  | "reading"
  | "app";

export const SHORTCUT_GROUPS: Array<{ id: GroupId; title: string }> = [
  { id: "files", title: "Files" },
  { id: "navigation", title: "Navigation & Quick Look" },
  { id: "tabs", title: "Tabs & Windows" },
  { id: "search", title: "Search" },
  { id: "romhome", title: "Games Home" },
  { id: "players", title: "Music & Video" },
  { id: "reading", title: "Reading & Viewing" },
  { id: "app", title: "App" },
];

const P = (key: string, shift?: boolean): Chord => ({ mod: "primary", key, shift });
const C = (key: string, shift?: boolean): Chord => ({ mod: "ctrl", key, shift });
const A = (key: string): Chord => ({ mod: "alt", key });
const N = (key: string): Chord => ({ mod: "none", key });

export const SHORTCUTS: ShortcutBinding[] = [
  // ------------------------------------------------------------- Files
  { id: "files.open", label: "Open the selected item", group: "files", surfaces: ["File list"], chords: [N("Enter")], dispatch: "surface" },
  { id: "files.rename", label: "Rename the selected item", group: "files", surfaces: ["File list"], chords: [N("F2")], dispatch: "shell" },
  { id: "files.cut", label: "Cut the selection", group: "files", surfaces: ["File list"], chords: [P("x")], dispatch: "shell" },
  { id: "files.copy", label: "Copy the selection", group: "files", surfaces: ["File list"], chords: [P("c")], dispatch: "shell" },
  { id: "files.paste", label: "Paste into the current folder", group: "files", surfaces: ["File list"], chords: [P("v")], dispatch: "shell" },
  { id: "files.duplicate", label: "Duplicate the selection", group: "files", surfaces: ["File list"], chords: [P("d")], dispatch: "shell" },
  { id: "files.newFolder", label: "New folder here", group: "files", surfaces: ["File list"], chords: [P("n", true)], dispatch: "shell" },
  { id: "files.trash", label: "Move the selection to Trash", group: "files", surfaces: ["File list"], chords: [N("Delete"), N("Backspace")], dispatch: "shell", note: "Backspace deletes — it never navigates back.", display: { mac: "⌫", other: "Del · Backspace" } },
  { id: "files.undo", label: "Undo the last trash or move", group: "files", surfaces: ["File list"], chords: [P("z", false)], dispatch: "shell" },
  { id: "files.selectAll", label: "Select everything in the view", group: "files", surfaces: ["File list"], chords: [P("a")], dispatch: "shell" },
  { id: "files.makeExecutable", label: "Make the selected file executable", group: "files", surfaces: ["File list"], chords: [P("e")], dispatch: "shell" },
  { id: "files.print", label: "Print the selected item", group: "files", surfaces: ["File list"], chords: [P("p")], dispatch: "shell" },
  { id: "files.search", label: "Search this library", group: "files", surfaces: ["File list"], chords: [P("f")], dispatch: "shell", note: "Inside Games Home, this finds a game instead." },

  // ------------------------------------------- Navigation & Quick Look
  { id: "nav.arrows", label: "Move the selection", group: "navigation", surfaces: ["File list"], chords: [N("ArrowUp"), N("ArrowDown"), N("ArrowLeft"), N("ArrowRight")], dispatch: "surface", display: { mac: "↑ ↓ ← →", other: "↑ ↓ ← →" } },
  { id: "nav.extend", label: "Extend the selection", group: "navigation", surfaces: ["File list"], chords: [{ mod: "none", shift: true, key: "ArrowUp" }], dispatch: "surface", display: { mac: "Shift + Arrows", other: "Shift + Arrows" } },
  { id: "nav.firstLast", label: "First / last item", group: "navigation", surfaces: ["File list"], chords: [N("Home"), N("End")], dispatch: "surface" },
  { id: "nav.up", label: "Up one folder", group: "navigation", surfaces: ["Everywhere"], chords: [A("ArrowUp")], dispatch: "shell" },
  { id: "nav.back", label: "Back", group: "navigation", surfaces: ["Everywhere"], chords: [A("ArrowLeft")], dispatch: "shell" },
  { id: "nav.forward", label: "Forward", group: "navigation", surfaces: ["Everywhere"], chords: [A("ArrowRight")], dispatch: "shell" },
  { id: "nav.refresh", label: "Refresh the current view", group: "navigation", surfaces: ["Everywhere"], chords: [N("F5"), P("r")], dispatch: "shell" },
  { id: "nav.path", label: "Type a path", group: "navigation", surfaces: ["Everywhere"], chords: [P("l")], dispatch: "shell" },
  { id: "nav.pathKeys", label: "Path bar: go · cancel", group: "navigation", surfaces: ["Path bar"], chords: [N("Enter"), N("Escape")], dispatch: "surface" },
  { id: "quicklook.toggle", label: "Quick Look the selection", group: "navigation", surfaces: ["File list"], chords: [N("Space")], dispatch: "shell", note: "In the players, Space plays and pauses instead." },
  { id: "quicklook.step", label: "Previous / next item — preview follows", group: "navigation", surfaces: ["Quick Look"], chords: [N("ArrowLeft"), N("ArrowRight")], dispatch: "surface" },

  // ---------------------------------------------------- Tabs & Windows
  { id: "tabs.new", label: "New tab", group: "tabs", surfaces: ["Everywhere"], chords: [P("t")], dispatch: "shell" },
  { id: "tabs.close", label: "Close the tab", group: "tabs", surfaces: ["Everywhere"], chords: [P("w")], dispatch: "shell" },
  { id: "tabs.next", label: "Next tab", group: "tabs", surfaces: ["Everywhere"], chords: [C("Tab")], dispatch: "shell" },
  { id: "tabs.prev", label: "Previous tab", group: "tabs", surfaces: ["Everywhere"], chords: [C("Tab", true)], dispatch: "shell" },

  // -------------------------------------------------------------- Search
  { id: "search.open", label: "Open the highlighted result", group: "search", surfaces: ["Search results"], chords: [N("Enter")], dispatch: "surface" },
  { id: "search.preview", label: "Quick Look the highlighted result", group: "search", surfaces: ["Search results"], chords: [N("Space")], dispatch: "surface" },
  { id: "search.move", label: "Move between results", group: "search", surfaces: ["Search results"], chords: [N("ArrowDown"), N("ArrowUp")], dispatch: "surface", display: { mac: "↑ · ↓", other: "↑ · ↓" } },
  { id: "search.clear", label: "Clear the search", group: "search", surfaces: ["Search field"], chords: [N("Escape")], dispatch: "surface" },
  { id: "search.trash", label: "Trash the selected results", group: "search", surfaces: ["Search results"], chords: [N("Delete"), N("Backspace")], dispatch: "surface", note: "Runs through the same Trash action as the file list.", display: { mac: "⌫", other: "Del · Backspace" } },

  // ------------------------------------------------------------ ROM Home
  { id: "rom.move", label: "Move around the shelf", group: "romhome", surfaces: ["Games Home"], chords: [N("ArrowUp"), N("ArrowDown"), N("ArrowLeft"), N("ArrowRight")], dispatch: "surface", display: { mac: "↑ ↓ ← →", other: "↑ ↓ ← →" } },
  { id: "rom.extend", label: "Add to the selection", group: "romhome", surfaces: ["Games Home"], chords: [{ mod: "none", shift: true, key: "ArrowUp" }], dispatch: "surface", display: { mac: "Shift + Arrows", other: "Shift + Arrows" } },
  { id: "rom.firstLast", label: "First / last game", group: "romhome", surfaces: ["Games Home"], chords: [N("Home"), N("End")], dispatch: "surface" },
  { id: "rom.play", label: "Play the selected game", group: "romhome", surfaces: ["Games Home"], chords: [N("Enter")], dispatch: "surface" },
  { id: "rom.preview", label: "Quick Look the selected game", group: "romhome", surfaces: ["Games Home"], chords: [N("Space")], dispatch: "surface" },
  { id: "rom.find", label: "Find a game", group: "romhome", surfaces: ["Games Home"], chords: [N("/"), P("f")], dispatch: "surface" },
  { id: "rom.exit", label: "Clear selection · leave the room", group: "romhome", surfaces: ["Games Home"], chords: [N("Escape")], dispatch: "surface" },

  // ------------------------------------------------------ Music & Video
  { id: "music.play", label: "Play / pause", group: "players", surfaces: ["Music player"], chords: [N("Space")], dispatch: "surface" },
  { id: "music.seek", label: "Back / forward 5 seconds", group: "players", surfaces: ["Music player"], chords: [N("ArrowLeft"), N("ArrowRight")], dispatch: "surface", display: { mac: "← · →", other: "← · →" } },
  { id: "music.volume", label: "Volume up / down", group: "players", surfaces: ["Music player"], chords: [N("ArrowUp"), N("ArrowDown")], dispatch: "surface", display: { mac: "↑ · ↓", other: "↑ · ↓" } },
  { id: "music.mute", label: "Mute / unmute", group: "players", surfaces: ["Music player"], chords: [N("m")], dispatch: "surface" },
  { id: "music.close", label: "Close the player", group: "players", surfaces: ["Music player"], chords: [N("Escape")], dispatch: "surface" },
  { id: "video.play", label: "Play / pause", group: "players", surfaces: ["Video player"], chords: [N("Space"), N("k")], dispatch: "surface" },
  { id: "video.seek", label: "Back / forward 5 seconds", group: "players", surfaces: ["Video player"], chords: [N("ArrowLeft"), N("ArrowRight")], dispatch: "surface", display: { mac: "← · →", other: "← · →" } },
  { id: "video.fullscreen", label: "Fullscreen", group: "players", surfaces: ["Video player"], chords: [N("f")], dispatch: "surface" },
  { id: "video.mute", label: "Mute / unmute", group: "players", surfaces: ["Video player"], chords: [N("m")], dispatch: "surface" },
  { id: "popout.play", label: "Play / pause (player window)", group: "players", surfaces: ["Player window"], chords: [N("Space")], dispatch: "surface" },
  { id: "popout.seek", label: "Back / forward 5 seconds (player window)", group: "players", surfaces: ["Player window"], chords: [N("ArrowLeft"), N("ArrowRight")], dispatch: "surface", display: { mac: "← · →", other: "← · →" } },

  // -------------------------------------------------- Reading & Viewing
  { id: "reader.page", label: "Previous / next page (eBook)", group: "reading", surfaces: ["eBook reader"], chords: [N("ArrowLeft"), N("ArrowRight")], dispatch: "surface", display: { mac: "← · →", other: "← · →" } },
  { id: "pdf.page", label: "Previous / next page (PDF)", group: "reading", surfaces: ["PDF reader"], chords: [N("ArrowLeft"), N("ArrowRight"), N("PageUp"), N("PageDown")], dispatch: "surface", display: { mac: "← · → · PgUp · PgDn", other: "← · → · PgUp · PgDn" } },
  { id: "pdf.firstLast", label: "First / last page (PDF)", group: "reading", surfaces: ["PDF reader"], chords: [N("Home"), N("End")], dispatch: "surface" },
  { id: "pdf.goToPage", label: "Go to the typed page number", group: "reading", surfaces: ["PDF reader"], chords: [N("Enter")], dispatch: "surface" },
  { id: "reader.close", label: "Close the reader", group: "reading", surfaces: ["Readers"], chords: [N("Escape")], dispatch: "surface" },
  { id: "gallery.move", label: "Previous / next image", group: "reading", surfaces: ["Gallery"], chords: [N("ArrowLeft"), N("ArrowRight")], dispatch: "surface", display: { mac: "← · →", other: "← · →" } },
  { id: "gallery.open", label: "Open the image", group: "reading", surfaces: ["Gallery"], chords: [N("Enter")], dispatch: "surface" },

  // ----------------------------------------------------------------- App
  { id: "app.palette", label: "Command palette", group: "app", surfaces: ["Everywhere"], chords: [P("k")], dispatch: "shell" },
  { id: "app.terminal", label: "Terminal", group: "app", surfaces: ["Everywhere"], chords: [P("`")], dispatch: "shell" },
  { id: "app.preferences", label: "Preferences", group: "app", surfaces: ["Everywhere"], chords: [P(",")], dispatch: "shell" },
  { id: "app.textBigger", label: "Text larger", group: "app", surfaces: ["Everywhere"], chords: [P("=")], dispatch: "shell" },
  { id: "app.textSmaller", label: "Text smaller", group: "app", surfaces: ["Everywhere"], chords: [P("-")], dispatch: "shell" },
  { id: "app.textReset", label: "Reset text size", group: "app", surfaces: ["Everywhere"], chords: [P("0")], dispatch: "shell" },
  { id: "app.shortcuts", label: "This cheat sheet", group: "app", surfaces: ["Everywhere"], chords: [N("?"), P("/"), P("?")], dispatch: "shell" },
  { id: "app.menus", label: "Menus: move · choose · close", group: "app", surfaces: ["Menus"], chords: [N("ArrowUp"), N("Enter"), N("Escape")], dispatch: "surface", display: { mac: "↑ ↓ · Enter · Esc", other: "↑ ↓ · Enter · Esc" } },
  { id: "app.dialogs", label: "Dialogs: confirm · cancel", group: "app", surfaces: ["Dialogs"], chords: [N("Enter"), N("Escape")], dispatch: "surface", display: { mac: "Enter · Esc", other: "Enter · Esc" } },
];

export function bindingById(id: string): ShortcutBinding | undefined {
  return SHORTCUTS.find((b) => b.id === id);
}

// ------------------------------------------------------------ platform

export function isMacPlatform(): boolean {
  if (typeof navigator === "undefined") return false;
  const p = (navigator as { platform?: string; userAgent?: string }).platform ?? "";
  const ua = (navigator as { userAgent?: string }).userAgent ?? "";
  return /Mac|iPhone|iPad/.test(p) || /Mac OS X/.test(ua);
}

const KEY_DISPLAY: Record<string, { mac: string; other: string }> = {
  Space: { mac: "Space", other: "Space" },
  Enter: { mac: "Enter", other: "Enter" },
  Escape: { mac: "Esc", other: "Esc" },
  Delete: { mac: "⌫", other: "Del" },
  Backspace: { mac: "⌫", other: "Backspace" },
  Tab: { mac: "Tab", other: "Tab" },
  Home: { mac: "Home", other: "Home" },
  End: { mac: "End", other: "End" },
  PageUp: { mac: "PgUp", other: "PgUp" },
  PageDown: { mac: "PgDn", other: "PgDn" },
  ArrowUp: { mac: "↑", other: "↑" },
  ArrowDown: { mac: "↓", other: "↓" },
  ArrowLeft: { mac: "←", other: "←" },
  ArrowRight: { mac: "→", other: "→" },
  "=": { mac: "+", other: "+" },
  "-": { mac: "−", other: "−" },
};

export function formatChord(c: Chord, mac: boolean): string {
  const parts: string[] = [];
  if (c.mod === "primary") parts.push(mac ? "⌘" : "Ctrl");
  else if (c.mod === "ctrl") parts.push(mac ? "⌃" : "Ctrl");
  else if (c.mod === "alt") parts.push(mac ? "⌥" : "Alt");
  if (c.shift) parts.push(mac ? "⇧" : "Shift");
  const kd = KEY_DISPLAY[c.key];
  const key = kd ? (mac ? kd.mac : kd.other) : c.key.length === 1 ? c.key.toUpperCase() : c.key;
  parts.push(key);
  return mac ? parts.join("") : parts.join("+");
}

export function formatBinding(b: ShortcutBinding, mac: boolean): string {
  if (b.display) return mac ? b.display.mac : b.display.other;
  return b.chords.map((c) => formatChord(c, mac)).join(" · ");
}

// ------------------------------------------------------------- matching

export interface KeyEventLike {
  key: string;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
}

export function matchChord(e: KeyEventLike, c: Chord): boolean {
  const modOk =
    c.mod === "primary"
      ? e.ctrlKey || e.metaKey
      : c.mod === "ctrl"
        ? e.ctrlKey && !e.metaKey
        : c.mod === "alt"
          ? e.altKey && !e.ctrlKey && !e.metaKey
          : !e.ctrlKey && !e.metaKey && !e.altKey;
  if (!modOk) return false;
  if (c.shift === true && !e.shiftKey) return false;
  if (c.shift === false && e.shiftKey) return false;
  if (c.key.length === 1 && /[a-z]/i.test(c.key)) {
    return e.key.toLowerCase() === c.key.toLowerCase();
  }
  return e.key === c.key;
}

// -------------------------------------------------- global dispatch

export interface GlobalKeyContext {
  /** Focus is in an <input> or <textarea>. */
  inField: boolean;
  /** Focus is in a contentEditable element. */
  inEditable: boolean;
  /** Tag name of the event target ("" if unknown). */
  targetTag: string;
  /** The search results view is on screen. */
  searchActive: boolean;
  /** ROM Home is the active room (its own Mod+F find wins). */
  romHomeActive: boolean;
  /** The current view has a non-empty selection. */
  hasSelection: boolean;
  /** There is an active tab. */
  hasTab: boolean;
}

/**
 * Resolve a keydown to a registry binding id, or null. The order and
 * guards reproduce the shell's previous hand-written chain exactly,
 * with the 2026-10-16 additions slotted into their families:
 * overlay doors (?, Mod+/), Mod+F search, Mod+L path, Mod+, prefs,
 * Ctrl+Tab cycling, Mod+D duplicate, Mod+Shift+N new folder,
 * Mod+P print, Alt+←/→ history. Esc is NOT resolved here — the
 * shell keeps its layered Esc chain as the fallback when this
 * returns null.
 */
export function resolveGlobalAction(e: KeyEventLike, ctx: GlobalKeyContext): string | null {
  const prim = e.ctrlKey || e.metaKey;
  const lower = e.key.toLowerCase();

  // Overlay doors — first, like the old Ctrl+? branch.
  if (prim && (e.key === "?" || e.key === "/")) return "app.shortcuts";
  if (e.key === "?" && !prim && !e.altKey && !ctx.inField && !ctx.inEditable) {
    return "app.shortcuts";
  }
  if (prim && lower === "k") return "app.palette";
  if (prim && e.key === ",") return "app.preferences";
  if (prim && lower === "f" && !ctx.romHomeActive) return "files.search";
  if (prim && lower === "l" && !ctx.inField) return "nav.path";
  if (prim && lower === "t") return "tabs.new";
  if (prim && lower === "w") return "tabs.close";
  if (e.ctrlKey && !e.metaKey && e.key === "Tab") {
    return e.shiftKey ? "tabs.prev" : "tabs.next";
  }
  if (prim && lower === "e") return "files.makeExecutable";
  if (prim && (e.key === "=" || e.key === "+")) return "app.textBigger";
  if (prim && e.key === "-") return "app.textSmaller";
  if (prim && e.key === "0") return "app.textReset";
  if (prim && lower === "z" && !e.shiftKey) {
    return ctx.inField ? null : "files.undo";
  }
  if (prim && !ctx.inField) {
    if ((lower === "x" || lower === "c") && ctx.hasSelection) {
      return lower === "x" ? "files.cut" : "files.copy";
    }
    if (lower === "v") return "files.paste";
    if (lower === "a") return "files.selectAll";
    if (e.key === "`") return "app.terminal";
    if (lower === "d" && ctx.hasSelection) return "files.duplicate";
    if (lower === "n" && e.shiftKey) return "files.newFolder";
    if (lower === "p" && ctx.hasSelection) return "files.print";
  }
  if (e.key === "F5") return "nav.refresh";
  if (prim && lower === "r" && !ctx.inField) return "nav.refresh";
  if (e.key === "F2" && !ctx.inField) return "files.rename";
  if (ctx.inField) return null;
  if (e.key === "Delete" || e.key === "Backspace") return "files.trash";
  if (
    e.key === " " &&
    ctx.hasTab &&
    !ctx.searchActive &&
    ctx.targetTag !== "BUTTON" &&
    ctx.targetTag !== "INPUT"
  ) {
    return "quicklook.toggle";
  }
  if (e.altKey && !e.ctrlKey && !e.metaKey) {
    if (e.key === "ArrowUp") return "nav.up";
    if (e.key === "ArrowLeft") return "nav.back";
    if (e.key === "ArrowRight") return "nav.forward";
  }
  return null;
}

// --------------------------------------------------------------- filter

export interface FilteredGroup {
  group: { id: GroupId; title: string };
  rows: ShortcutBinding[];
}

/** The overlay's live filter: matches action labels and the
 * platform-rendered chord text ("trash", "ctrl", "f5"). An empty
 * query returns every group. Pure — the overlay just renders it. */
export function filterShortcuts(query: string, mac: boolean): FilteredGroup[] {
  const q = query.trim().toLowerCase();
  const out: FilteredGroup[] = [];
  for (const g of SHORTCUT_GROUPS) {
    const rows = SHORTCUTS.filter((b) => {
      if (b.group !== g.id) return false;
      if (!q) return true;
      return (
        b.label.toLowerCase().includes(q) ||
        formatBinding(b, mac).toLowerCase().includes(q) ||
        b.surfaces.some((s) => s.toLowerCase().includes(q))
      );
    });
    if (rows.length) out.push({ group: g, rows });
  }
  return out;
}
