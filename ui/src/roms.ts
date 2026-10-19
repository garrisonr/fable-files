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
  /** Hidden (Dickles, 2026-10-18): no sidebar entry, no Games Home
   * shelf, excluded from All Games and every count/completeness
   * surface — "hidden for good" until unhidden in Preferences.
   * Identification keeps running silently: unhide and the shelf is
   * instantly current. Absent = visible, so every pre-existing
   * record stays visible (migration-safe). */
  hidden?: boolean;
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
  // Console completion pass (2026-10-18): DAT-governed additions.
  // Every key here has hashed rows in the compiled dataset — the
  // Wii U rule above is the bar. Systems with no redistributable
  // hashed DAT (Xbox One, N-Gage, Pippin, Nuon, …) are NOT mapped
  // and keep extension rules or placeholder honesty.
  "Casio Loopy": "loopy",
  "Cassette Vision": "cassettevision",
  "Commodore CDTV": "cdtv",
  "GP32": "gp32",
  "Jaguar CD": "jaguarcd",
  "Leapster": "leapster",
  "Nintendo 64DD": "n64dd",
  "Pokémon Mini": "pokemonmini",
  "Sega Pico": "pico",
  "SuperGrafx": "supergrafx",
  "TurboGrafx-CD": "tgcd",
  "V.Smile": "vsmile",
  "Watara Supervision": "supervision",
  // Computer completion pass (2026-10-19): every key here has
  // hashed rows in the compiled dataset (verified in the checkout:
  // No-Intro for ST / X68000 / X1, TOSEC for Apple II / CPC /
  // PC-88, TOSEC+Redump for PC-98). BBC Micro, TI-99/4A and
  // TRS-80 CoCo are deliberately NOT mapped — no DAT in the
  // checkout — and keep extension rules.
  "Atari ST": "atarist",
  "Apple II": "apple2",
  "Amstrad CPC": "cpc",
  "PC-88": "pc88",
  "PC-98": "pc98",
  "Sharp X68000": "x68000",
  "Sharp X1": "x1",
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

/** Extension claims (Bouncer, 2026-10-18): the extensions each
 * system may gather files by — an extension speaks for a system
 * only when that system is its SOLE claimant in the whole list.
 * An extension several systems list (.bin is listed by sixteen)
 * can never honestly assign a file to one of them, so it claims
 * nothing: those files wait for a hash identification (and sit
 * in Needs Identification meanwhile) instead of being dressed
 * as whichever system happens to sort first. */
export function soleClaimExtensions(
  systems: RomSystem[],
): Map<string, string[]> {
  const counts = new Map<string, number>();
  for (const s of systems) {
    for (const e of new Set(s.extensions.map((x) => x.toLowerCase()))) {
      counts.set(e, (counts.get(e) ?? 0) + 1);
    }
  }
  const out = new Map<string, string[]>();
  for (const s of systems) {
    out.set(
      s.name,
      s.extensions.filter((e) => counts.get(e.toLowerCase()) === 1),
    );
  }
  return out;
}

/** Placeholder systems make no identification promises — every
 * surface (collection view, media fetch, editor) checks this. */
export function isRomPlaceholder(sys: RomSystem): boolean {
  return sys.kind === "placeholder";
}

/** A system is hidden only when its record says so — absent or
 * false both mean visible. */
export function isRomHidden(sys: RomSystem): boolean {
  return sys.hidden === true;
}

/** Dataset keys of the hidden systems, for the catalog calls that
 * must exclude them (counts + shelf windows). Extension systems
 * have no key; their hiding is enforced on the UI surfaces. */
export function hiddenRomKeys(): string[] {
  const keys: string[] = [];
  for (const s of loadRomSystems()) {
    if (!isRomHidden(s)) continue;
    const k = romSystemKey(s);
    if (k) keys.push(k);
  }
  return keys;
}

/** Set one system's hidden flag, persisted with its record. The
 * record keeps every other field; unhiding deletes the flag so the
 * stored shape stays the pre-hiding shape. */
export function setRomSystemHidden(name: string, hidden: boolean): void {
  const list = loadRomSystems();
  const sys = list.find((x) => x.name === name);
  if (!sys) return;
  if (hidden) sys.hidden = true;
  else delete sys.hidden;
  saveRomSystems(list);
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

export type RomDivisionId = "console" | "computer" | "odds";

export interface RomSystemMeta {
  maker: string;
  /** Standard console generation (1–9). Computers are bucketed by
   * launch era. null = unclassifiable (custom systems, Arcade). */
  gen: number | null;
  /** The collection division this system lives in (Dickles,
   * 2026-10-19): consoles ride the generation/maker ladder,
   * computers get a division of their own (never interleaved with
   * consoles), and bar-top / touch curiosities sit in Odds & Ends.
   * Absent = "console". Owned hardware is Odds & Ends by nature. */
  division?: RomDivisionId;
}

export const ROM_SYSTEM_META: Record<string, RomSystemMeta> = {
  "3DO": { maker: "3DO", gen: 5 },
  "Amiga": { maker: "Commodore", gen: 3, division: "computer" },
  "Amiga CD32": { maker: "Commodore", gen: 5 },
  "APF-MP1000": { maker: "APF", gen: 2 },
  "Arcade": { maker: "Arcade", gen: null },
  "Arcade (MAME)": { maker: "Arcade", gen: null },
  "Arcadia 2001": { maker: "Emerson", gen: 2 },
  "Atari 2600": { maker: "Atari", gen: 2 },
  "Atari 5200": { maker: "Atari", gen: 2 },
  "Atari 7800": { maker: "Atari", gen: 3 },
  "Atari 8-Bit": { maker: "Atari", gen: 2, division: "computer" },
  "Atari Lynx": { maker: "Atari", gen: 4 },
  "Bally Astrocade": { maker: "Bally", gen: 2 },
  "CD-i": { maker: "Philips", gen: 4 },
  "Channel F": { maker: "Fairchild", gen: 2 },
  "ColecoVision": { maker: "Coleco", gen: 2 },
  "Commodore 64": { maker: "Commodore", gen: 2, division: "computer" },
  "Dreamcast": { maker: "Sega", gen: 6 },
  "FM Towns": { maker: "Fujitsu", gen: 4, division: "computer" },
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
  "MSX": { maker: "MSX", gen: 3, division: "computer" },
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
  "PC": { maker: "PC", gen: null, division: "computer" },
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
  "VIC-20": { maker: "Commodore", gen: 2, division: "computer" },
  "Virtual Boy": { maker: "Nintendo", gen: 5 },
  "Wii": { maker: "Nintendo", gen: 7 },
  "Wii U": { maker: "Nintendo", gen: 8 },
  "WonderSwan": { maker: "Bandai", gen: 5 },
  "Xbox": { maker: "Microsoft", gen: 6 },
  "Xbox Series": { maker: "Microsoft", gen: 9 },
  "Xbox 360": { maker: "Microsoft", gen: 7 },
  "ZX Spectrum": { maker: "Sinclair", gen: 2, division: "computer" },
  // Console completion pass (2026-10-18).
  "Apple Pippin": { maker: "Apple", gen: 5 },
  "Casio Loopy": { maker: "Casio", gen: 5 },
  "Cassette Vision": { maker: "Epoch", gen: 2 },
  "Commodore CDTV": { maker: "Commodore", gen: 4 },
  "Evercade": { maker: "Blaze", gen: 9 },
  "Gizmondo": { maker: "Tiger", gen: 7 },
  "GP32": { maker: "GamePark", gen: 6 },
  "HyperScan": { maker: "Mattel", gen: 7 },
  "Jaguar CD": { maker: "Atari", gen: 5 },
  "Leapster": { maker: "LeapFrog", gen: 6 },
  "Mega Duck": { maker: "Welback", gen: 4 },
  "Microvision": { maker: "Milton Bradley", gen: 2 },
  "N-Gage": { maker: "Nokia", gen: 6 },
  "Nintendo 64DD": { maker: "Nintendo", gen: 5 },
  "Nuon": { maker: "VM Labs", gen: 6 },
  "OUYA": { maker: "Ouya", gen: 8 },
  "Playdate": { maker: "Panic", gen: 9 },
  "Pokémon Mini": { maker: "Nintendo", gen: 6 },
  "Sega Pico": { maker: "Sega", gen: 4 },
  "SuperGrafx": { maker: "NEC", gen: 4 },
  "TurboGrafx-CD": { maker: "NEC", gen: 4 },
  "V.Smile": { maker: "VTech", gen: 6 },
  "Watara Supervision": { maker: "Watara", gen: 4 },
  "Xbox One": { maker: "Microsoft", gen: 8 },
  "Zodiac": { maker: "Tapwave", gen: 6 },
  // Computer completion pass (2026-10-19): the gaming computers join
  // the roster in a division of their own. DAT-governed where the
  // pipeline's libretro-database checkout carries a hashed DAT
  // (Atari ST, Apple II, Amstrad CPC, PC-88, PC-98, X68000, X1);
  // BBC Micro, TI-99/4A and the CoCo have no DAT in the checkout and
  // keep honest extension rules.
  "Atari ST": { maker: "Atari", gen: 3, division: "computer" },
  "Apple II": { maker: "Apple", gen: 2, division: "computer" },
  "Amstrad CPC": { maker: "Amstrad", gen: 3, division: "computer" },
  "BBC Micro": { maker: "Acorn", gen: 2, division: "computer" },
  "TI-99/4A": { maker: "Texas Instruments", gen: 2, division: "computer" },
  "TRS-80 CoCo": { maker: "Tandy", gen: 2, division: "computer" },
  "PC-88": { maker: "NEC", gen: 2, division: "computer" },
  "PC-98": { maker: "NEC", gen: 3, division: "computer" },
  "Sharp X68000": { maker: "Sharp", gen: 3, division: "computer" },
  "Sharp X1": { maker: "Sharp", gen: 2, division: "computer" },
  // Odds & Ends (Dickles, 2026-10-18): the bar-top corner. JVL's
  // countertop line (Concorde, iTouch, Encore, Retro) is Merit
  // MegaTouch's Canadian rival — fixed software packs on dedicated
  // cabinets, so the shelf is a placeholder: no DAT, no loose files.
  "JVL": { maker: "JVL", gen: null, division: "odds" },
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
  { name: "Game Wave", maker: "ZAPiT", year: 2005, gen: 7, note: "A DVD-based family game system — its games are DVDs, not ROM files." },
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
  hardware: OwnedHardware[] = OWNED_HARDWARE,
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
  for (const hw of hardware) {
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

/** The division a system belongs to. Owned hardware is Odds &
 * Ends by nature and handled by romSections, never here. */
export function romDivisionOf(name: string): RomDivisionId {
  return ROM_SYSTEM_META[name]?.division ?? "console";
}

export interface RomSection {
  id: RomDivisionId;
  title: string;
  divisions: RomDivision[];
}

/** The collection in its three divisions (Dickles, 2026-10-19):
 * Consoles on the generation/maker ladder, Computers in a division
 * of their own — never interleaved with consoles — and Odds & Ends
 * as one flat drawer holding the bar-top/touch curiosities (JVL)
 * and every owned-hardware entry (the dedicated Pong and Odyssey
 * machines, MegaTouch Home, the Shield…), which used to flood the
 * generation ladder. Nothing is deleted: every entry keeps its
 * seat, filed where it belongs. */
export function romSections(
  systems: RomSystem[],
  mode: RomGrouping,
): RomSection[] {
  const inDiv = (id: RomDivisionId) =>
    systems.filter((s) => romDivisionOf(s.name) === id);
  const sections: RomSection[] = [];
  const consoles = inDiv("console");
  if (consoles.length > 0) {
    sections.push({
      id: "console",
      title: "Consoles",
      divisions: romDivisions(consoles, mode, []),
    });
  }
  const computers = inDiv("computer");
  if (computers.length > 0) {
    sections.push({
      id: "computer",
      title: "Computers",
      divisions: romDivisions(computers, mode, []),
    });
  }
  const odds = inDiv("odds");
  if (odds.length > 0 || OWNED_HARDWARE.length > 0) {
    sections.push({
      id: "odds",
      title: "Odds & Ends",
      divisions: [
        {
          title: "Odds & Ends",
          systems: [...odds].sort((a, b) => a.name.localeCompare(b.name)),
          hardware: [...OWNED_HARDWARE].sort((a, b) =>
            a.name.localeCompare(b.name),
          ),
        },
      ],
    });
  }
  return sections;
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
const SEED_FLAG_V6 = "ff-roms-v6";
const SEED_FLAG_V7 = "ff-roms-v7";
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

/** Console completion pass (Dickles, 2026-10-18): the retail roster
 * gaps, closed. DAT-governed where a redistributable hashed DAT
 * exists in libretro-database (Casio Loopy, Commodore CDTV, Pokémon
 * Mini, Watara Supervision, GP32, V.Smile, Leapster, Sega Pico,
 * SuperGrafx, Nintendo 64DD, Cassette Vision via TOSEC — plus the
 * TurboGrafx-CD and Jaguar CD splits out of their parent systems);
 * extension rules where no DAT is redistributed (Xbox One, N-Gage,
 * Gizmondo, Microvision, Mega Duck, Zodiac, Apple Pippin, HyperScan,
 * Nuon); placeholders where no files can be shelved yet (OUYA —
 * Android packages would sole-claim every stray .apk on disk, so it
 * gathers nothing — Playdate, Evercade). PlayStation TV is NOT a
 * separate shelf: it runs the PS Vita's exact software (.vpk), so
 * its games live on the PS Vita shelf. Fresh installs seed these via
 * ROM_SEED; existing installs gain them via the v6 merge. */
export const ROM_SEED_V6: Array<{
  name: string;
  extensions: string[];
  kind?: RomSystemKind;
}> = [
  { name: "Apple Pippin", extensions: ["iso", "cue", "chd"] },
  { name: "Casio Loopy", extensions: ["bin"] },
  { name: "Cassette Vision", extensions: ["bin"] },
  { name: "Commodore CDTV", extensions: ["iso", "cue", "chd"] },
  { name: "Evercade", extensions: [], kind: "placeholder" },
  { name: "Gizmondo", extensions: ["giz"] },
  { name: "GP32", extensions: ["smc", "gxb", "gxe"] },
  { name: "HyperScan", extensions: ["iso", "cue", "chd"] },
  { name: "Jaguar CD", extensions: ["cue", "chd", "iso", "cdi"] },
  { name: "Leapster", extensions: ["bin"] },
  { name: "Mega Duck", extensions: ["bin"] },
  { name: "Microvision", extensions: ["bin"] },
  { name: "N-Gage", extensions: ["sis"] },
  { name: "Nintendo 64DD", extensions: ["ndd", "z64"] },
  { name: "Nuon", extensions: ["iso", "chd"] },
  { name: "OUYA", extensions: [], kind: "placeholder" },
  { name: "Playdate", extensions: [], kind: "placeholder" },
  { name: "Pokémon Mini", extensions: ["min"] },
  { name: "Sega Pico", extensions: ["md", "bin"] },
  { name: "SuperGrafx", extensions: ["sgx", "pce"] },
  { name: "TurboGrafx-CD", extensions: ["cue", "chd", "iso"] },
  { name: "V.Smile", extensions: ["bin"] },
  { name: "Watara Supervision", extensions: ["sv", "bin"] },
  { name: "Xbox One", extensions: ["xvc", "msixvc"] },
  { name: "Zodiac", extensions: ["prc", "pdb"] },
];

/** Computer completion pass + Odds & Ends (Dickles, 2026-10-19):
 * the gaming computers (DAT-governed where the checkout carries a
 * hashed DAT; extension rules for BBC Micro, TI-99/4A, CoCo) and
 * JVL, the Canadian bar-top line, as an Odds & Ends placeholder —
 * its games are fixed software packs inside dedicated cabinets, so
 * there are no files to gather, exactly like MegaTouch Home's seat
 * in OWNED_HARDWARE. Fresh installs seed via ROM_SEED; existing
 * installs gain them via the v7 merge. */
export const ROM_SEED_V7: Array<{
  name: string;
  extensions: string[];
  kind?: RomSystemKind;
}> = [
  { name: "Amstrad CPC", extensions: ["dsk", "cdt", "sna"] },
  { name: "Apple II", extensions: ["dsk", "woz", "po", "do", "nib"] },
  { name: "Atari ST", extensions: ["st", "msa", "stx"] },
  { name: "BBC Micro", extensions: ["ssd", "dsd", "uef", "tap"] },
  { name: "JVL", extensions: [], kind: "placeholder" },
  { name: "PC-88", extensions: ["d88", "n88"] },
  { name: "PC-98", extensions: ["d88", "hdi", "fdi", "iso", "chd"] },
  { name: "Sharp X1", extensions: ["d88", "tap", "cas"] },
  { name: "Sharp X68000", extensions: ["dim", "xdf", "hdf", "d88"] },
  { name: "TI-99/4A", extensions: ["rpk", "dsk", "bin"] },
  { name: "TRS-80 CoCo", extensions: ["dsk", "ccc", "rom", "cas"] },
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
  ...ROM_SEED_V6.map((s) => ({
    name: s.name,
    extensions: [...s.extensions],
    ...(s.kind ? { kind: s.kind } : {}),
  })),
  ...ROM_SEED_V7.map((s) => ({
    name: s.name,
    extensions: [...s.extensions],
    ...(s.kind ? { kind: s.kind } : {}),
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
  // Console completion pass (2026-10-18): one-time additive merge of
  // the completed retail roster. Runs exactly once per install;
  // deletions made after this merge are never undone, and no record
  // gains a hidden flag here — nothing starts hidden.
  let mergedV6 = false;
  try { mergedV6 = !!localStorage.getItem(SEED_FLAG_V6); } catch { mergedV6 = true; }
  if (!mergedV6) {
    let changed = false;
    for (const d of ROM_SEED_V6) {
      if (!list.some((s) => s.name === d.name)) {
        list.push({
          name: d.name,
          extensions: [...d.extensions],
          ...(d.kind ? { kind: d.kind } : {}),
        });
        changed = true;
      }
    }
    if (changed) {
      list.sort((a, b) => a.name.localeCompare(b.name));
      saveRomSystems(list);
    }
    try { localStorage.setItem(SEED_FLAG_V6, "1"); } catch { /* ignore */ }
  }
  // Computer completion pass (2026-10-19): one-time additive merge
  // of the computer roster + JVL. Runs exactly once per install;
  // deletions made after this merge are never undone, and no record
  // gains a hidden flag here — nothing starts hidden.
  let mergedV7 = false;
  try { mergedV7 = !!localStorage.getItem(SEED_FLAG_V7); } catch { mergedV7 = true; }
  if (!mergedV7) {
    let changed = false;
    for (const d of ROM_SEED_V7) {
      if (!list.some((s) => s.name === d.name)) {
        list.push({
          name: d.name,
          extensions: [...d.extensions],
          ...(d.kind ? { kind: d.kind } : {}),
        });
        changed = true;
      }
    }
    if (changed) {
      list.sort((a, b) => a.name.localeCompare(b.name));
      saveRomSystems(list);
    }
    try { localStorage.setItem(SEED_FLAG_V7, "1"); } catch { /* ignore */ }
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
// The museum label (Dickles, 2026-10-19): every console's page
// carries a couple of calm, true lines about the unit, and a
// Wikipedia link where an article exists. The blurbs are editorial
// and factual — what the machine was, its moment, its character;
// no marketing copy, no invented specs. The Wikipedia table is
// STAGED data: it is the feed point for the link-verification pass
// (Game Warden verifies each article by opening it; a system with
// no verified article renders no link at all).
// ---------------------------------------------------------------------------

export const ROM_SYSTEM_BLURBS: Record<string, string> = {
  "3DO": "The 3DO Interactive Multiplayer (1993) tried to license one standard to every manufacturer, like VHS for games. Beautiful ambition, a $699 launch price, and a short, interesting life.",
  "APF-MP1000": "APF's 1978 cartridge console, from a New York electronics firm better known for calculators. Modest, sturdy, and designed to grow toward a full computer few owners ever saw.",
  "Amiga": "Commodore's 1985 machine made art, music and multitasking feel like magic tricks — and across Europe it was the games computer of a generation.",
  "Amiga CD32": "The Amiga CD32 (1993) put a computer's soul in a console shell — the first 32-bit CD console sold in the West, built on Amiga 1200 guts. Commodore folded within the year.",
  "Amstrad CPC": "Alan Sugar's 1984 all-in-one came with its own monitor and a mandate: fun, cheap, complete. It ruled French and Spanish bedrooms especially.",
  "Apple II": "The 1977 machine that launched the personal computer industry — and its games culture with it, from Akalabeth to a certain wagon trail and its dysentery.",
  "Apple Pippin": "Apple's 1996 Bandai-built multimedia console was a Macintosh in TV clothing — expensive, underpowered, and gone within a year. The rarest branch on Apple's tree.",
  "Arcade": "The coin-op room: four decades of cabinets, boards and attract modes. Not one machine but a whole culture of them.",
  "Arcade (MAME)": "The coin-op room: four decades of cabinets, boards and attract modes. Not one machine but a whole culture of them.",
  "Arcadia 2001": "Emerson's 1982 console, a tidy also-ran from the pre-crash years. Its games traveled the world under a dozen licensed names and shells.",
  "Atari 2600": "The 1977 machine that taught homes to swap cartridges. Woodgrain, joysticks like gear levers, and a library that defined the first console era.",
  "Atari 5200": "Atari's 1982 answer to the Intellivision — powerful, enormous, and saddled with a famously fragile joystick. Great arcade ports live here.",
  "Atari 7800": "Built in 1984, shelved during the crash, released in 1986 to fight the NES with one hand tied. Backward-compatible with the 2600, and beloved for it.",
  "Atari 8-Bit": "Atari's 1979 400 and 800 brought custom graphics chips and serious games home before 'home computer' was a phrase. Star Raiders sold the dream.",
  "Atari Lynx": "The first color handheld (1989) — big, bright, hungry for batteries, and years ahead of the Game Boy it couldn't outsell.",
  "Atari ST": "Atari's 1985 machine paired a sharp work ethic with color games and built-in MIDI ports that quietly made it the musician's computer.",
  "BBC Micro": "Acorn's 1981 classroom computer, commissioned alongside a BBC television series to teach a nation computing — and home to Elite, which taught it space trading instead.",
  "Bally Astrocade": "A 1978 console that was secretly a computer: BASIC on a cartridge, room for four controllers, and graphics hardware shared with the arcade.",
  "CD-i": "Philips' 1991 CD player that also played games — or a game console playing it safe; history still argues. Famous now for its licensed Nintendo oddities.",
  "Casio Loopy": "Casio's 1995 console aimed at girls had a built-in thermal printer for printing stickers of your screenshots — a feature so charming it outlived the tiny library.",
  "Cassette Vision": "Epoch's 1981 machine was Japan's first cartridge console, two years before the Famicom — modest games, honest engineering, and a head start that didn't hold.",
  "Channel F": "The Fairchild Channel F (1976) was the first console to play swappable ROM cartridges — the idea every later machine inherited. Its controllers were strange and wonderful.",
  "ColecoVision": "Coleco's 1982 machine brought the arcade home more faithfully than anyone else, Donkey Kong first. The crash cut it short in 1985.",
  "Commodore 64": "The best-selling computer model of all time (1982): a game library in the tens of thousands and a SID chip that still headlines chiptune concerts.",
  "Commodore CDTV": "The CDTV (1991) hid an Amiga inside a stereo-component black box and sold the multimedia future by remote control. The living room wasn't ready; the idea was simply early.",
  "Dreamcast": "Sega's last console (1999): a built-in modem, arcade-perfect ports, and ideas a decade early. It burned bright and fast, and its library still glows.",
  "Evercade": "Blaze's 2020 handheld revived the cartridge on purpose: licensed retro collections on real carts, in boxes, with manuals. A love letter with a slot.",
  "FM Towns": "Fujitsu's 1989 Japanese powerhouse shipped with a CD drive as standard and multimedia ambitions the West wouldn't match for years.",
  "GP32": "Game Park's 2001 Korean handheld was open before open was cool — homebrew and emulators flourished on it years ahead of the app-store era.",
  "GX4000": "Amstrad's 1990 console turn with its CPC computer line — handsome, well-made, and about eight months from launch to clearance bin. A what-if in white plastic.",
  "Game Boy": "Nintendo's 1989 monochrome brick: modest specs, legendary battery life, and Tetris in the box. The handheld everything else is measured against.",
  "Game Boy Advance": "A Super Nintendo in your pocket (2001), minus two buttons and plus a golden age of 2D games. The clamshell SP made it pocket-perfect.",
  "Game Boy Color": "The Game Boy, in color at last (1998) — backward-compatible with everything before it and home to a second wind of classics.",
  "Game Gear": "Sega's color handheld (1990): a Master System in your hands, with a backlit screen and a famous appetite for AA batteries.",
  "Game.com": "Tiger's 1997 handheld reached for the future — a touchscreen, a stylus, even a modem — with a screen that couldn't keep up. Fascinating, flawed, first.",
  "GameCube": "Nintendo's little purple lunchbox (2001) hid serious muscle and one of the most-loved controllers ever made. The handle was not a joke; it was an invitation.",
  "Genesis": "Sega's 1989 16-bit swung hard at Nintendo with speed and attitude. The console war it started defined a generation of playgrounds.",
  "Gizmondo": "Tiger Telematics' 2005 GPS-toting handheld is remembered less for its games than for the spectacular scandal that ended the company. The hardware itself was genuinely neat.",
  "HyperScan": "Mattel's 2006 console scanned RFID cards into its games — toys-to-life before the term existed — wrapped around a machine few got to know.",
  "Intellivision": "Mattel's 1979 machine sold sophistication: finer graphics, a keypad controller, and George Plimpton explaining why. The thinking person's first console.",
  "JVL": "JVL's bar-top touchscreen cabinets — Concord, Ontario, from the 1995 Concorde on — were Merit MegaTouch's great Canadian rivals: fixed packs of card, quiz and puzzle games, built for the corner of the bar.",
  "Jaguar": "Atari's last stand (1993): '64-bit' by marketing math, powerful and peculiar underneath, with a controller like a phone keypad. Do the math.",
  "Jaguar CD": "The 1995 CD add-on that perched on the Jaguar and played a small shelf of discs. Atari's last hardware exhale.",
  "Leapster": "LeapFrog's 2003 edutainment handheld taught letters and numbers to a generation of back-seat passengers. The stylus was chunky on purpose.",
  "MSX": "The 1983 standard shared by a dozen makers let one software library run everywhere — huge in Japan, Brazil and Europe, and where Hideo Kojima started.",
  "Master System": "Sega's 1986 8-bit was outclassed in America but enormous in Europe and Brazil, where it sold for decades. Crisp sprites and a devoted afterlife.",
  "Mega Duck": "Welback's 1993 handheld was a Game Boy in all but name and library — sold around the world under aliases, Cougar Boy among them.",
  "Microvision": "Milton Bradley's 1979 handheld was the first to swap game cartridges — the Game Boy's big idea, a decade early, on a screen the size of a stamp.",
  "N-Gage": "Nokia's 2003 phone-console hybrid made you hold it sideways to talk, and 'sidetalking' became a meme. The games were better than the jokes.",
  "NES": "The 1985 gray box that resurrected the American game industry and taught a generation to blow on cartridges. Its constraints became an aesthetic.",
  "Neo Geo": "SNK's 1990 miracle: the actual arcade board, sold for the home at arcade prices. The games were perfect because they were identical.",
  "Neo Geo CD": "SNK tried to make the Neo Geo affordable in 1994 by swapping cartridges for CDs — and its load times became the stuff of legend.",
  "Neo Geo Pocket": "SNK's 1998 monochrome handheld, refined in color a year later: a clicky stick, beautiful fighters, and a life cut short by its maker's collapse.",
  "Nintendo 3DS": "Glasses-free 3D (2011) was the headline; the library was the story. Two screens, one hinge, and a decade of quiet classics.",
  "Nintendo 64": "Nintendo's 1996 leap to 3D, cartridges held against the CD tide, and a three-pronged controller nobody held the same way twice. Four ports made it a social network.",
  "Nintendo 64DD": "The 1999 disk drive that clipped under the Nintendo 64 in Japan only — ten disks, a modem, and a decade of promises mostly unkept.",
  "Nintendo DS": "Two screens, a stylus and Wi-Fi (2004): Nintendo stopped chasing power and started chasing everyone. One of the best-selling machines ever made.",
  "Nintendo Switch": "The 2017 hybrid that made docking a verb: TV to handheld in one motion. It outsold nearly everything and unified Nintendo's hardware into a single line.",
  "Nuon": "VM Labs hid a game console inside DVD players (2000) — lights that pulsed along with your music and a handful of games, Tempest 3000 among them.",
  "OUYA": "The 2013 Kickstarter cube promised an open Android console for $99 and delivered a beautiful argument about what consoles are. Its games are Android packages — APKs, identified by their package names.",
  "Odyssey²": "Magnavox's 1979 cartridge console carried a full membrane keyboard — part game machine, part home-computer promise. Its voice module talked, which felt like magic.",
  "PC": "The open platform that never shipped as one box: DOS spelunking, shareware discs, and forty years of everything. Its library arrives by import, not by cartridge.",
  "PC-88": "NEC's 1981 line became Japan's default home computer of the eighties — the machine Falcom and the early Japanese RPG grew up on.",
  "PC-98": "NEC's 1982 successor line owned Japanese desks for two decades — business by day, and by night the cradle of Touhou and the doujin game scene.",
  "PC-FX": "NEC's 1994 Japan-only successor to the PC Engine bet everything on full-motion video just as polygons arrived. A beautiful dead end.",
  "PS Vita": "Sony's 2011 handheld was the finest hardware of its generation and a commercial shrug — beloved fiercely by its few, an indie haven to the end.",
  "PSP": "Sony's first handheld (2004) put a cinema screen in your pocket and movies on little optical discs. Sleek, ambitious, and quietly enormous in Japan.",
  "PV-1000": "Casio's 1983 console, launched the same season as the Famicom and buried by it within the year. A footnote with charm.",
  "PlayStation": "Sony's 1994 CD machine made games grow up: real 3D, compact discs, and a cool the industry hadn't sold before. A hundred million units, and the culture moved.",
  "PlayStation 2": "The best-selling console of all time (2000) — a DVD player that played games, or the reverse — in a black monolith that anchored a decade of living rooms.",
  "PlayStation 3": "Sony's 2006 Cell-powered swing: Blu-ray, supercomputer ambitions, a rocky launch, and a library that aged into respect.",
  "PlayStation 4": "Sony kept it simple in 2013 — a games machine, fairly priced, easy to build for — and sold over a hundred million of them.",
  "PlayStation 5": "The 2020 white tower: near-instant loading from its SSD, a controller that rumbles in textures, and worlds that stream in whole.",
  "Playdate": "Panic's 2022 yellow square has a crank, a black-and-white screen, and games delivered in seasons. Deliberately small, defiantly joyful.",
  "Playdia": "Bandai's 1994 anime machine for small fans: an infrared remote, disc-based cartoons you could play, and homework replaced by homework about Dragon Ball.",
  "Pokémon Mini": "Nintendo's tiniest console (2001): cartridges of pocket-sized Pokémon, a rumble motor, and an infrared port for tiny multiplayer.",
  "SG-1000": "Sega's first home console (1983), released in Japan the same day as the Famicom. Everything Sega became starts here.",
  "SNES": "Nintendo's 1991 16-bit answered Sega with color, Mode 7 flair, and arguably the strongest library ever assembled. The purple buttons were a choice.",
  "Saturn": "Sega's 1994 2D powerhouse stumbled over its own surprise launch and the polygon age — and remains a treasure chest for import shelves.",
  "Sega 32X": "A mushroom-shaped power-up for the Genesis (1994) that asked too much, too late. A small library and endless what-ifs.",
  "Sega CD": "The Genesis grew a CD drive in 1992 and games grew cutscenes — grainy, earnest full-motion video that defined an era's ambitions.",
  "Sega Pico": "Sega's 1993 storybook console turned picture books into games with a magic pen. It taught a generation to read before it taught them Sonic.",
  "Sharp X1": "Sharp's 1982 television-division computer needed no dedicated monitor and kept a clean, modern line — a loyal Japanese following agreed.",
  "Sharp X68000": "Sharp's 1987 machine was practically an arcade board with a keyboard — the connoisseur's computer for pixel-perfect conversions.",
  "Studio II": "RCA's 1977 black-and-white console was outclassed on arrival next to the Channel F — and is now a small, honest museum piece.",
  "Super A'can": "Funtech's 1995 Taiwan-only 16-bit launched into the Saturn's shadow with a dozen games and genuine craft. Obscure even at home.",
  "Super Cassette Vision": "Epoch's 1984 follow-up put real muscle behind the Cassette Vision line and fought the Famicom bravely, briefly.",
  "SuperGrafx": "NEC's 1989 PC Engine upgrade doubled the sprites for exactly five games. The library fits in one hand — and every one of them is real.",
  "Switch 2": "Nintendo's 2025 successor keeps the hybrid faith — bigger, faster, magnetic. The shelf stands ready before the library does; that's what placeholders are for.",
  "TI-99/4A": "Texas Instruments' 1981 machine was the first home computer built around a 16-bit CPU, then got price-warred nearly to death by Commodore within two years.",
  "TRS-80 CoCo": "Radio Shack's 1980 Color Computer hid a superb Motorola 6809 chip behind a modest badge and became a hobbyist's beloved workhorse.",
  "TurboGrafx-16": "NEC and Hudson's 1987 PC Engine: tiny HuCards, huge arcade energy. A star in Japan, a cult object in America.",
  "TurboGrafx-CD": "The 1988 add-on made the PC Engine the first console family to play games from CD — voices, real music, and Ys. In America it arrived as the TurboGrafx-CD.",
  "V.Smile": "VTech's 2004 console for small children: a big friendly joystick, cartridges called Smartridges, and learning games that respected their audience.",
  "VIC-20": "Commodore's 1981 'friendly computer' — William Shatner asked why you'd buy a game machine instead. Five kilobytes of RAM, endless ingenuity.",
  "Vectrex": "The 1982 console with its own vector screen — glowing line graphics no television could fake. A self-contained arcade that still looks like the future.",
  "Virtual Boy": "Nintendo's 1995 red-and-black gamble on tabletop 3D: Gunpei Yokoi's strangest machine, a small library, and headaches both literal and corporate.",
  "Watara Supervision": "A 1992 budget Game Boy rival from Watara, with a tilting screen neck and a library of earnest near-misses. Cheap then, charming now.",
  "Wii": "Nintendo's 2006 white wedge sold motion to everyone — bowling grandparents included — and became a hundred-million-unit argument that fun beats specs.",
  "Wii U": "The 2012 tablet-controller experiment stumbled on its name and seeded the Switch's best ideas. Its library deserved better; much of it emigrated.",
  "WonderSwan": "Bandai's 1999 Japan-only handheld, designed by Game Boy creator Gunpei Yokoi: one AA battery, forty hours, and Final Fantasy in miniature.",
  "Xbox": "Microsoft's 2001 entry was a PC in console clothing — heavy, powerful, and home to Halo. Online play followed, and the industry changed.",
  "Xbox 360": "Microsoft's 2005 head start defined the HD era: Xbox Live, achievements, downloadable games — and a red ring that defined its warranty program.",
  "Xbox One": "Microsoft's 2013 all-in-one box stumbled out of the gate, then settled into a solid life powered by the best backward-compatibility program in the business.",
  "Xbox Series": "Microsoft's 2020 pair — the X and the little S — sold power and subscriptions over spectacle. This shelf waits while its library tools mature.",
  "ZX Spectrum": "Sinclair's 1982 rainbow-striped computer put a games industry in British bedrooms at a student's price. Rubber keys, squealing tape loaders, immortal games.",
  "Zodiac": "Tapwave's 2003 Palm-powered handheld was a PDA that played serious games — business in the front, Doom in the back.",
};

/** The museum label for a system, or null when none is written
 * (custom systems carry no label). */
export function romSystemBlurb(name: string): string | null {
  return ROM_SYSTEM_BLURBS[name] ?? null;
}

export const ROM_SYSTEM_NOTES: Record<string, string> = {
  "JVL": "JVL's games are fixed software packs inside dedicated bar-top cabinets — there are no loose files to gather, so this shelf keeps the machine's place in the collection, the way MegaTouch Home does.",
  "OUYA": "Ouya games are Android packages (APKs), identified by the package inside them — not ROMs. Fable deliberately keeps this shelf manual: a blanket .apk sweep would claim every stray Android package on your drives as an OUYA game. Point the system editor at a folder of OUYA APKs if you want this shelf to gather, and the choice (and its risk) is yours, on purpose.",
};

/** A placeholder shelf's honest note for systems whose story the
 * generic coming-soon line would get wrong (OUYA's APKs, JVL's
 * fixed packs). Null = the standard placeholder copy. */
export function romSystemNote(name: string): string | null {
  return ROM_SYSTEM_NOTES[name] ?? null;
}

export const ROM_SYSTEM_WIKI: Record<string, string> = {
  "3DO": "https://en.wikipedia.org/wiki/3DO",
  "Action Max": "https://en.wikipedia.org/wiki/Action_Max",
  "Amiga": "https://en.wikipedia.org/wiki/Amiga",
  "Amiga CD32": "https://en.wikipedia.org/wiki/Amiga_CD32",
  "Amstrad CPC": "https://en.wikipedia.org/wiki/Amstrad_CPC",
  "Apple II": "https://en.wikipedia.org/wiki/Apple_II",
  "Apple Pippin": "https://en.wikipedia.org/wiki/Apple_Pippin",
  "Arcade": "https://en.wikipedia.org/wiki/MAME",
  "Arcade (MAME)": "https://en.wikipedia.org/wiki/MAME",
  "Arcadia 2001": "https://en.wikipedia.org/wiki/Arcadia_2001",
  "Atari 2600": "https://en.wikipedia.org/wiki/Atari_2600",
  "Atari 5200": "https://en.wikipedia.org/wiki/Atari_5200",
  "Atari 7800": "https://en.wikipedia.org/wiki/Atari_7800",
  "Atari 8-Bit": "https://en.wikipedia.org/wiki/Atari_8-bit_computers",
  "Atari Lynx": "https://en.wikipedia.org/wiki/Atari_Lynx",
  "Atari ST": "https://en.wikipedia.org/wiki/Atari_ST",
  "BBC Micro": "https://en.wikipedia.org/wiki/BBC_Micro",
  "Bally Astrocade": "https://en.wikipedia.org/wiki/Bally_Astrocade",
  "CD-i": "https://en.wikipedia.org/wiki/CD-i",
  "Casio Loopy": "https://en.wikipedia.org/wiki/Casio_Loopy",
  "Cassette Vision": "https://en.wikipedia.org/wiki/Cassette_Vision",
  "Channel F": "https://en.wikipedia.org/wiki/Fairchild_Channel_F",
  "ColecoVision": "https://en.wikipedia.org/wiki/ColecoVision",
  "Commodore 64": "https://en.wikipedia.org/wiki/Commodore_64",
  "Commodore CDTV": "https://en.wikipedia.org/wiki/Commodore_CDTV",
  "Dreamcast": "https://en.wikipedia.org/wiki/Dreamcast",
  "Evercade": "https://en.wikipedia.org/wiki/Evercade",
  "FM Towns": "https://en.wikipedia.org/wiki/FM_Towns",
  "GP32": "https://en.wikipedia.org/wiki/GP32",
  "GX4000": "https://en.wikipedia.org/wiki/GX4000",
  "Game Boy": "https://en.wikipedia.org/wiki/Game_Boy",
  "Game Boy Advance": "https://en.wikipedia.org/wiki/Game_Boy_Advance",
  "Game Boy Color": "https://en.wikipedia.org/wiki/Game_Boy_Color",
  "Game Gear": "https://en.wikipedia.org/wiki/Game_Gear",
  "Game.com": "https://en.wikipedia.org/wiki/Game.com",
  "GameCube": "https://en.wikipedia.org/wiki/GameCube",
  "Genesis": "https://en.wikipedia.org/wiki/Sega_Genesis",
  "Gizmondo": "https://en.wikipedia.org/wiki/Gizmondo",
  "HyperScan": "https://en.wikipedia.org/wiki/HyperScan",
  "Intellivision": "https://en.wikipedia.org/wiki/Intellivision",
  "Jaguar": "https://en.wikipedia.org/wiki/Atari_Jaguar",
  "Jaguar CD": "https://en.wikipedia.org/wiki/Atari_Jaguar_CD",
  "Leapster": "https://en.wikipedia.org/wiki/Leapster",
  "MSX": "https://en.wikipedia.org/wiki/MSX",
  "Magnavox Odyssey": "https://en.wikipedia.org/wiki/Magnavox_Odyssey",
  "Master System": "https://en.wikipedia.org/wiki/Master_System",
  "Mega Duck": "https://en.wikipedia.org/wiki/Mega_Duck",
  "Microvision": "https://en.wikipedia.org/wiki/Microvision",
  "N-Gage": "https://en.wikipedia.org/wiki/N-Gage_(device)",
  "NES": "https://en.wikipedia.org/wiki/Nintendo_Entertainment_System",
  "Neo Geo": "https://en.wikipedia.org/wiki/Neo_Geo",
  "Neo Geo CD": "https://en.wikipedia.org/wiki/Neo_Geo_CD",
  "Neo Geo Pocket": "https://en.wikipedia.org/wiki/Neo_Geo_Pocket",
  "Nintendo 3DS": "https://en.wikipedia.org/wiki/Nintendo_3DS",
  "Nintendo 64": "https://en.wikipedia.org/wiki/Nintendo_64",
  "Nintendo 64DD": "https://en.wikipedia.org/wiki/64DD",
  "Nintendo DS": "https://en.wikipedia.org/wiki/Nintendo_DS",
  "Nintendo Switch": "https://en.wikipedia.org/wiki/Nintendo_Switch",
  "Nuon": "https://en.wikipedia.org/wiki/Nuon_(DVD_technology)",
  "OUYA": "https://en.wikipedia.org/wiki/Ouya",
  "Odyssey²": "https://en.wikipedia.org/wiki/Magnavox_Odyssey_2",
  "PC-88": "https://en.wikipedia.org/wiki/PC-8800_series",
  "PC-98": "https://en.wikipedia.org/wiki/PC-9800_series",
  "PC-FX": "https://en.wikipedia.org/wiki/PC-FX",
  "PS Vita": "https://en.wikipedia.org/wiki/PlayStation_Vita",
  "PSP": "https://en.wikipedia.org/wiki/PlayStation_Portable",
  "PV-1000": "https://en.wikipedia.org/wiki/PV-1000",
  "PlayStation": "https://en.wikipedia.org/wiki/PlayStation_(console)",
  "PlayStation 2": "https://en.wikipedia.org/wiki/PlayStation_2",
  "PlayStation 3": "https://en.wikipedia.org/wiki/PlayStation_3",
  "PlayStation 4": "https://en.wikipedia.org/wiki/PlayStation_4",
  "PlayStation 5": "https://en.wikipedia.org/wiki/PlayStation_5",
  "Playdate": "https://en.wikipedia.org/wiki/Playdate_(console)",
  "Playdia": "https://en.wikipedia.org/wiki/Playdia",
  "Pokémon Mini": "https://en.wikipedia.org/wiki/Poké9mon_Mini",
  "SG-1000": "https://en.wikipedia.org/wiki/SG-1000",
  "SNES": "https://en.wikipedia.org/wiki/Super_Nintendo_Entertainment_System",
  "Saturn": "https://en.wikipedia.org/wiki/Sega_Saturn",
  "Sega 32X": "https://en.wikipedia.org/wiki/32X",
  "Sega CD": "https://en.wikipedia.org/wiki/Sega_CD",
  "Sega Pico": "https://en.wikipedia.org/wiki/Sega_Pico",
  "Sharp X1": "https://en.wikipedia.org/wiki/Sharp_X1",
  "Sharp X68000": "https://en.wikipedia.org/wiki/X68000",
  "Studio II": "https://en.wikipedia.org/wiki/RCA_Studio_II",
  "Super A'can": "https://en.wikipedia.org/wiki/Super_A'can",
  "Super Cassette Vision": "https://en.wikipedia.org/wiki/Super_Cassette_Vision",
  "SuperGrafx": "https://en.wikipedia.org/wiki/PC_Engine_SuperGrafx",
  "Switch 2": "https://en.wikipedia.org/wiki/Nintendo_Switch_2",
  "TI-99/4A": "https://en.wikipedia.org/wiki/TI-99/4A",
  "TRS-80 CoCo": "https://en.wikipedia.org/wiki/TRS-80_Color_Computer",
  "TurboGrafx-16": "https://en.wikipedia.org/wiki/TurboGrafx-16",
  "TurboGrafx-CD": "https://en.wikipedia.org/wiki/TurboGrafx-CD",
  "V.Smile": "https://en.wikipedia.org/wiki/V.Smile",
  "VIC-20": "https://en.wikipedia.org/wiki/Commodore_VIC-20",
  "Vectrex": "https://en.wikipedia.org/wiki/Vectrex",
  "Virtual Boy": "https://en.wikipedia.org/wiki/Virtual_Boy",
  "Watara Supervision": "https://en.wikipedia.org/wiki/Watara_Supervision",
  "Wii": "https://en.wikipedia.org/wiki/Wii",
  "Wii U": "https://en.wikipedia.org/wiki/Wii_U",
  "WonderSwan": "https://en.wikipedia.org/wiki/WonderSwan",
  "Xbox": "https://en.wikipedia.org/wiki/Xbox_(console)",
  "Xbox 360": "https://en.wikipedia.org/wiki/Xbox_360",
  "Xbox One": "https://en.wikipedia.org/wiki/Xbox_One",
  "Xbox Series": "https://en.wikipedia.org/wiki/Xbox_Series_X_and_Series_S",
  "ZX Spectrum": "https://en.wikipedia.org/wiki/ZX_Spectrum",
  "Zodiac": "https://en.wikipedia.org/wiki/Tapwave_Zodiac",
};

/** The system's Wikipedia article (staged for verification — see
 * the museum-label note above), or null when none is staged. */
export function romSystemWiki(name: string): string | null {
  return ROM_SYSTEM_WIKI[name] ?? null;
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
  // Console completion pass (2026-10-18): flat glyphs ship for all
  // of these; art/photo WebPs land with the icon-art follow-up —
  // until then the sets degrade to the flat glyph, never a blank.
  "Apple Pippin": "apple-pippin", "Casio Loopy": "casio-loopy",
  "Cassette Vision": "cassette-vision", "Commodore CDTV": "commodore-cdtv",
  "Evercade": "evercade", "Gizmondo": "gizmondo", "GP32": "gp32",
  "HyperScan": "hyperscan", "Jaguar CD": "jaguar-cd", "Leapster": "leapster",
  "Mega Duck": "mega-duck", "Microvision": "microvision", "N-Gage": "n-gage",
  "Nintendo 64DD": "nintendo-64dd", "Nuon": "nuon", "OUYA": "ouya",
  "Playdate": "playdate", "Pokémon Mini": "pokemon-mini", "Sega Pico": "sega-pico",
  "SuperGrafx": "supergrafx", "TurboGrafx-CD": "turbografx-cd",
  "V.Smile": "v-smile", "Watara Supervision": "watara-supervision",
  "Xbox One": "xbox-one", "Zodiac": "zodiac",
  // Computer completion pass + Odds & Ends (2026-10-19): flat
  // glyphs ship for all of these; art/photo WebPs land with the
  // icon-art follow-up, and their hardware photos join the graded
  // set then — until then the sets degrade to the flat glyph.
  "Amstrad CPC": "amstrad-cpc", "Apple II": "apple-ii",
  "Atari ST": "atari-st", "BBC Micro": "bbc-micro", "JVL": "jvl",
  "PC-88": "pc-88", "PC-98": "pc-98", "Sharp X1": "sharp-x1",
  "Sharp X68000": "sharp-x68000", "TI-99/4A": "ti-99", "TRS-80 CoCo": "trs-80-coco",
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
