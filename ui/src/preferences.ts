/**
 * PreferencesDialog — the app's settings surface.
 *
 * A left nav + one page at a time (cleanup, Dickles 2026-10-16:
 * "these preferences are still way too busy… clean it up!"). The
 * dialog owns no storage: every control writes through the shell's
 * hooks into the durable ui-prefs sidecar / the existing stores,
 * so a preference set here is the same preference the rest of the
 * app reads.
 *
 * Pages: Appearance (theme gallery, text/icon size, sidebar state,
 * ROM icon set) · Library & Indexing (Library Memory) · ROMs &
 * Emulation (the ROM Collections switch, the EmuSection cards —
 * fleet prep, updates, System files/BIOS, Reset ROM collection —
 * and Game Media) · Metadata Accounts (every provider account in
 * one place, with honest saved-vs-connected state) · Movies, TV &
 * Music (Media Resolution, Media Completion, Music Completion —
 * their accounts live under Metadata Accounts) · Advanced (Plugins,
 * Folder Templates).
 */
import type { ThemeInfo } from "./api";
import { DISTRO_ENGINES } from "./themes";
import { RomMediaSection } from "./rommedia";
import { EmuSection } from "./emu";
import { ROM_ICON_SET_INFO, type RomIconSet } from "./roms";
import { MediaResSection } from "./mediares";
import { MediaCompleteSection } from "./mediacomplete";
import { MusicCompleteSection } from "./musiccomplete";
import { LibraryMemorySection } from "./library";
import { PluginsSection } from "./plugins";
import { TemplatesSection } from "./templates";
import { MetadataAccountsPage } from "./accounts";

export type PrefsPage =
  | "appearance"
  | "library"
  | "roms"
  | "accounts"
  | "media"
  | "advanced";

const PREFS_PAGES: Array<{ id: PrefsPage; label: string }> = [
  { id: "appearance", label: "Appearance" },
  { id: "library", label: "Library & Indexing" },
  { id: "roms", label: "ROMs (Games) & Emulation" },
  { id: "accounts", label: "Metadata Accounts" },
  { id: "media", label: "Movies, TV & Music" },
  { id: "advanced", label: "Advanced" },
];

export interface PreferencesHooks {
  /** Backend (built-in) themes from list_themes. */
  backendThemes(): ThemeInfo[];
  /** The active theme preference id ("auto", an engine id, or a
   * built-in theme id). */
  activeTheme(): string;
  /** The engine the Auto/distro preference currently resolves to —
   * for the Auto card's preview + "now:" label. */
  resolvedEngine(): { name: string; preview: [string, string, string] } | null;
  applyTheme(id: string): Promise<void>;
  fontScale(): number;
  setFontScale(v: number): void;
  iconSize(): number;
  stepIconSize(dir: 1 | -1): void;
  romsOn(): boolean;
  setRomsOn(on: boolean): void;
  romIconSet(): RomIconSet;
  setRomIconSet(set: RomIconSet): void;
  /** Folder chooser for the Game Media section (the shell's picker). */
  pickFolder(initial: string | null): Promise<string | null>;
  /** Library Memory: rebuild one connected volume's index from
   *  scratch (the shell owns locations + the index tracker). */
  rebuildVolume(fsUuid: string): Promise<void>;
  /** Home base (steal 1): the local-only Recent/Frequent learning
   *  switch + the wipe. */
  rememberHomeFeed(): boolean;
  setRememberHomeFeed(on: boolean): void;
  clearHomeFeed(): Promise<void>;
  /** Check boxes for selection (steal 8). */
  checkBoxes(): boolean;
  setCheckBoxes(on: boolean): void;
  sectionCollapsed(key: string): boolean;
  setSectionCollapsed(key: string, collapsed: boolean): void;
  showShortcuts(): void;
  /** Replay the first-run welcome (it plays by itself; the seen
   * flag is untouched — replay is a visit, not a reset). */
  replayWelcome(): void;
}

export class PreferencesDialog {
  private el: HTMLElement | null = null;
  private hooks: PreferencesHooks | null = null;
  private galleryEl: HTMLElement | null = null;
  private lastPage: PrefsPage = "appearance";

  open(hooks: PreferencesHooks, page?: PrefsPage): void {
    this.close();
    this.hooks = hooks;
    const overlay = document.createElement("div");
    overlay.className = "props-overlay";
    overlay.addEventListener("mousedown", (e) => {
      if (e.target === overlay) this.close();
    });
    const dialog = document.createElement("div");
    dialog.className = "prefs-dialog prefs-dialog-nav";
    dialog.setAttribute("role", "dialog");
    dialog.setAttribute("aria-label", "Preferences");

    const head = document.createElement("div");
    head.className = "prefs-head";
    const title = document.createElement("h2");
    title.className = "prefs-title";
    title.textContent = "Preferences";
    const closeBtn = document.createElement("button");
    closeBtn.className = "icon-btn prefs-close";
    closeBtn.textContent = "✕";
    closeBtn.title = "Close";
    closeBtn.addEventListener("click", () => this.close());
    head.append(title, closeBtn);
    dialog.appendChild(head);

    const layout = document.createElement("div");
    layout.className = "prefs-layout";
    const nav = document.createElement("nav");
    nav.className = "prefs-nav";
    nav.setAttribute("aria-label", "Preferences sections");
    const pagesEl = document.createElement("div");
    pagesEl.className = "prefs-pages";
    layout.append(nav, pagesEl);
    dialog.appendChild(layout);

    const pageHosts = new Map<PrefsPage, HTMLElement>();
    const navBtns = new Map<PrefsPage, HTMLButtonElement>();
    const showPage = (id: PrefsPage): void => {
      this.lastPage = id;
      for (const [pid, host] of pageHosts) host.hidden = pid !== id;
      for (const [pid, btn] of navBtns) {
        btn.classList.toggle("active", pid === id);
        btn.setAttribute("aria-selected", pid === id ? "true" : "false");
      }
      pagesEl.scrollTop = 0;
    };
    for (const p of PREFS_PAGES) {
      const btn = document.createElement("button");
      btn.className = "prefs-nav-btn";
      btn.textContent = p.label;
      btn.setAttribute("role", "tab");
      btn.addEventListener("click", () => showPage(p.id));
      nav.appendChild(btn);
      navBtns.set(p.id, btn);
      const host = document.createElement("div");
      host.className = "prefs-page";
      host.dataset.page = p.id;
      pagesEl.appendChild(host);
      pageHosts.set(p.id, host);
    }

    this.buildAppearance(pageHosts.get("appearance")!, hooks);
    this.buildLibrary(pageHosts.get("library")!, hooks);
    this.buildRoms(pageHosts.get("roms")!, hooks);
    this.buildAccounts(pageHosts.get("accounts")!);
    this.buildMedia(pageHosts.get("media")!);
    this.buildAdvanced(pageHosts.get("advanced")!, hooks);

    const foot = document.createElement("div");
    foot.className = "prefs-foot";
    const keys = document.createElement("button");
    keys.className = "pill-btn";
    keys.textContent = "Keyboard Shortcuts…";
    keys.addEventListener("click", () => {
      this.close();
      hooks.showShortcuts();
    });
    const done = document.createElement("button");
    done.className = "pill-btn primary";
    done.textContent = "Done";
    done.addEventListener("click", () => this.close());
    foot.append(keys, done);
    dialog.appendChild(foot);

    overlay.appendChild(dialog);
    document.body.appendChild(overlay);
    this.el = overlay;
    showPage(page ?? this.lastPage);
    closeBtn.focus();
  }

  close(): void {
    if (this.el) {
      this.el.remove();
      this.el = null;
    }
    this.galleryEl = null;
  }

  // ---------------------------------------------------------------
  // Appearance: themes, sizes, sidebar state, ROM icon set.
  // ---------------------------------------------------------------
  private buildAppearance(page: HTMLElement, hooks: PreferencesHooks): void {
    page.appendChild(this.section("Theme"));
    const gallery = document.createElement("div");
    this.galleryEl = gallery;
    page.appendChild(gallery);
    this.renderGallery();
    const galleryNote = document.createElement("p");
    galleryNote.className = "prefs-note";
    galleryNote.textContent =
      "Themes apply the moment you pick one — the whole app is the preview.";
    page.appendChild(galleryNote);

    page.appendChild(
      this.stepperRow(
        "Text size",
        () => `${Math.round(hooks.fontScale() * 100)}%`,
        () => hooks.setFontScale(hooks.fontScale() - 0.1),
        () => hooks.setFontScale(hooks.fontScale() + 0.1),
      ),
    );
    page.appendChild(
      this.stepperRow(
        "Icon size (grid view)",
        () => `${hooks.iconSize()} px`,
        () => hooks.stepIconSize(-1),
        () => hooks.stepIconSize(1),
      ),
    );
    page.appendChild(
      this.toggleRow(
        "Check boxes for selection",
        "A check box on every file, for touch and trackpads alike. Fable turns this on by itself on touch screens.",
        hooks.checkBoxes(),
        (on) => hooks.setCheckBoxes(on),
      ),
    );

    page.appendChild(this.section("Sidebar"));
    page.appendChild(
      this.toggleRow(
        "Labels section expanded",
        null,
        !hooks.sectionCollapsed("ff-labels-collapsed"),
        (on) => hooks.setSectionCollapsed("ff-labels-collapsed", !on),
      ),
    );
    page.appendChild(
      this.toggleRow(
        "Games section expanded",
        null,
        !hooks.sectionCollapsed("ff-roms-collapsed"),
        (on) => hooks.setSectionCollapsed("ff-roms-collapsed", !on),
      ),
    );

    // ROM icon set chooser (Dickles, 2026-10-15): three complete sets —
    // 3D art, photorealistic, flat outline. Applies to the sidebar the
    // moment it's picked; the choice persists across launches.
    page.appendChild(this.section("Game icons"));
    const iconSetNote = document.createElement("p");
    iconSetNote.className = "prefs-note";
    iconSetNote.textContent =
      "Icon set — how each game system is pictured in the sidebar. Every system has its own icon in every set.";
    page.appendChild(iconSetNote);
    const picker = document.createElement("div");
    picker.className = "icon-set-picker";
    const pickerBtns: HTMLButtonElement[] = [];
    for (const info of ROM_ICON_SET_INFO) {
      const btn = document.createElement("button");
      btn.className = "icon-set-btn";
      btn.dataset.set = info.id;
      const strong = document.createElement("strong");
      strong.textContent = info.label;
      const blurb = document.createElement("span");
      blurb.textContent = info.blurb;
      btn.appendChild(strong);
      btn.appendChild(blurb);
      btn.addEventListener("click", () => {
        hooks.setRomIconSet(info.id);
        for (const b of pickerBtns) {
          b.setAttribute("aria-pressed", b.dataset.set === info.id ? "true" : "false");
        }
      });
      pickerBtns.push(btn);
      picker.appendChild(btn);
    }
    for (const b of pickerBtns) {
      b.setAttribute("aria-pressed", b.dataset.set === hooks.romIconSet() ? "true" : "false");
    }
    page.appendChild(picker);

    page.appendChild(this.section("Welcome"));
    const welcomeNote = document.createElement("p");
    welcomeNote.className = "prefs-note";
    welcomeNote.textContent =
      "The first-run welcome — the philosophy, the honest weight, the rooms, the introductions. It plays by itself and never nags. There are three doors back to it: this button, the Welcome Tour button in the About box, and Welcome Tour in the \u2630 menu.";
    page.appendChild(welcomeNote);
    const replay = document.createElement("button");
    replay.className = "pill-btn";
    replay.textContent = "Show the welcome again";
    replay.addEventListener("click", () => hooks.replayWelcome());
    page.appendChild(replay);
  }

  // ---------------------------------------------------------------
  // Library & Indexing: Library Memory (remembered drives).
  // ---------------------------------------------------------------
  private buildLibrary(page: HTMLElement, hooks: PreferencesHooks): void {
    const libHost = document.createElement("div");
    page.appendChild(libHost);
    void new LibraryMemorySection().mount(libHost, hooks);

    // Home base (Grand Pass steal 1): the learning that fills the
    // Home door's Recent and "You visit these a lot" shelves.
    // Local-only by doctrine — the copy says so, plainly.
    page.appendChild(this.section("Home base"));
    const homeNote = document.createElement("p");
    homeNote.className = "prefs-note";
    homeNote.textContent =
      "Home base learns — on this computer only — which files you open and which folders you visit, so the Home door can offer them back. The list never leaves this machine, and every entry can be forgotten from the Home door itself.";
    page.appendChild(homeNote);
    page.appendChild(
      this.toggleRow(
        "Remember recent files and frequent folders",
        null,
        hooks.rememberHomeFeed(),
        (on) => hooks.setRememberHomeFeed(on),
      ),
    );
    const clearHome = document.createElement("button");
    clearHome.className = "pill-btn";
    clearHome.textContent = "Clear recent & frequent now";
    clearHome.addEventListener("click", () => {
      void hooks.clearHomeFeed().then(() => {
        clearHome.textContent = "Cleared";
        window.setTimeout(() => {
          clearHome.textContent = "Clear recent & frequent now";
        }, 1500);
      });
    });
    page.appendChild(clearHome);
  }

  // ---------------------------------------------------------------
  // ROMs & Emulation: the Collections switch, Game Media, and the
  // EmuSection cards (Emulators, System files, Unified view,
  // Emulator updates, ES-DE, Drive access, ROM collection incl.
  // the Reset card).
  // ---------------------------------------------------------------
  private buildRoms(page: HTMLElement, hooks: PreferencesHooks): void {
    page.appendChild(
      this.toggleRow(
        "Games Collection",
        "Game systems in the sidebar, identified against the No-Intro / Redump / MAME lists.",
        hooks.romsOn(),
        (on) => hooks.setRomsOn(on),
      ),
    );
    const scopeNote = document.createElement("p");
    scopeNote.className = "prefs-note";
    scopeNote.textContent =
      "Games collection scope — which drives a system draws from — is set per system in the Games system editor (sidebar ▸ Games).";
    page.appendChild(scopeNote);

    // Game Media (ROM media enrichment): mounted from its own
    // backend state; its provider accounts live under Metadata
    // Accounts.
    const mediaHost = document.createElement("div");
    page.appendChild(mediaHost);
    void new RomMediaSection().mount(mediaHost, hooks);

    // Emulation: the Emulators + System files + updates cards.
    const emuHost = document.createElement("div");
    page.appendChild(emuHost);
    void new EmuSection().mount(emuHost, hooks);
  }

  // ---------------------------------------------------------------
  // Metadata Accounts: every provider account in one place.
  // ---------------------------------------------------------------
  private buildAccounts(page: HTMLElement): void {
    void new MetadataAccountsPage().mount(page);
  }

  // ---------------------------------------------------------------
  // Movies, TV & Music: Media Resolution, Media Completion,
  // Music Completion (their accounts live under Metadata Accounts).
  // ---------------------------------------------------------------
  private buildMedia(page: HTMLElement): void {
    const mresHost = document.createElement("div");
    page.appendChild(mresHost);
    void new MediaResSection().mount(mresHost);

    const mcHost = document.createElement("div");
    page.appendChild(mcHost);
    void new MediaCompleteSection().mount(mcHost);

    const muHost = document.createElement("div");
    page.appendChild(muHost);
    void new MusicCompleteSection().mount(muHost);
  }

  // ---------------------------------------------------------------
  // Advanced: Plugins + Folder Templates.
  // ---------------------------------------------------------------
  private buildAdvanced(page: HTMLElement, hooks: PreferencesHooks): void {
    const pluginsHost = document.createElement("div");
    page.appendChild(pluginsHost);
    void new PluginsSection().mount(pluginsHost);

    const templatesHost = document.createElement("div");
    page.appendChild(templatesHost);
    void new TemplatesSection().mount(templatesHost, hooks);
  }

  private section(title: string): HTMLElement {
    const h = document.createElement("h3");
    h.className = "prefs-section";
    h.textContent = title;
    return h;
  }

  /** The theme gallery (UI round item 2): a card per engine with a
   * live-color mock window — the same preview triples the Profiler
   * gallery uses (bg, ink, accent from themes.ts). Picking a card
   * applies the theme for real; the active card is ringed. */
  private renderGallery(): void {
    const hooks = this.hooks;
    const host = this.galleryEl;
    if (!hooks || !host) return;
    host.innerHTML = "";
    const active = hooks.activeTheme();
    const grid = document.createElement("div");
    grid.className = "prefs-gallery";
    const resolved = hooks.resolvedEngine();
    grid.appendChild(
      this.themeCard(
        "auto",
        "Auto (match my system)",
        resolved ? `now: ${resolved.name}` : "Distro theme",
        resolved?.preview,
        active === "auto",
      ),
    );
    const engines = [...DISTRO_ENGINES].sort((a, b) =>
      a.name.localeCompare(b.name, undefined, { sensitivity: "base" }),
    );
    for (const e of engines) {
      grid.appendChild(
        this.themeCard(e.id, e.name, "Distro theme", e.preview, active === e.id),
      );
    }
    host.appendChild(grid);
    const backend = hooks.backendThemes();
    const builtin = backend.length > 0 ? backend : [{ id: "grizzette", name: "Grizzette", dark: false }];
    const sub = document.createElement("div");
    sub.className = "prefs-subhead";
    sub.textContent = "Built-in themes";
    host.appendChild(sub);
    const grid2 = document.createElement("div");
    grid2.className = "prefs-gallery";
    for (const t of builtin) {
      grid2.appendChild(
        this.themeCard(t.id, t.name, t.dark ? "Dark" : "Light", undefined, active === t.id),
      );
    }
    host.appendChild(grid2);
  }

  private themeCard(
    id: string,
    name: string,
    meta: string,
    preview: [string, string, string] | undefined,
    active: boolean,
  ): HTMLButtonElement {
    const card = document.createElement("button");
    card.className = "prefs-theme-card" + (active ? " active" : "");
    card.setAttribute("aria-pressed", String(active));
    const mock = document.createElement("div");
    mock.className = "prefs-theme-mock";
    if (preview) {
      mock.style.background = preview[0];
      const bar = document.createElement("div");
      bar.className = "prefs-theme-bar";
      bar.style.background = preview[2];
      const line1 = document.createElement("div");
      line1.className = "prefs-theme-line";
      line1.style.background = preview[1];
      const line2 = document.createElement("div");
      line2.className = "prefs-theme-line short";
      line2.style.background = preview[1];
      mock.append(bar, line1, line2);
    } else {
      const glyph = document.createElement("div");
      glyph.className = "prefs-theme-glyph";
      glyph.textContent = "Aa";
      mock.appendChild(glyph);
    }
    const label = document.createElement("div");
    label.className = "prefs-theme-name";
    label.textContent = (active ? "✓ " : "") + name;
    const metaEl = document.createElement("div");
    metaEl.className = "prefs-theme-meta";
    metaEl.textContent = meta;
    card.append(mock, label, metaEl);
    card.addEventListener("click", () => {
      void this.hooks?.applyTheme(id).then(() => this.renderGallery());
    });
    return card;
  }

  private stepperRow(
    label: string,
    valueText: () => string,
    down: () => void,
    up: () => void,
  ): HTMLElement {
    const row = document.createElement("div");
    row.className = "prefs-row";
    const lab = document.createElement("span");
    lab.className = "prefs-row-label";
    lab.textContent = label;
    const minus = document.createElement("button");
    minus.className = "icon-btn prefs-step";
    minus.textContent = "−";
    minus.title = `Decrease ${label.toLowerCase()}`;
    const val = document.createElement("span");
    val.className = "prefs-step-value";
    val.textContent = valueText();
    const plus = document.createElement("button");
    plus.className = "icon-btn prefs-step";
    plus.textContent = "+";
    plus.title = `Increase ${label.toLowerCase()}`;
    minus.addEventListener("click", () => {
      down();
      val.textContent = valueText();
    });
    plus.addEventListener("click", () => {
      up();
      val.textContent = valueText();
    });
    row.append(lab, minus, val, plus);
    return row;
  }

  private toggleRow(
    label: string,
    note: string | null,
    on: boolean,
    fn: (on: boolean) => void,
  ): HTMLElement {
    const row = document.createElement("label");
    row.className = "prefs-row prefs-toggle";
    const box = document.createElement("input");
    box.type = "checkbox";
    box.checked = on;
    box.addEventListener("change", () => fn(box.checked));
    const text = document.createElement("span");
    text.className = "prefs-row-label";
    text.textContent = label;
    row.append(box, text);
    if (note) {
      const n = document.createElement("span");
      n.className = "prefs-note prefs-toggle-note";
      n.textContent = note;
      row.appendChild(n);
    }
    return row;
  }
}
