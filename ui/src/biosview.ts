/**
 * The System-files (BIOS) presentation, shared by the two doors
 * onto the same machinery: the welcome tour's setup scene and
 * Preferences → ROMs (Games) & Emulation → System files. The tour
 * is a door onto the card, never a fork of it — so the part both
 * doors render lives here, once.
 *
 * Dickles (2026-10-17): the BIOS step "feels disorganized as you
 * are setting them up… I like how we explain but tell a bit more.
 * What does the format look like? Help them." So under each
 * system's own words (the note stays with the surfaces), the card
 * is a path: WHAT TO BRING — the sets as a checklist with the
 * shape of the ask in words, not a filename pile — then WHAT IT
 * LOOKS LIKE WHEN IT'S HOME — the folder drawn as a tree with
 * the real filenames in it, so a man can hold his own folder up
 * against the picture.
 *
 * Every fact comes from the wire, which comes from ufm_emu::bios:
 * the names, the any-of / all-of shape, the subdirectory each set
 * lives in. Nothing is invented. Glob sets (a family of names,
 * not a list) show the one concrete example the requirement
 * table's own notes give, and the copy says it's an example.
 */
import type { EmuBiosSet, EmuBiosSystem } from "./api";

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

/** The concrete example name for a glob set, verbatim from the
 * requirement table's own notes (ufm_emu::bios) — PS2 dumps are
 * "a file named like scph39001.bin", CD32 Kickstarts "a file
 * named like kick40060.CD32". Keyed by system id; any other glob
 * set falls back to its pattern, unadorned. */
const GLOB_EXAMPLES: Record<string, string> = {
  ps2: "scph39001.bin",
  amigacd32: "kick40060.CD32",
};

function isGlobSet(set: EmuBiosSet): boolean {
  return set.names.some((n) => n.includes("…"));
}

function exampleName(sys: EmuBiosSystem, set: EmuBiosSet): string {
  return GLOB_EXAMPLES[sys.systemId] ?? set.names[0];
}

/** The folder's own last segment, for the tree's root line
 * ("~/.config/retroarch/system" draws as "system/"). */
function folderName(home: string): string {
  const parts = home.replace(/\/+$/, "").split("/");
  return parts[parts.length - 1] || home;
}

/** "What to bring": one block per set — the shape of the ask in
 * words (ONE file, any of these names / ALL of them, together /
 * one file named LIKE this), then the names as a quiet list,
 * never a comma pile. A satisfied set collapses to its truth:
 * the words, with ✓, and no list — it's home. */
export function biosBringBlock(sys: EmuBiosSystem): HTMLElement {
  const wrap = el("div", "bios-bring");
  wrap.appendChild(el("p", "bios-block-label", "What to bring"));
  for (const set of sys.sets) {
    const item = el("div", "bios-set");
    const head = set.present
      ? `${set.what} — already home ✓`
      : set.all
        ? `${set.what} — all ${set.names.length} files, together:`
        : isGlobSet(set)
          ? `${set.what} — one file, named like this:`
          : set.names.length === 1
            ? `${set.what} — one file:`
            : `${set.what} — one file, any of these names:`;
    item.appendChild(el("p", "bios-set-head", head));
    if (!set.present) {
      const shown = isGlobSet(set) ? [exampleName(sys, set)] : set.names;
      item.appendChild(
        el("p", "bios-names", shown.join(set.all ? ", " : "  ·  ")),
      );
    }
    wrap.appendChild(item);
  }
  return wrap;
}

/** "What it looks like when it's home": the folder drawn as a
 * tree with real filenames in it. Any-of sets show ONE file (the
 * suffix says one is enough); all-of sets show every file; a
 * satisfied set's files carry ✓. PS3 is the special shape the
 * table itself names: what you hand over is Sony's PS3UPDAT.PUP
 * and RPCS3 installs it — so the block says that, and names
 * where the installed firmware lands. */
export function biosFormatBlock(sys: EmuBiosSystem): HTMLElement {
  const wrap = el("div", "bios-format");
  wrap.appendChild(
    el("p", "bios-block-label", "What it looks like when it's home"),
  );
  const tree = el("div", "bios-tree");
  if (sys.systemId === "ps3") {
    tree.appendChild(el("p", "bios-tree-line", "You bring: PS3UPDAT.PUP"));
    tree.appendChild(
      el(
        "p",
        "bios-tree-note",
        "RPCS3 installs it for you — afterwards the firmware lives in " +
          `${folderName(sys.home)}/dev_flash/sys/external/ as .sprx files, ` +
          "and this card reads Ready.",
      ),
    );
    wrap.appendChild(tree);
    return wrap;
  }
  tree.appendChild(el("p", "bios-tree-line", `${folderName(sys.home)}/`));
  for (const set of sys.sets) {
    const inSub = set.subdir !== "";
    if (inSub) {
      tree.appendChild(el("p", "bios-tree-line", `└─ ${set.subdir}/`));
    }
    const indent = inSub ? "   " : "";
    const files = set.all
      ? set.names
      : [isGlobSet(set) ? exampleName(sys, set) : set.names[0]];
    files.forEach((name, fi) => {
      const branch = fi === files.length - 1 ? "└─ " : "├─ ";
      const suffix = set.present
        ? " ✓"
        : set.all
          ? ""
          : isGlobSet(set)
            ? "  ← for example"
            : set.names.length > 1
              ? "  ← one is enough"
              : "";
      tree.appendChild(
        el("p", "bios-tree-line", `${indent}${branch}${name}${suffix}`),
      );
    });
  }
  wrap.appendChild(tree);
  return wrap;
}

/** The whole path under a system's head: the bring-list, then
 * the format picture. (The note and the Add button stay with
 * each surface — the wagon and Preferences word those in their
 * own grammar.) */
export function biosPathBlock(sys: EmuBiosSystem): HTMLElement {
  const wrap = el("div", "bios-path");
  wrap.appendChild(biosBringBlock(sys));
  wrap.appendChild(biosFormatBlock(sys));
  return wrap;
}
