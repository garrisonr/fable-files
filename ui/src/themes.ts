/**
 * Theme switching. Lists themes via list_themes, applies via apply_theme
 * (the Rust core returns CSS custom properties; the shell installs them
 * verbatim into #ufm-theme-vars and never invents a color), persists the
 * choice in localStorage. Default: grizzette.
 */
import { api, type ThemeInfo } from "./api";

const STORAGE_KEY = "fable.theme";
export const DEFAULT_THEME = "grizzette";

/**
 * Distro theme engines — the EXACT set the Linux System Profiler GUI
 * ships (Dickles: "dress them the same way"). Palettes live in
 * distro-themes.css as [data-theme] token blocks; this table carries
 * the names, dark behavior and the preview triples the Profiler's
 * theme gallery uses (bg, ink, accent). `dark`: true = dark-native,
 * "auto" = ships light+dark and follows the system scheme.
 */
export interface DistroEngine {
  id: string;
  name: string;
  dark: boolean | "auto";
  preview: [string, string, string];
}
export const DISTRO_ENGINES: DistroEngine[] = [
  { id: "bondi", name: "Bondi", dark: true, preview: ["#232326", "#e8e8ea", "#2ba3c7"] },
  { id: "cupertino", name: "Cupertino", dark: true, preview: ["#f5f5f7", "#1d1d1f", "#2ba3c7"] },
  { id: "omarchy", name: "Omarchy", dark: true, preview: ["#1a1b26", "#c0caf5", "#7aa2f7"] },
  { id: "ubuntu", name: "Ubuntu · Yaru", dark: "auto", preview: ["#f7f7f7", "#2c2c2c", "#e95420"] },
  { id: "fedora", name: "Fedora · Adwaita", dark: "auto", preview: ["#f6f5f4", "#241f19", "#3584e4"] },
  { id: "zorin", name: "Zorin OS", dark: "auto", preview: ["#f2f4f7", "#1b1d21", "#175ddc"] },
  { id: "elementary", name: "elementary OS", dark: "auto", preview: ["#fafafa", "#333333", "#3689e6"] },
  { id: "deepin", name: "Deepin", dark: "auto", preview: ["#eef4fb", "#1f2329", "#0081ff"] },
  { id: "pop", name: "Pop!_OS · COSMIC", dark: true, preview: ["#2b2b2b", "#f0efed", "#faa41a"] },
  { id: "mint", name: "Linux Mint · Mint-Y", dark: "auto", preview: ["#f4f6f1", "#2e3436", "#92b372"] },
  { id: "breeze", name: "KDE · Breeze", dark: "auto", preview: ["#eff0f1", "#232627", "#3daee9"] },
  { id: "opensuse", name: "openSUSE", dark: "auto", preview: ["#f3f6ee", "#26331f", "#73ba25"] },
  { id: "manjaro", name: "Manjaro", dark: true, preview: ["#22262a", "#e8ecef", "#16a085"] },
  { id: "endeavour", name: "EndeavourOS", dark: true, preview: ["#282a36", "#f8f8f2", "#7b5ea7"] },
  { id: "garuda", name: "Garuda", dark: true, preview: ["#1e1e2e", "#e5e9f0", "#e2436b"] },
  { id: "peppermint", name: "Peppermint", dark: true, preview: ["#2a2326", "#f2ecec", "#a31f34"] },
  { id: "mx", name: "MX Linux", dark: "auto", preview: ["#eceff1", "#263238", "#546e7a"] },
  { id: "phosh", name: "Phosh (mobile)", dark: true, preview: ["#242424", "#eeeeec", "#3584e4"] },
  { id: "ereader", name: "E-Reader (B/W high contrast)", dark: false, preview: ["#ffffff", "#000000", "#000000"] },
  { id: "redmond7", name: "Redmond 7 \u00b7 Aero", dark: false, preview: ["#cfe8f8", "#10243a", "#1f6fb2"] },
  { id: "redmondxp", name: "Redmond XP \u00b7 Luna", dark: false, preview: ["#ece9de", "#102040", "#0058e6"] },
  { id: "redmond8", name: "Redmond 8 \u00b7 Metro", dark: true, preview: ["#122028", "#f2f2f2", "#1ba1e2"] },
  { id: "redmond1011", name: "Redmond 10/11 \u00b7 Fluent", dark: true, preview: ["#202020", "#f5f5f5", "#0067c0"] },
  { id: "liquidglass", name: "Liquid Glass", dark: true, preview: ["#1a202c", "#f5f9ff", "#6fd3ff"] },
  { id: "materialyou", name: "Material You", dark: true, preview: ["#141218", "#e6e0e9", "#cfbcff"] },
];
const DISTRO_IDS = new Set(DISTRO_ENGINES.map((e) => e.id));

/** os-release ID -> engine, copied from the Profiler's detectTheme. */
const OSID_THEME: Record<string, string> = {
  cachyos: "breeze", linuxmint: "mint", mint: "mint", mx: "mx", debian: "fedora",
  pop: "pop", endeavouros: "endeavour", zorin: "zorin", manjaro: "manjaro", fedora: "fedora",
  ubuntu: "ubuntu", anduinos: "zorin", opensuse: "opensuse", "opensuse-leap": "opensuse",
  "opensuse-tumbleweed": "opensuse", nobara: "fedora", arch: "cupertino", bluestar: "breeze",
  bazzite: "breeze", elementary: "elementary", nixos: "fedora", biglinux: "breeze", neon: "breeze",
  peppermint: "peppermint", garuda: "garuda", omarchy: "omarchy", deepin: "deepin",
};

function detectDistroTheme(osId: string, desktop: string): string {
  const id = (osId || "").toLowerCase();
  const desk = (desktop || "").toLowerCase();
  if (desk.includes("phosh")) return "phosh";
  if (desk.includes("dde") || id === "deepin") return "deepin";
  return OSID_THEME[id] || "cupertino";
}

/** What the theme menu renders: backend themes plus distro metadata. */
export interface MenuThemeItem extends ThemeInfo {
  preview?: [string, string, string];
  metaLabel?: string;
  separator?: boolean;
}

export interface ThemeHooks {
  /** Render the theme menu anchored to `anchor`. */
  renderMenu(themes: MenuThemeItem[], activeId: string, anchor: HTMLElement): void;
  closeMenu(): void;
}

export class ThemeManager {
  private themes: ThemeInfo[] = [];
  private activeId = DEFAULT_THEME;
  /** When a distro pref is active, the engine it resolved to. */
  private resolvedId: string | null = null;

  constructor(private hooks: ThemeHooks) {}

  async init(): Promise<void> {
    try {
      this.themes = await api.listThemes();
    } catch {
      this.themes = []; // backend not ready — stay on built-in defaults
    }
    // One-time migration: builds before the distro themes AUTO-SAVED
    // the old forced default (grizzette) at every boot, making it
    // indistinguishable from a deliberate choice. Clear exactly that
    // artifact once so Auto can dress the app; any later deliberate
    // pick — grizzette included — is honored normally.
    try {
      if (!localStorage.getItem("fable.theme-forced-default-cleared")) {
        if (localStorage.getItem(STORAGE_KEY) === DEFAULT_THEME) {
          localStorage.removeItem(STORAGE_KEY);
        }
        localStorage.setItem("fable.theme-forced-default-cleared", "1");
      }
    } catch {
      // storage unavailable — fall through with whatever we have
    }
    await this.ensureDetected();
    const saved = localStorage.getItem(STORAGE_KEY);
    if (saved === "auto" || (saved && DISTRO_IDS.has(saved))) {
      await this.apply(saved);
      return;
    }
    if (!saved) {
      // First run: match the Profiler — Auto (dress for the distro).
      await this.apply("auto");
      return;
    }
    const pick =
      this.themes.find((t) => t.id === saved) ??
      this.themes.find((t) => t.id === DEFAULT_THEME) ??
      this.themes[0];
    if (pick) await this.apply(pick.id);
    else this.applyVars("", false); // no themes: tokens come from CSS fallback
  }

  get list(): ThemeInfo[] {
    return this.themes;
  }

  get active(): string {
    return this.activeId;
  }

  isDark(): boolean {
    if (this.resolvedId) {
      const e = DISTRO_ENGINES.find((x) => x.id === this.resolvedId);
      if (!e) return true;
      if (e.dark === "auto") {
        return window.matchMedia?.("(prefers-color-scheme: dark)").matches ?? true;
      }
      return e.dark;
    }
    return this.themes.find((t) => t.id === this.activeId)?.dark ?? false;
  }

  /** The engine an "auto"/distro pref currently resolves to. */
  get resolved(): string | null {
    return this.resolvedId;
  }

  /** Engine detected from the OS (set at startup, whatever the active
   * preference is) — the theme menu's "now:" label reads this, so it
   * never guesses. */
  private detectedEngineId: string | null = null;

  /** One distroInfo round-trip, cached: fills detectedEngineId. */
  private async ensureDetected(): Promise<void> {
    if (this.detectedEngineId) return;
    try {
      const info = await api.distroInfo();
      this.detectedEngineId = detectDistroTheme(info.os_id, info.desktop);
    } catch {
      // Backend unreachable: leave null; the menu shows "…" not a lie.
    }
  }

  async apply(id: string): Promise<void> {
    if (id === "auto" || DISTRO_IDS.has(id)) {
      await this.applyDistro(id);
      return;
    }
    try {
      const css = await api.applyTheme(id);
      const info = this.themes.find((t) => t.id === id);
      delete document.documentElement.dataset.theme;
      this.resolvedId = null;
      this.applyVars(css, info?.dark ?? false);
      this.activeId = id;
      localStorage.setItem(STORAGE_KEY, id);
    } catch {
      // apply failed (backend stub): keep current vars, don't corrupt UI.
    }
  }

  /** Distro engines dress the app via [data-theme] token blocks. */
  private async applyDistro(pref: string): Promise<void> {
    let resolved = pref;
    if (pref === "auto") {
      resolved = "cupertino";
      try {
        const info = await api.distroInfo();
        resolved = detectDistroTheme(info.os_id, info.desktop);
        this.detectedEngineId = resolved;
      } catch {
        // detection unavailable — native dress
      }
    }
    this.resolvedId = resolved;
    if (resolved === "cupertino") delete document.documentElement.dataset.theme;
    else document.documentElement.dataset.theme = resolved;
    // Clear any backend theme vars so the [data-theme] tokens rule.
    this.applyVars("", this.isDark());
    this.activeId = pref;
    try {
      localStorage.setItem(STORAGE_KEY, pref);
    } catch {
      // storage unavailable
    }
  }

  private applyVars(css: string, dark: boolean): void {
    const el = document.getElementById("ufm-theme-vars");
    if (el) el.textContent = css;
    // Native controls (scrollbars, inputs) follow the theme's scheme.
    document.documentElement.style.colorScheme = dark ? "dark" : "light";
    document.body.dataset.themeDark = String(dark);
  }

  openMenu(anchor: HTMLElement): void {
    const detectedId = this.resolvedId ?? this.detectedEngineId;
    const resolvedName =
      DISTRO_ENGINES.find((e) => e.id === detectedId)?.name ?? "…";
    const items: MenuThemeItem[] = [
      {
        id: "auto",
        name: "Auto (match my system)",
        dark: this.isDark(),
        metaLabel: `now: ${resolvedName}`,
        preview: DISTRO_ENGINES.find((e) => e.id === detectedId)?.preview,
      },
      // Round 7.1 (Dickles): alphabetical by display name — Auto stays
      // pinned first above, the built-in themes stay pinned last below.
      ...[...DISTRO_ENGINES]
        .sort((a, b) =>
          a.name.localeCompare(b.name, undefined, { sensitivity: "base" }),
        )
        .map((e) => ({
          id: e.id,
          name: e.name,
          dark: e.dark === true,
          metaLabel: "Distro theme",
          preview: e.preview,
        })),
    ];
    const backend =
      this.themes.length > 0
        ? this.themes
        : [{ id: DEFAULT_THEME, name: "Grizzette", dark: false }];
    items.push({ id: "__sep__", name: "", dark: false, separator: true });
    items.push(...backend);
    this.hooks.renderMenu(items, this.activeId, anchor);
  }

  closeMenu(): void {
    this.hooks.closeMenu();
  }
}
