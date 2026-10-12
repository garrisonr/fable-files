/**
 * Theme switching. Lists themes via list_themes, applies via apply_theme
 * (the Rust core returns CSS custom properties; the shell installs them
 * verbatim into #ufm-theme-vars and never invents a color), persists the
 * choice in localStorage. Default: grizzette.
 */
import { api, type ThemeInfo } from "./api";

const STORAGE_KEY = "fable.theme";
export const DEFAULT_THEME = "grizzette";

export interface ThemeHooks {
  /** Render the theme menu anchored to `anchor`. */
  renderMenu(themes: ThemeInfo[], activeId: string, anchor: HTMLElement): void;
  closeMenu(): void;
}

export class ThemeManager {
  private themes: ThemeInfo[] = [];
  private activeId = DEFAULT_THEME;

  constructor(private hooks: ThemeHooks) {}

  async init(): Promise<void> {
    try {
      this.themes = await api.listThemes();
    } catch {
      this.themes = []; // backend not ready — stay on built-in defaults
    }
    const saved = localStorage.getItem(STORAGE_KEY);
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
    return this.themes.find((t) => t.id === this.activeId)?.dark ?? false;
  }

  async apply(id: string): Promise<void> {
    try {
      const css = await api.applyTheme(id);
      const info = this.themes.find((t) => t.id === id);
      this.applyVars(css, info?.dark ?? false);
      this.activeId = id;
      localStorage.setItem(STORAGE_KEY, id);
    } catch {
      // apply failed (backend stub): keep current vars, don't corrupt UI.
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
    const items =
      this.themes.length > 0
        ? this.themes
        : [{ id: DEFAULT_THEME, name: "Grizzette", dark: false }];
    this.hooks.renderMenu(items, this.activeId, anchor);
  }

  closeMenu(): void {
    this.hooks.closeMenu();
  }
}
