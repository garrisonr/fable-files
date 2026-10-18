/**
 * Home base (Grand Pass steal 1, from Sticky Steve's Explorer
 * study): one door combining your pinned folders, the files you
 * recently used, and the folders you visit most — learned from
 * use, on this computer only.
 *
 * THE SOVEREIGNTY LOCK (Steve's verdict, Dickles-approved): the
 * learning is local-only, lives in the catalog, is never synced
 * or sent anywhere, and there is NO "Recommended" feed — nothing
 * here exists that Fable didn't watch YOU do on THIS machine.
 * Every row can be Forgotten; every shelf can be Cleared; the
 * Preferences switch ("Remember recent files and frequent
 * folders") stops the learning entirely. The Forget pattern is
 * inherited from Library Memory.
 */
import {
  api,
  type HomeFeed,
  type HomeRecent,
  type HomeVisit,
} from "./api";
import { fileTypeGlyph, sectionGlyph } from "./sectionicons";

/** One pinned door, supplied by the shell (it owns the favorites
 * list and the navigation the click performs). */
export interface PinnedDoor {
  name: string;
  /** Glyph HTML (a sectionGlyph span's markup). */
  glyphHtml: string;
  title: string;
  open: () => void;
}

export interface HomeViewHooks {
  onExit: () => void;
  pinnedDoors(): Promise<PinnedDoor[]>;
  /** Frequent/recent folder row: navigate into it. */
  onOpenFolder(entry: {
    locationId: string;
    itemId: string;
    path: string;
    name: string;
  }): void;
  /** Recent file, single click: show it in its folder. */
  onRevealFile(entry: HomeRecent): void;
  /** Recent file, double click: open it with its app. */
  onOpenFile(entry: HomeRecent): void;
}

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

function whenLabel(unixSecs: number): string {
  const d = new Date(unixSecs * 1000);
  const now = new Date();
  const sameDay =
    d.getFullYear() === now.getFullYear() &&
    d.getMonth() === now.getMonth() &&
    d.getDate() === now.getDate();
  const time = d.toLocaleTimeString([], {
    hour: "numeric",
    minute: "2-digit",
  });
  if (sameDay) return `today at ${time}`;
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  if (
    d.getFullYear() === yesterday.getFullYear() &&
    d.getMonth() === yesterday.getMonth() &&
    d.getDate() === yesterday.getDate()
  ) {
    return `yesterday at ${time}`;
  }
  return d.toLocaleDateString([], {
    month: "short",
    day: "numeric",
    year: d.getFullYear() === now.getFullYear() ? undefined : "numeric",
  });
}

function dirnameOf(path: string): string {
  const i = path.lastIndexOf("/");
  return i > 0 ? path.slice(0, i) : path;
}

export class HomeView {
  readonly el = document.createElement("div");
  active = false;
  private body: HTMLElement;
  private hooks: HomeViewHooks;

  constructor(hooks: HomeViewHooks) {
    this.hooks = hooks;
    this.el.className = "homebase";
    this.el.hidden = true;

    const toolbar = el("div", "rh-toolbar");
    const back = el("button", "icon-btn rh-back", "‹") as HTMLButtonElement;
    back.title = "Back to Files";
    back.setAttribute("aria-label", "Back to Files");
    back.addEventListener("click", () => this.hooks.onExit());
    const title = el("span", "rh-title", "Home");
    const note = el(
      "span",
      "homebase-note",
      "Fable keeps this list on this computer only. Nothing is sent anywhere.",
    );
    toolbar.append(back, title, note);
    this.el.appendChild(toolbar);

    this.body = el("div", "homebase-body");
    this.el.appendChild(this.body);
  }

  async open(): Promise<void> {
    this.active = true;
    this.el.hidden = false;
    await this.refresh();
  }

  close(): void {
    this.active = false;
    this.el.hidden = true;
  }

  /** Repaint all three shelves from the catalog + the shell's
   * pinned doors. */
  async refresh(): Promise<void> {
    if (!this.active) return;
    const [feed, doors] = await Promise.all([
      api.getHomeFeed().catch((): HomeFeed | null => null),
      this.hooks.pinnedDoors().catch((): PinnedDoor[] => []),
    ]);
    this.body.innerHTML = "";
    if (doors.length > 0) this.body.appendChild(this.pinnedSection(doors));
    if (feed) {
      this.body.appendChild(this.recentSection(feed.recents));
      this.body.appendChild(this.frequentSection(feed.frequent));
    }
  }

  // ---------------------------------------------------------- sections

  private sectionHead(
    title: string,
    clear: (() => void) | null,
  ): HTMLElement {
    const head = el("div", "homebase-head");
    head.appendChild(el("h3", "homebase-h", title));
    if (clear) {
      const btn = el("button", "pill-btn homebase-clear", "Clear");
      btn.addEventListener("click", clear);
      head.appendChild(btn);
    }
    return head;
  }

  private pinnedSection(doors: PinnedDoor[]): HTMLElement {
    const sec = el("section", "homebase-section");
    sec.appendChild(this.sectionHead("Pinned", null));
    const grid = el("div", "homebase-doors");
    for (const door of doors) {
      const b = el("button", "homebase-door");
      b.title = door.title;
      const glyph = el("span", "homebase-glyph");
      glyph.innerHTML = door.glyphHtml;
      b.append(glyph, el("span", "homebase-door-name", door.name));
      b.addEventListener("click", () => door.open());
      grid.appendChild(b);
    }
    sec.appendChild(grid);
    return sec;
  }

  private forgetButton(label: string, forget: () => Promise<void>): HTMLButtonElement {
    const b = el("button", "icon-btn homebase-forget", "✕") as HTMLButtonElement;
    b.title = label;
    b.setAttribute("aria-label", label);
    const swallow = (e: Event): void => {
      e.stopPropagation();
    };
    b.addEventListener("click", (e) => {
      swallow(e);
      void forget().then(() => this.refresh());
    });
    b.addEventListener("dblclick", swallow);
    return b;
  }

  /** A shelf row: a div (never a button — the Forget control
   * rides inside it, and buttons can't nest). Enter = click. */
  private rowEl(title: string, onOpen: () => void): HTMLElement {
    const row = el("div", "homebase-row");
    row.title = title;
    row.tabIndex = 0;
    row.setAttribute("role", "button");
    row.addEventListener("keydown", (e) => {
      if (e.key === "Enter") onOpen();
    });
    return row;
  }

  private recentSection(recents: HomeRecent[]): HTMLElement {
    const sec = el("section", "homebase-section");
    sec.appendChild(
      this.sectionHead(
        "Recent",
        recents.length > 0
          ? () => void api.clearRecents().then(() => this.refresh())
          : null,
      ),
    );
    if (recents.length === 0) {
      sec.appendChild(
        el(
          "p",
          "homebase-empty",
          "Nothing here yet. Files you open — or peek at with Quick Look — will show up here.",
        ),
      );
      return sec;
    }
    const list = el("div", "homebase-list");
    for (const r of recents) {
      const row = this.rowEl(
        `${r.path}\nClick to show it in its folder; double-click to open it.`,
        () => this.hooks.onRevealFile(r),
      );
      const glyph = el("span", "homebase-glyph");
      if (r.kind === "folder") {
        glyph.appendChild(sectionGlyph("folder"));
      } else {
        // fileTypeGlyph returns an element or null (unknown type):
        // an unknown file still gets the plain document glyph via
        // a .txt probe name, never a broken slot.
        glyph.appendChild(
          fileTypeGlyph(r.name) ?? fileTypeGlyph("file.txt") ?? sectionGlyph("folder"),
        );
      }
      const texts = el("span", "homebase-texts");
      texts.append(
        el("span", "homebase-name", r.name),
        el(
          "span",
          "homebase-sub",
          `${dirnameOf(r.path)} · ${whenLabel(r.opened_at)}`,
        ),
      );
      row.append(
        glyph,
        texts,
        this.forgetButton(`Forget “${r.name}”`, () => api.forgetRecent(r.path)),
      );
      row.addEventListener("click", () => this.hooks.onRevealFile(r));
      row.addEventListener("dblclick", () => this.hooks.onOpenFile(r));
      list.appendChild(row);
    }
    sec.appendChild(list);
    return sec;
  }

  private frequentSection(frequent: HomeVisit[]): HTMLElement {
    const sec = el("section", "homebase-section");
    sec.appendChild(
      this.sectionHead(
        "You visit these a lot",
        frequent.length > 0
          ? () => void api.clearFrequent().then(() => this.refresh())
          : null,
      ),
    );
    if (frequent.length === 0) {
      sec.appendChild(
        el(
          "p",
          "homebase-empty",
          "Folders you visit will gather here as Fable learns your rounds.",
        ),
      );
      return sec;
    }
    const list = el("div", "homebase-list");
    for (const v of frequent) {
      const row = this.rowEl(v.path, () =>
        this.hooks.onOpenFolder({
          locationId: v.location_id,
          itemId: v.item_id,
          path: v.path,
          name: v.name,
        }),
      );
      const glyph = el("span", "homebase-glyph");
      glyph.innerHTML = sectionGlyph("folder").outerHTML;
      const texts = el("span", "homebase-texts");
      texts.append(
        el("span", "homebase-name", v.name),
        el(
          "span",
          "homebase-sub",
          `${v.path} · ${v.visits} visit${v.visits === 1 ? "" : "s"}, last ${whenLabel(v.last_at)}`,
        ),
      );
      row.append(
        glyph,
        texts,
        this.forgetButton(`Forget “${v.name}”`, () => api.forgetFrequent(v.path)),
      );
      row.addEventListener("click", () =>
        this.hooks.onOpenFolder({
          locationId: v.location_id,
          itemId: v.item_id,
          path: v.path,
          name: v.name,
        }),
      );
      list.appendChild(row);
    }
    sec.appendChild(list);
    return sec;
  }
}
