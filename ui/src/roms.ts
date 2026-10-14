// Roms Collection (Dickles, 2026-10-13): a sidebar section of per-system
// smart collections. Each system gathers ROM files by extension through the
// same rule engine as smart folders (searchView.runRule). Systems are
// manageable and deletable like labels/smart folders; the whole section can
// be deactivated from the hamburger menu. Storage mirrors labels/smart
// folders: localStorage, seeded once, deletions stick.

export interface RomSystem {
  name: string;
  extensions: string[];
  /** Collection scope (Dickles 2026-10-14): which drives this
   * collection draws from. Absent = all drives. Persisted with the
   * system definition. Drive entries are location ids as the app
   * knows them today (Indexer v2 will harden volume identity). */
  scope?: { mode: "all" | "boot" | "drives"; drives: string[] };
}

/** Built-in system → DAT dataset key (crates/ufm-media romdat).
 * Systems listed here are governed by the ROM LISTS: membership is a
 * hash identification against the No-Intro / Redump / MAME DATs, and
 * their extension lists are retired. Systems NOT listed (Nintendo
 * Switch, PlayStation 4/5 — no DAT exists in any source we can
 * redistribute) keep extension rules, as do Dickles' custom systems. */
export const ROM_SYSTEM_KEYS: Record<string, string> = {
  "3DO": "3do",
  "Amiga": "amiga",
  "Arcade (MAME)": "arcade",
  "Arcade": "arcade",
  "Atari 2600": "atari2600",
  "Atari 5200": "atari5200",
  "Atari 7800": "atari7800",
  "Atari 8-Bit": "atari8bit",
  "Atari Lynx": "lynx",
  "ColecoVision": "colecovision",
  "Commodore 64": "c64",
  "Dreamcast": "dreamcast",
  "Game Boy": "gb",
  "Game Boy Advance": "gba",
  "Game Boy Color": "gbc",
  "Game Gear": "gamegear",
  "GameCube": "gamecube",
  "Genesis": "genesis",
  "Intellivision": "intellivision",
  "Jaguar": "jaguar",
  "Master System": "mastersystem",
  "MSX": "msx",
  "Neo Geo": "neogeo",
  "Neo Geo Pocket": "neogeopocket",
  "NES": "nes",
  "Nintendo 3DS": "n3ds",
  "Nintendo 64": "n64",
  "Nintendo DS": "nds",
  "PlayStation": "psx",
  "PlayStation 2": "ps2",
  "PlayStation 3": "ps3",
  "PSP": "psp",
  "Saturn": "saturn",
  "Sega 32X": "sega32x",
  "Sega CD": "segacd",
  "SNES": "snes",
  "TurboGrafx-16": "tg16",
  "Vectrex": "vectrex",
  "Virtual Boy": "virtualboy",
  "Wii": "wii",
  "Wii U": "wiiu",
  "WonderSwan": "wonderswan",
  "Xbox": "xbox",
  "Xbox 360": "xbox360",
  "ZX Spectrum": "zxspectrum",
};

/** The DAT dataset key for a system, or null when it is not a
 * list-governed built-in (custom systems, Switch, PS4, PS5). */
export function romSystemKey(sys: RomSystem): string | null {
  return ROM_SYSTEM_KEYS[sys.name] ?? null;
}

/** The system's scope as a backend RomScope (default: all drives). */
export function romScopeOf(sys: RomSystem): {
  mode: "all" | "boot" | "drives";
  drives: string[];
} {
  return sys.scope ?? { mode: "all", drives: [] };
}

const STORE_KEY = "ff-rom-systems";
const SEED_FLAG = "ff-roms-v1";
const SEED_FLAG_V2 = "ff-roms-v2";
const ENABLED_KEY = "ff-roms-enabled";

/** Round 7.1 additions (Dickles): modern consoles + arcade. Applied as a
 * ONE-TIME additive merge (ff-roms-v2) so existing installs gain them
 * without resurrecting anything the user later deletes. "Arcade" also
 * matches the v1 name "Arcade (MAME)" — its extensions are extended
 * instead of adding a duplicate entry. */
export const ROM_SEED_V2: Array<{
  name: string;
  extensions: string[];
  aliases?: string[];
}> = [
  { name: "Nintendo Switch", extensions: ["nsp", "xci", "nsz", "xcz"] },
  { name: "Xbox 360", extensions: ["iso", "xex"] },
  { name: "PlayStation 3", extensions: ["iso", "pkg"] },
  { name: "PlayStation 4", extensions: ["pkg"] },
  { name: "PlayStation 5", extensions: ["pkg"] },
  { name: "Arcade", extensions: ["zip", "7z", "chd"], aliases: ["Arcade (MAME)"] },
];

export const ROM_SEED: RomSystem[] = [
  { name: "3DO", extensions: ["iso", "cue"] },
  { name: "Amiga", extensions: ["adf", "ipf", "lha"] },
  { name: "Arcade (MAME)", extensions: ["zip"] },
  { name: "Atari 2600", extensions: ["a26", "bin"] },
  { name: "Atari 5200", extensions: ["a52", "bin"] },
  { name: "Atari 7800", extensions: ["a78", "bin"] },
  { name: "Atari 8-Bit", extensions: ["atr", "atx", "xex"] },
  { name: "Atari Lynx", extensions: ["lnx"] },
  { name: "ColecoVision", extensions: ["col", "cv"] },
  { name: "Commodore 64", extensions: ["d64", "t64", "crt", "tap"] },
  { name: "Dreamcast", extensions: ["gdi", "cdi", "chd"] },
  { name: "Game Boy", extensions: ["gb"] },
  { name: "Game Boy Advance", extensions: ["gba"] },
  { name: "Game Boy Color", extensions: ["gbc"] },
  { name: "Game Gear", extensions: ["gg"] },
  { name: "GameCube", extensions: ["gcm", "ciso", "iso"] },
  { name: "Genesis", extensions: ["gen", "md", "smd"] },
  { name: "Intellivision", extensions: ["int", "bin"] },
  { name: "Jaguar", extensions: ["j64", "jag"] },
  { name: "Master System", extensions: ["sms"] },
  { name: "MSX", extensions: ["rom", "dsk"] },
  { name: "Neo Geo", extensions: ["zip", "neo"] },
  { name: "Neo Geo Pocket", extensions: ["ngp", "ngc"] },
  { name: "NES", extensions: ["nes"] },
  { name: "Nintendo 3DS", extensions: ["3ds", "cia"] },
  { name: "Nintendo 64", extensions: ["n64", "z64", "v64"] },
  { name: "Nintendo DS", extensions: ["nds"] },
  { name: "PlayStation", extensions: ["cue", "chd", "pbp", "iso"] },
  { name: "PlayStation 2", extensions: ["iso", "chd"] },
  { name: "PSP", extensions: ["iso", "cso", "pbp"] },
  { name: "Saturn", extensions: ["cue", "chd", "iso"] },
  { name: "Sega 32X", extensions: ["32x"] },
  { name: "Sega CD", extensions: ["cue", "chd", "iso"] },
  { name: "SNES", extensions: ["sfc", "smc"] },
  { name: "TurboGrafx-16", extensions: ["pce", "cue"] },
  { name: "Vectrex", extensions: ["vec", "bin"] },
  { name: "Virtual Boy", extensions: ["vb"] },
  { name: "Wii", extensions: ["wbfs", "wad", "iso"] },
  { name: "Wii U", extensions: ["wua", "wux", "rpx"] },
  { name: "WonderSwan", extensions: ["ws", "wsc"] },
  { name: "Xbox", extensions: ["xbe", "iso"] },
  { name: "ZX Spectrum", extensions: ["tzx", "tap", "z80", "sna"] },
];

export function loadRomSystems(): RomSystem[] {
  let list: RomSystem[] = [];
  try {
    const raw = localStorage.getItem(STORE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as RomSystem[];
      if (Array.isArray(parsed)) list = parsed;
    }
  } catch {
    // no stored systems
  }
  let seeded = false;
  try { seeded = !!localStorage.getItem(SEED_FLAG); } catch { seeded = true; }
  if (!seeded) {
    for (const d of ROM_SEED) {
      if (!list.some((s) => s.name === d.name)) {
        list.push({ name: d.name, extensions: [...d.extensions] });
      }
    }
    list.sort((a, b) => a.name.localeCompare(b.name));
    try { localStorage.setItem(SEED_FLAG, "1"); } catch { /* ignore */ }
    saveRomSystems(list);
  }
  // Round 7.1: one-time additive merge of the v2 systems. Runs exactly
  // once per install; deletions made after this merge are never undone.
  let mergedV2 = false;
  try { mergedV2 = !!localStorage.getItem(SEED_FLAG_V2); } catch { mergedV2 = true; }
  if (!mergedV2) {
    let changed = false;
    for (const d of ROM_SEED_V2) {
      const names = [d.name, ...(d.aliases ?? [])];
      const found = list.find((s) => names.includes(s.name));
      if (found) {
        for (const ext of d.extensions) {
          if (!found.extensions.includes(ext)) {
            found.extensions.push(ext);
            changed = true;
          }
        }
      } else {
        list.push({ name: d.name, extensions: [...d.extensions] });
        changed = true;
      }
    }
    if (changed) {
      list.sort((a, b) => a.name.localeCompare(b.name));
      saveRomSystems(list);
    }
    try { localStorage.setItem(SEED_FLAG_V2, "1"); } catch { /* ignore */ }
  }
  return list;
}

export function saveRomSystems(list: RomSystem[]): void {
  try {
    localStorage.setItem(STORE_KEY, JSON.stringify(list));
  } catch {
    // storage unavailable
  }
}

export function romsEnabled(): boolean {
  try {
    return localStorage.getItem(ENABLED_KEY) !== "0";
  } catch {
    return true;
  }
}

export function setRomsEnabled(on: boolean): void {
  try {
    localStorage.setItem(ENABLED_KEY, on ? "1" : "0");
  } catch {
    // storage unavailable
  }
}
