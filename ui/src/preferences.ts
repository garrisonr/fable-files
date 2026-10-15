/**
 * PreferencesDialog — the app's settings surface (UI round item 1).
 *
 * Hosts the settings that used to live only implicitly: the theme
 * (as a visual gallery — item 2), text size, grid icon size,
 * sidebar section state, and the ROM Collections switch. The dialog
 * owns no storage: every control writes through the shell's hooks
 * into the durable ui-prefs sidecar / the existing stores, so a
 * preference set here is the same preference the rest of the app
 * reads. Indexer retention settings are NOT here — they belong to
 * Indexer v2 and arrive with it.
 */
import type { ThemeInfo } from "./api";
import { DISTRO_ENGINES } from "./themes";

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
  sectionCollapsed(key: string): boolean;
  setSectionCollapsed(key: string, collapsed: boolean): void;
  showShortcuts(): void;
}

export class PreferencesDialog {
  private el: HTMLElement | null = null;
  private hooks: PreferencesHooks | null = null;
  private galleryEl: HTMLElement | null = null;

  open(hooks: PreferencesHooks): void {
    this.close();
    this.hooks = hooks;
    const overlay = document.createElement("div");
    overlay.className = "props-overlay";
    overlay.addEventListener("mousedown", (e) => {
      if (e.target === overlay) this.close();
    });
    const dialog = document.createElement("div");
    dialog.className = "prefs-dialog";
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

    dialog.appendChild(this.section("Appearance"));
    const gallery = document.createElement("div");
    this.galleryEl = gallery;
    dialog.appendChild(gallery);
    this.renderGallery();
    const galleryNote = document.createElement("p");
    galleryNote.className = "prefs-note";
    galleryNote.textContent =
      "Themes apply the moment you pick one — the whole app is the preview.";
    dialog.appendChild(galleryNote);

    dialog.appendChild(
      this.stepperRow(
        "Text size",
        () => `${Math.round(hooks.fontScale() * 100)}%`,
        () => hooks.setFontScale(hooks.fontScale() - 0.1),
        () => hooks.setFontScale(hooks.fontScale() + 0.1),
      ),
    );
    dialog.appendChild(
      this.stepperRow(
        "Icon size (grid view)",
        () => `${hooks.iconSize()} px`,
        () => hooks.stepIconSize(-1),
        () => hooks.stepIconSize(1),
      ),
    );

    dialog.appendChild(this.section("Sidebar"));
    dialog.appendChild(
      this.toggleRow(
        "ROM Collections",
        "Game systems in the sidebar, identified against the No-Intro / Redump / MAME lists.",
        hooks.romsOn(),
        (on) => hooks.setRomsOn(on),
      ),
    );
    dialog.appendChild(
      this.toggleRow(
        "Labels section expanded",
        null,
        !hooks.sectionCollapsed("ff-labels-collapsed"),
        (on) => hooks.setSectionCollapsed("ff-labels-collapsed", !on),
      ),
    );
    dialog.appendChild(
      this.toggleRow(
        "ROMs section expanded",
        null,
        !hooks.sectionCollapsed("ff-roms-collapsed"),
        (on) => hooks.setSectionCollapsed("ff-roms-collapsed", !on),
      ),
    );
    const scopeNote = document.createElement("p");
    scopeNote.className = "prefs-note";
    scopeNote.textContent =
      "ROM collection scope — which drives a system draws from — is set per system in the Roms system editor (sidebar ▸ Roms).";
    dialog.appendChild(scopeNote);

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
    closeBtn.focus();
  }

  close(): void {
    if (this.el) {
      this.el.remove();
      this.el = null;
    }
    this.galleryEl = null;
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
