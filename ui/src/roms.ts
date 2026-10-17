// Roms Collection (Dickles, 2026-10-13): a sidebar section of per-system
// smart collections. Each system gathers ROM files by extension through the
// same rule engine as smart folders (searchView.runRule). Systems are
// manageable and deletable like labels/smart folders; the whole section can
// be deactivated from the hamburger menu. Storage mirrors labels/smart
// folders: localStorage, seeded once, deletions stick.

/** System kind. "standard" systems gather files (by ROM-list hash
 * identification, or by extension when no list governs them).
 * "placeholder" systems (Dickles, 2026-10-15: Xbox Series, Switch 2)
 * hold a shelf for a system whose emulator cores and verified game
 * lists don't exist yet: they appear in the sidebar like any system
 * but gather NOTHING, promise NO identification, and stay out of the
 * DAT / get-missing-media machinery entirely. Absent = standard. */
export type RomSystemKind = "standard" | "placeholder";

export interface RomSystem {
  name: string;
  extensions: string[];
  kind?: RomSystemKind;
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
  "Amiga CD32": "amigacd32",
  "Arcade (MAME)": "arcade",
  "Arcade": "arcade",
  "Arcadia 2001": "arcadia2001",
  "Atari 2600": "atari2600",
  "Atari 5200": "atari5200",
  "Atari 7800": "atari7800",
  "Atari 8-Bit": "atari8bit",
  "Atari Lynx": "lynx",
  "CD-i": "cdi",
  "Channel F": "channelf",
  "ColecoVision": "colecovision",
  "Commodore 64": "c64",
  "Dreamcast": "dreamcast",
  "Game Boy": "gb",
  "Game Boy Advance": "gba",
  "Game Boy Color": "gbc",
  "Game Gear": "gamegear",
  "GameCube": "gamecube",
  "Game.com": "gamecom",
  "Genesis": "genesis",
  "GX4000": "gx4000",
  "Intellivision": "intellivision",
  "Jaguar": "jaguar",
  "Master System": "mastersystem",
  "MSX": "msx",
  "Neo Geo": "neogeo",
  "Neo Geo CD": "neogeocd",
  "Neo Geo Pocket": "neogeopocket",
  "NES": "nes",
  "Nintendo 3DS": "n3ds",
  "Nintendo 64": "n64",
  "Nintendo DS": "nds",
  "Odyssey²": "odyssey2",
  "PC": "pc",
  "PC-FX": "pcfx",
  "PlayStation": "psx",
  "PlayStation 2": "ps2",
  "PlayStation 3": "ps3",
  "PS Vita": "psvita",
  "PSP": "psp",
  "PV-1000": "pv1000",
  "Saturn": "saturn",
  "Sega 32X": "sega32x",
  "Sega CD": "segacd",
  "SG-1000": "sg1000",
  "SNES": "snes",
  "Studio II": "studio2",
  "Super A'can": "superacan",
  "Super Cassette Vision": "scv",
  "TurboGrafx-16": "tg16",
  "Vectrex": "vectrex",
  "VIC-20": "vic20",
  "Virtual Boy": "virtualboy",
  "Wii": "wii",
  "WonderSwan": "wonderswan",
  "Xbox": "xbox",
  "Xbox 360": "xbox360",
  "ZX Spectrum": "zxspectrum",
};
// NOT mapped, deliberately: "Wii U". Its only DAT (the GameTDB-derived
// list libretro-database carries) lists rom names with NO sizes or
// hashes, so the compiled dataset holds zero Wii U rows and a mapped
// collection could never match anything. Wii U keeps extension rules
// (wua/wux/rpx) like Switch / PS4 / PS5; if a hashed DAT ever ships,
// re-adding "Wii U": "wiiu" here restores list governance.

/** The DAT dataset key for a system, or null when it is not a
 * list-governed built-in (custom systems, Switch, PS4, PS5). */
export function romSystemKey(sys: RomSystem): string | null {
  return ROM_SYSTEM_KEYS[sys.name] ?? null;
}

/** Placeholder systems make no identification promises — every
 * surface (collection view, media fetch, editor) checks this. */
export function isRomPlaceholder(sys: RomSystem): boolean {
  return sys.kind === "placeholder";
}

/** The system's scope as a backend RomScope (default: all drives). */
export function romScopeOf(sys: RomSystem): {
  mode: "all" | "boot" | "drives";
  drives: string[];
} {
  return sys.scope ?? { mode: "all", drives: [] };
}

// ---------------------------------------------------------------------------
// Organization (Dickles, 2026-10-14): the full systems list is grouped
// under collapsible DIVISION headers — by manufacturer or by console
// generation, switchable in the section. Arcade is its own division in
// both schemes. Handhelds are first-class citizens of their maker /
// generation divisions, never a ghetto of their own.
// ---------------------------------------------------------------------------

export interface RomSystemMeta {
  maker: string;
  /** Standard console generation (1–9). Computers are bucketed by
   * launch era. null = unclassifiable (custom systems, Arcade). */
  gen: number | null;
}

export const ROM_SYSTEM_META: Record<string, RomSystemMeta> = {
  "3DO": { maker: "3DO", gen: 5 },
  "Amiga": { maker: "Commodore", gen: 3 },
  "Amiga CD32": { maker: "Commodore", gen: 5 },
  "APF-MP1000": { maker: "APF", gen: 2 },
  "Arcade": { maker: "Arcade", gen: null },
  "Arcade (MAME)": { maker: "Arcade", gen: null },
  "Arcadia 2001": { maker: "Emerson", gen: 2 },
  "Atari 2600": { maker: "Atari", gen: 2 },
  "Atari 5200": { maker: "Atari", gen: 2 },
  "Atari 7800": { maker: "Atari", gen: 3 },
  "Atari 8-Bit": { maker: "Atari", gen: 2 },
  "Atari Lynx": { maker: "Atari", gen: 4 },
  "Bally Astrocade": { maker: "Bally", gen: 2 },
  "CD-i": { maker: "Philips", gen: 4 },
  "Channel F": { maker: "Fairchild", gen: 2 },
  "ColecoVision": { maker: "Coleco", gen: 2 },
  "Commodore 64": { maker: "Commodore", gen: 2 },
  "Dreamcast": { maker: "Sega", gen: 6 },
  "FM Towns": { maker: "Fujitsu", gen: 4 },
  "Game Boy": { maker: "Nintendo", gen: 4 },
  "Game Boy Advance": { maker: "Nintendo", gen: 6 },
  "Game Boy Color": { maker: "Nintendo", gen: 5 },
  "Game Gear": { maker: "Sega", gen: 4 },
  "Game.com": { maker: "Tiger", gen: 5 },
  "GameCube": { maker: "Nintendo", gen: 6 },
  "Genesis": { maker: "Sega", gen: 4 },
  "GX4000": { maker: "Amstrad", gen: 3 },
  "Intellivision": { maker: "Mattel", gen: 2 },
  "Jaguar": { maker: "Atari", gen: 5 },
  "Master System": { maker: "Sega", gen: 3 },
  "MSX": { maker: "MSX", gen: 3 },
  "Neo Geo": { maker: "SNK", gen: 4 },
  "Neo Geo CD": { maker: "SNK", gen: 5 },
  "Neo Geo Pocket": { maker: "SNK", gen: 5 },
  "NES": { maker: "Nintendo", gen: 3 },
  "Nintendo 3DS": { maker: "Nintendo", gen: 8 },
  "Nintendo 64": { maker: "Nintendo", gen: 5 },
  "Nintendo DS": { maker: "Nintendo", gen: 7 },
  "Nintendo Switch": { maker: "Nintendo", gen: 8 },
  "Switch 2": { maker: "Nintendo", gen: 9 },
  "Odyssey²": { maker: "Magnavox", gen: 2 },
  "PC": { maker: "PC", gen: null },
  "PC-FX": { maker: "NEC", gen: 5 },
  "PlayStation": { maker: "Sony", gen: 5 },
  "PlayStation 2": { maker: "Sony", gen: 6 },
  "PlayStation 3": { maker: "Sony", gen: 7 },
  "PlayStation 4": { maker: "Sony", gen: 8 },
  "PlayStation 5": { maker: "Sony", gen: 9 },
  "Playdia": { maker: "Bandai", gen: 5 },
  "PS Vita": { maker: "Sony", gen: 8 },
  "PSP": { maker: "Sony", gen: 7 },
  "PV-1000": { maker: "Casio", gen: 3 },
  "Saturn": { maker: "Sega", gen: 5 },
  "Sega 32X": { maker: "Sega", gen: 4 },
  "Sega CD": { maker: "Sega", gen: 4 },
  "SG-1000": { maker: "Sega", gen: 2 },
  "SNES": { maker: "Nintendo", gen: 4 },
  "Studio II": { maker: "RCA", gen: 2 },
  "Super A'can": { maker: "Funtech", gen: 4 },
  "Super Cassette Vision": { maker: "Epoch", gen: 2 },
  "TurboGrafx-16": { maker: "NEC", gen: 4 },
  "Vectrex": { maker: "GCE", gen: 2 },
  "VIC-20": { maker: "Commodore", gen: 2 },
  "Virtual Boy": { maker: "Nintendo", gen: 5 },
  "Wii": { maker: "Nintendo", gen: 7 },
  "Wii U": { maker: "Nintendo", gen: 8 },
  "WonderSwan": { maker: "Bandai", gen: 5 },
  "Xbox": { maker: "Microsoft", gen: 6 },
  "Xbox Series": { maker: "Microsoft", gen: 9 },
  "Xbox 360": { maker: "Microsoft", gen: 7 },
  "ZX Spectrum": { maker: "Sinclair", gen: 2 },
};

export function romSystemMeta(name: string): RomSystemMeta {
  return ROM_SYSTEM_META[name] ?? { maker: "Other", gen: null };
}

/** Owned hardware with NO ROM library (Dickles' canon, 2026-10-14):
 * honored collection entries — name, maker, year, an honest note.
 * They never run a file search and never show a game count: the
 * machines predate ROMs (analog/dedicated circuits), or their
 * software isn't files at all (VHS tapes, DVDs, Android apps). */
export interface OwnedHardware {
  name: string;
  maker: string;
  year: number;
  gen: number | null;
  note: string;
}

const DEDICATED_NOTE =
  "Dedicated console — its games are hardwired circuits, not ROMs, so there is no library to identify.";

export const OWNED_HARDWARE: OwnedHardware[] = [
  { name: "Magnavox Odyssey", maker: "Magnavox", year: 1972, gen: 1, note: "The first home video game console. Analog circuitry and plug-in game cards — no ROMs exist; the plastic screen overlays are the graphics." },
  { name: "Sears Telegames Pong", maker: "Atari", year: 1975, gen: 1, note: DEDICATED_NOTE },
  { name: "Odyssey 100", maker: "Magnavox", year: 1975, gen: 1, note: DEDICATED_NOTE },
  { name: "Odyssey 200", maker: "Magnavox", year: 1975, gen: 1, note: DEDICATED_NOTE },
  { name: "Odyssey 300", maker: "Magnavox", year: 1976, gen: 1, note: DEDICATED_NOTE },
  { name: "Odyssey 400", maker: "Magnavox", year: 1976, gen: 1, note: DEDICATED_NOTE },
  { name: "Odyssey 500", maker: "Magnavox", year: 1976, gen: 1, note: DEDICATED_NOTE },
  { name: "Atari Super Pong C-140", maker: "Atari", year: 1976, gen: 1, note: DEDICATED_NOTE },
  { name: "Odyssey 2000", maker: "Magnavox", year: 1977, gen: 1, note: DEDICATED_NOTE },
  { name: "Odyssey 3000", maker: "Magnavox", year: 1977, gen: 1, note: DEDICATED_NOTE },
  { name: "Odyssey 4000", maker: "Magnavox", year: 1977, gen: 1, note: DEDICATED_NOTE },
  { name: "Atari Ultra Pong Doubles", maker: "Atari", year: 1977, gen: 1, note: DEDICATED_NOTE },
  { name: "Atari Video Pinball", maker: "Atari", year: 1977, gen: 1, note: DEDICATED_NOTE },
  { name: "Atari Touch Me", maker: "Atari", year: 1978, gen: 1, note: "Handheld electronic memory game — a circuit, not a computer. Nothing to load, nothing to collect." },
  { name: "Action Max", maker: "Worlds of Wonder", year: 1987, gen: 3, note: "A light-gun system whose games are VHS tapes — the software is videotape, so there is no ROM library." },
  { name: "Nuon", maker: "VM Labs", year: 2000, gen: 6, note: "A gaming platform built into DVD players. A handful of discs, no ROM ecosystem to identify." },
  { name: "Game Wave", maker: "ZAPiT", year: 2005, gen: 7, note: "A DVD-based family game system — its games are DVDs, not ROM files." },
  { name: "Ouya", maker: "Ouya", year: 2012, gen: 8, note: "An Android microconsole — its games are Android apps, not ROMs." },
  { name: "Nvidia Shield", maker: "Nvidia", year: 2013, gen: 8, note: "An Android TV box — apps and streaming, no ROM library." },
  { name: "MegaTouch Home", maker: "Merit", year: 2013, gen: 8, note: "The home edition of the bar-top touchscreen — self-contained games, no ROMs." },
];

export type RomGrouping = "maker" | "generation";

export interface RomDivision {
  title: string;
  systems: RomSystem[];
  hardware: OwnedHardware[];
}

function ordinal(n: number): string {
  return n === 1 ? "1st" : n === 2 ? "2nd" : n === 3 ? "3rd" : `${n}th`;
}

function divisionTitle(
  name: string,
  meta: RomSystemMeta,
  mode: RomGrouping,
): string {
  if (meta.maker === "Arcade") return "Arcade";
  if (mode === "maker") return meta.maker;
  return meta.gen !== null ? `${ordinal(meta.gen)} Generation` : "Other";
}

/** Group systems (+ owned-hardware entries) into divisions for the
 * ROMs section. Arcade leads in both modes; maker divisions are
 * alphabetical; generation divisions run 1st → 9th with "Other" last.
 * Inside a division: systems first (alphabetical), then hardware. */
export function romDivisions(
  systems: RomSystem[],
  mode: RomGrouping,
): RomDivision[] {
  const byTitle = new Map<string, RomDivision>();
  const order = new Map<string, number>();
  const keyOf = (title: string, meta: RomSystemMeta): number => {
    if (title === "Arcade") return -1;
    if (mode === "maker") return 0; // alphabetical via localeCompare below
    if (meta.gen !== null) return meta.gen;
    return 99; // "Other" last
  };
  const ensure = (title: string, meta: RomSystemMeta): RomDivision => {
    let d = byTitle.get(title);
    if (!d) {
      d = { title, systems: [], hardware: [] };
      byTitle.set(title, d);
      order.set(title, keyOf(title, meta));
    }
    return d;
  };
  for (const sys of systems) {
    const meta = romSystemMeta(sys.name);
    ensure(divisionTitle(sys.name, meta, mode), meta).systems.push(sys);
  }
  for (const hw of OWNED_HARDWARE) {
    const meta: RomSystemMeta = { maker: hw.maker, gen: hw.gen };
    ensure(divisionTitle(hw.name, meta, mode), meta).hardware.push(hw);
  }
  const divs = [...byTitle.values()];
  for (const d of divs) {
    d.systems.sort((a, b) => a.name.localeCompare(b.name));
    d.hardware.sort((a, b) => a.name.localeCompare(b.name));
  }
  divs.sort((a, b) => {
    const ka = order.get(a.title) ?? 0;
    const kb = order.get(b.title) ?? 0;
    if (ka !== kb) return ka - kb;
    return a.title.localeCompare(b.title);
  });
  return divs;
}

const GROUPING_KEY = "ff-rom-grouping";
const DIV_OPEN_KEY = "ff-rom-div-open";

export function loadRomGrouping(): RomGrouping {
  try {
    return localStorage.getItem(GROUPING_KEY) === "generation"
      ? "generation"
      : "maker";
  } catch {
    return "maker";
  }
}

export function saveRomGrouping(mode: RomGrouping): void {
  try {
    localStorage.setItem(GROUPING_KEY, mode);
  } catch {
    // storage unavailable
  }
}

/** Division collapse state: divisions are COLLAPSED unless their title
 * is in the open set (Dickles: default collapsed, everywhere). */
export function loadOpenDivisions(): Set<string> {
  try {
    const raw = localStorage.getItem(DIV_OPEN_KEY);
    if (raw) {
      const arr = JSON.parse(raw) as string[];
      if (Array.isArray(arr)) return new Set(arr);
    }
  } catch {
    // no stored state
  }
  return new Set();
}

export function saveOpenDivisions(open: Set<string>): void {
  try {
    localStorage.setItem(DIV_OPEN_KEY, JSON.stringify([...open]));
  } catch {
    // storage unavailable
  }
}

const STORE_KEY = "ff-rom-systems";
const SEED_FLAG = "ff-roms-v1";
const SEED_FLAG_V2 = "ff-roms-v2";
const SEED_FLAG_V3 = "ff-roms-v3";
const SEED_FLAG_V4 = "ff-roms-v4";
const SEED_FLAG_V5 = "ff-roms-v5";
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

/** ROM organization pass (2026-10-15): systems restored from the
 * canon audit — every owned, ROM-capable machine whose DAT sat unused
 * in libretro-database (Odyssey², Channel F, Game.com, PS Vita, …),
 * plus the owned systems no redistributable DAT covers (Bally
 * Astrocade, APF-MP1000, FM Towns, Playdia), which keep extension
 * rules like Switch/PS4/PS5. Fresh installs seed them directly;
 * existing installs gain them via the one-time v3 additive merge. */
export const ROM_NEW_SYSTEMS: Array<{ name: string; extensions: string[] }> = [
  { name: "Amiga CD32", extensions: ["iso", "cue", "chd"] },
  { name: "APF-MP1000", extensions: ["bin"] },
  { name: "Arcadia 2001", extensions: ["bin"] },
  { name: "Bally Astrocade", extensions: ["bin"] },
  { name: "CD-i", extensions: ["iso", "cue", "chd", "bin"] },
  { name: "Channel F", extensions: ["bin", "chf"] },
  { name: "FM Towns", extensions: ["iso", "cue", "chd"] },
  { name: "Game.com", extensions: ["tgc", "bin"] },
  { name: "GX4000", extensions: ["cpr"] },
  { name: "Neo Geo CD", extensions: ["iso", "cue", "chd"] },
  { name: "Odyssey²", extensions: ["bin"] },
  { name: "PC-FX", extensions: ["iso", "cue", "chd"] },
  { name: "Playdia", extensions: ["iso", "cue", "chd"] },
  { name: "PS Vita", extensions: ["vpk"] },
  { name: "PV-1000", extensions: ["bin"] },
  { name: "SG-1000", extensions: ["sg", "sc"] },
  { name: "Studio II", extensions: ["bin", "st2"] },
  { name: "Super A'can", extensions: ["bin"] },
  { name: "Super Cassette Vision", extensions: ["bin"] },
  { name: "VIC-20", extensions: ["prg", "crt", "tap", "d64"] },
];

/** Placeholder systems (Dickles, 2026-10-15): 9th-generation
 * shelves for machines with no emulator core and no verified game
 * list yet. No extensions — a placeholder gathers nothing, by
 * design. Existing installs gain them via the one-time v4 merge. */
export const ROM_PLACEHOLDER_SYSTEMS: Array<{
  name: string;
  extensions: string[];
  kind: RomSystemKind;
}> = [
  { name: "Xbox Series", extensions: [], kind: "placeholder" },
  { name: "Switch 2", extensions: [], kind: "placeholder" },
];

export const ROM_SEED: RomSystem[] = [
  ...ROM_PLACEHOLDER_SYSTEMS.map((s) => ({
    name: s.name,
    extensions: [...s.extensions],
    kind: s.kind,
  })),
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
  { name: "PC", extensions: [] },
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
  ...ROM_NEW_SYSTEMS.map((s) => ({
    name: s.name,
    extensions: [...s.extensions],
  })),
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
        list.push({
          name: d.name,
          extensions: [...d.extensions],
          ...(d.kind ? { kind: d.kind } : {}),
        });
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
  // ROM organization pass: one-time additive merge of the restored
  // systems. Runs exactly once per install; deletions made after this
  // merge are never undone.
  let mergedV3 = false;
  try { mergedV3 = !!localStorage.getItem(SEED_FLAG_V3); } catch { mergedV3 = true; }
  if (!mergedV3) {
    let changed = false;
    for (const d of ROM_NEW_SYSTEMS) {
      if (!list.some((s) => s.name === d.name)) {
        list.push({ name: d.name, extensions: [...d.extensions] });
        changed = true;
      }
    }
    if (changed) {
      list.sort((a, b) => a.name.localeCompare(b.name));
      saveRomSystems(list);
    }
    try { localStorage.setItem(SEED_FLAG_V3, "1"); } catch { /* ignore */ }
  }
  // Placeholder pass (2026-10-15): one-time additive merge of the
  // placeholder systems (Xbox Series, Switch 2). Runs exactly once
  // per install; a placeholder the user later deletes stays gone.
  let mergedV4 = false;
  try { mergedV4 = !!localStorage.getItem(SEED_FLAG_V4); } catch { mergedV4 = true; }
  if (!mergedV4) {
    let changed = false;
    for (const d of ROM_PLACEHOLDER_SYSTEMS) {
      if (!list.some((s) => s.name === d.name)) {
        list.push({
          name: d.name,
          extensions: [...d.extensions],
          kind: d.kind,
        });
        changed = true;
      }
    }
    if (changed) {
      list.sort((a, b) => a.name.localeCompare(b.name));
      saveRomSystems(list);
    }
    try { localStorage.setItem(SEED_FLAG_V4, "1"); } catch { /* ignore */ }
  }
  // PC Games P1 (2026-10-17): the PC section of Games Home. PC has
  // no extension rules — its games arrive by import (Playnite) and
  // folder scan, never by extension gathering, so the seed carries
  // an empty list exactly like a placeholder. One-time additive
  // merge; a deleted PC entry stays deleted.
  let mergedV5 = false;
  try { mergedV5 = !!localStorage.getItem(SEED_FLAG_V5); } catch { mergedV5 = true; }
  if (!mergedV5) {
    if (!list.some((s) => s.name === "PC")) {
      list.push({ name: "PC", extensions: [] });
      list.sort((a, b) => a.name.localeCompare(b.name));
      saveRomSystems(list);
    }
    try { localStorage.setItem(SEED_FLAG_V5, "1"); } catch { /* ignore */ }
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

/** Internet Archive destinations for the empty-system pointer
 * (Dickles, 2026-10-15: an empty shelf should POINT somewhere).
 * Verified 2026-10-15 against archive.org's own metadata service —
 * every identifier below is a live collection carrying that
 * system's library ("Software Library: NES", "…: Sega Game Gear",
 * and so on). The /details/ pages bot-wall scripted fetchers, so
 * the metadata service is the verification of record; systems
 * without a collection of their own fall back to the general
 * Software Library, which loads everywhere. */
export const ROM_ARCHIVE_GENERAL =
  "https://archive.org/details/softwarelibrary";

const ROM_ARCHIVE_LINKS: Record<string, string> = {
  amiga: "https://archive.org/details/softwarelibrary_amiga",
  arcade: "https://archive.org/details/softwarelibrary_arcade",
  atari2600: "https://archive.org/details/softwarelibrary_atari2600",
  atari7800: "https://archive.org/details/softwarelibrary_atari7800",
  atari8bit: "https://archive.org/details/softwarelibrary_atari800",
  lynx: "https://archive.org/details/softwarelibrary_lynx",
  c64: "https://archive.org/details/softwarelibrary_c64",
  gb: "https://archive.org/details/softwarelibrary_gameboy",
  gbc: "https://archive.org/details/softwarelibrary_gameboy",
  gamegear: "https://archive.org/details/softwarelibrary_gamegear",
  genesis: "https://archive.org/details/softwarelibrary_genesis",
  mastersystem: "https://archive.org/details/softwarelibrary_sms",
  nes: "https://archive.org/details/softwarelibrary_nes",
  snes: "https://archive.org/details/softwarelibrary_snes",
  tg16: "https://archive.org/details/softwarelibrary_turbografx",
  jaguar: "https://archive.org/details/softwarelibrary_jaguar",
  zxspectrum: "https://archive.org/details/softwarelibrary_zx_spectrum",
};

/** The archive.org shelf for a system key — its own collection
 * where one exists, the general Software Library otherwise. */
export function romArchiveLink(systemKey: string): {
  url: string;
  perSystem: boolean;
} {
  const url = ROM_ARCHIVE_LINKS[systemKey];
  return url
    ? { url, perSystem: true }
    : { url: ROM_ARCHIVE_GENERAL, perSystem: false };
}

// ---------------------------------------------------------------------------
// ROM SYSTEM ICON SETS (Dickles, 2026-10-15 — DESIGN.md "ROM SYSTEM ICON
// SETS"): every built-in system gets a per-system icon in THREE complete
// sets — "art" (full-color 3D art), "photo" (photorealistic) and "flat"
// (hand-authored outline SVG, theme-tinted). The chooser lives in
// Preferences ▸ Sidebar; the sidebar honors it immediately. Raster sets
// ship as 192px WebP under /rom-icons/<set>/<slug>.webp; a system whose
// icon is missing from the chosen set degrades per-system to the next
// completed set (photo → art → flat), never to a placeholder glyph.
// Custom (user-created) systems have no slug and keep the 🎮 glyph.
// ---------------------------------------------------------------------------

export type RomIconSet = "art" | "photo" | "flat";

export const ROM_ICON_SET_INFO: { id: RomIconSet; label: string; blurb: string }[] = [
  { id: "art", label: "3D Art", blurb: "Full-color 3D artwork of each console" },
  { id: "photo", label: "Photorealistic", blurb: "Studio photographs of the real hardware" },
  { id: "flat", label: "Flat Outline", blurb: "Single-color outlines that follow your theme" },
];

/** Built-in system name → icon slug. "Arcade" serves both Arcade names. */
export const ROM_ICON_SLUGS: Record<string, string> = {
  "3DO": "3do", "APF-MP1000": "apf-mp1000", "Amiga": "amiga", "Amiga CD32": "amiga-cd32",
  "Arcade": "arcade", "Arcade (MAME)": "arcade", "Arcadia 2001": "arcadia-2001",
  "Atari 2600": "atari-2600", "Atari 5200": "atari-5200", "Atari 7800": "atari-7800",
  "Atari 8-Bit": "atari-8bit", "Atari Lynx": "atari-lynx", "Bally Astrocade": "bally-astrocade",
  "CD-i": "cd-i", "Channel F": "channel-f", "ColecoVision": "colecovision",
  "Commodore 64": "commodore-64", "Dreamcast": "dreamcast", "FM Towns": "fm-towns",
  "GX4000": "gx4000", "Game Boy": "game-boy", "Game Boy Advance": "game-boy-advance",
  "Game Boy Color": "game-boy-color", "Game Gear": "game-gear", "Game.com": "game-com",
  "GameCube": "gamecube", "Genesis": "genesis", "Intellivision": "intellivision",
  "Jaguar": "jaguar", "MSX": "msx", "Master System": "master-system", "NES": "nes",
  "Neo Geo": "neo-geo", "Neo Geo CD": "neo-geo-cd", "Neo Geo Pocket": "neo-geo-pocket",
  "Nintendo 3DS": "nintendo-3ds", "Nintendo 64": "nintendo-64", "Nintendo DS": "nintendo-ds",
  "Nintendo Switch": "nintendo-switch", "Odyssey²": "odyssey2", "PC-FX": "pc-fx",
  "PS Vita": "ps-vita", "PSP": "psp", "PV-1000": "pv-1000", "PlayStation": "playstation",
  "PlayStation 2": "playstation-2", "PlayStation 3": "playstation-3",
  "PlayStation 4": "playstation-4", "PlayStation 5": "playstation-5", "Playdia": "playdia",
  "SG-1000": "sg-1000", "SNES": "snes", "Saturn": "saturn", "Sega 32X": "sega-32x",
  "Sega CD": "sega-cd", "Studio II": "studio-ii", "Super A'can": "super-acan",
  "Super Cassette Vision": "super-cassette-vision", "Switch 2": "switch-2",
  "TurboGrafx-16": "turbografx-16", "VIC-20": "vic-20", "Vectrex": "vectrex",
  "Virtual Boy": "virtual-boy", "Wii": "wii", "Wii U": "wii-u", "WonderSwan": "wonderswan",
  "Xbox": "xbox", "Xbox 360": "xbox-360", "Xbox Series": "xbox-series",
  "ZX Spectrum": "zx-spectrum",
};

export function romIconSlug(sysName: string): string | null {
  return ROM_ICON_SLUGS[sysName] ?? null;
}

const ROM_ICON_SET_KEY = "ff-rom-icon-set";

export function loadRomIconSet(): RomIconSet {
  try {
    const v = localStorage.getItem(ROM_ICON_SET_KEY);
    return v === "photo" || v === "flat" ? v : "art";
  } catch {
    return "art";
  }
}

export function saveRomIconSet(set: RomIconSet): void {
  try {
    localStorage.setItem(ROM_ICON_SET_KEY, set);
  } catch {
    /* storage unavailable — the chooser still applies for this session */
  }
}
