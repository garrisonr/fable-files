// Roms Collection (Dickles, 2026-10-13): a sidebar section of per-system
// smart collections. Each system gathers ROM files by extension through the
// same rule engine as smart folders (searchView.runRule). Systems are
// manageable and deletable like labels/smart folders; the whole section can
// be deactivated from the hamburger menu. Storage mirrors labels/smart
// folders: localStorage, seeded once, deletions stick.

export interface RomSystem {
  name: string;
  extensions: string[];
}

const STORE_KEY = "ff-rom-systems";
const SEED_FLAG = "ff-roms-v1";
const ENABLED_KEY = "ff-roms-enabled";

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
