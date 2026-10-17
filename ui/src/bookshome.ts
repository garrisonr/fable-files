/**
 * Books Home (Ebook P1 — "The Library", 2026-10-17): the books'
 * own room, a first-class Home beside Games Home in the same shelf
 * grammar. DESIGN principle (Dickles): "Shelves that keep
 * themselves" — the shelves organize THEMSELVES visibly: while the
 * Rust identification pass works, every `books-identify-progress`
 * event repaints the room, so unsorted books visibly settle onto
 * their Series / Author / All Books shelves (placement IS the
 * progress display — no spinner theater). A book shows shelved only
 * when its identity truly landed in the catalog.
 *
 * The room stages Fable's existing powers only: the reader
 * (openReader / openPdfReader), the catalog's book identity, and
 * the reading states (Want / Reading / Read). Covers come from the
 * books themselves (cached at identify time); a book without a
 * cover gets an honest typographic spine card, never a broken
 * image.
 */
import {
  api,
  fileSrc,
  type BookCard,
  type BookContinueRow,
  type BooksHomePayload,
} from "./api";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { openReader } from "./reader";
import { openPdfReader } from "./pdfreader";
import { reducedMotion } from "./motion";

export interface BooksHomeHooks {
  onExit: () => void;
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

type StateFilter = "all" | "want" | "reading" | "read";

const STATE_LABEL: Record<string, string> = {
  want: "Want to Read",
  reading: "Reading",
  read: "Read",
};

export class BooksHome {
  readonly el = document.createElement("div");
  active = false;
  private hooks: BooksHomeHooks;
  private payload: BooksHomePayload | null = null;
  private filter: StateFilter = "all";
  private bodyEl!: HTMLElement;
  private subEl!: HTMLElement;
  private unlisten: UnlistenFn | null = null;
  private refreshQueued = false;
  private kickedAutoIdentify = false;

  constructor(hooks: BooksHomeHooks) {
    this.hooks = hooks;
    this.el.className = "bookshome";
    this.el.hidden = true;
    this.buildChrome();
  }

  // ------------------------------------------------------------ chrome

  private buildChrome(): void {
    const toolbar = el("div", "rh-toolbar");
    const back = el("button", "icon-btn rh-back", "‹") as HTMLButtonElement;
    back.title = "Back to Files";
    back.setAttribute("aria-label", "Back to Files");
    back.addEventListener("click", () => this.hooks.onExit());
    const title = el("span", "rh-title", "Books");
    this.subEl = el("span", "bh-sub");
    const tidy = el("button", "bh-tidy", "Tidy the shelves") as HTMLButtonElement;
    tidy.title =
      "Read each book's own details and shelve it by series and author";
    tidy.addEventListener("click", () => void this.tidy());
    toolbar.append(back, title, this.subEl, tidy);
    const chips = el("div", "bh-chips");
    for (const f of ["all", "want", "reading", "read"] as StateFilter[]) {
      const chip = el(
        "button",
        "bh-chip" + (f === this.filter ? " on" : ""),
        f === "all" ? "All books" : STATE_LABEL[f],
      ) as HTMLButtonElement;
      chip.dataset.filter = f;
      chip.addEventListener("click", () => {
        this.filter = f;
        chips
          .querySelectorAll(".bh-chip")
          .forEach((c) => c.classList.toggle("on", c === chip));
        this.render();
      });
      chips.appendChild(chip);
    }
    this.bodyEl = el("div", "bh-body");
    this.el.append(toolbar, chips, this.bodyEl);
  }

  // ------------------------------------------------------------ lifecycle

  async open(): Promise<void> {
    this.active = true;
    this.el.hidden = false;
    // Paint FIRST, subscribe second: the shelves must never wait
    // on event registration (field lesson, 2026-10-17: awaiting
    // listen() ahead of the first refresh left the room blank
    // until an unrelated identify repainted it).
    await this.refresh(true);
    if (!this.unlisten) {
      void listen<{
        done: number;
        total: number;
        identified: number;
        failed: number;
      }>("books-identify-progress", () => {
        // The shelves keep themselves: each resolved book moves
        // the room. Coalesce bursts into one repaint per frame-ish.
        if (this.refreshQueued) return;
        this.refreshQueued = true;
        window.setTimeout(() => {
          this.refreshQueued = false;
          if (this.active) void this.refresh(false);
        }, 300);
      }).then((un) => {
        this.unlisten = un;
      });
    }
    // First visit with unsorted books waiting: start the pass —
    // the room tidies itself while he watches.
    if (
      !this.kickedAutoIdentify &&
      this.payload &&
      this.payload.counts.unsorted > 0
    ) {
      this.kickedAutoIdentify = true;
      void api.booksIdentify().catch(() => {});
    }
  }

  close(): void {
    this.active = false;
    if (this.unlisten) {
      this.unlisten();
      this.unlisten = null;
    }
  }

  /** Sidebar door counts: identified books, straight from the
   * payload endpoint (cheap COUNT queries — no scan). */
  async doorCount(): Promise<number> {
    const p = await api.booksHome().catch(() => null);
    return p ? p.counts.identified : 0;
  }

  private async tidy(): Promise<void> {
    const report = await api.booksIdentify().catch(() => null);
    if (report && !report.alreadyRunning) {
      await this.refresh(false);
    }
  }

  private async refresh(announce: boolean): Promise<void> {
    const p = await api.booksHome().catch(() => null);
    if (!p) {
      if (announce) {
        this.bodyEl.replaceChildren(
          el("p", "bh-empty", "The library couldn't be read just now."),
        );
      }
      return;
    }
    this.payload = p;
    this.render();
  }

  // ------------------------------------------------------------ render

  private passes(card: BookCard): boolean {
    return this.filter === "all" || card.state === this.filter;
  }

  private render(): void {
    const p = this.payload;
    if (!p) return;
    const c = p.counts;
    this.subEl.textContent =
      c.identified > 0
        ? `${c.identified.toLocaleString()} books · ${c.series} series · ${c.authors} authors`
        : "";
    const body = this.bodyEl;
    body.replaceChildren();

    if (c.identified === 0 && c.unsorted === 0) {
      body.appendChild(
        el(
          "p",
          "bh-empty",
          "No books yet. Add a folder of ebooks as a location and they'll shelve themselves here — by series, by author, gaps and all.",
        ),
      );
      return;
    }

    // Continue Reading — the reader's own record, identity-decorated.
    const cont = p.continueReading.filter((r) => this.passes(r.card));
    if (cont.length > 0) {
      body.appendChild(
        this.shelf("Continue Reading", cont.map((r) => this.continueCard(r))),
      );
    }

    const recent = p.recentlyAdded.filter((b) => this.passes(b));
    if (recent.length > 0) {
      body.appendChild(
        this.shelf("Recently Added", recent.map((b) => this.bookCard(b))),
      );
    }

    for (const s of p.series) {
      const books = s.books.filter((b) => this.passes(b));
      if (books.length === 0) continue;
      body.appendChild(
        this.shelf(
          s.series,
          books.map((b) => this.bookCard(b)),
          s.gapLabel ?? undefined,
        ),
      );
    }

    for (const a of p.authors) {
      const books = a.books.filter((b) => this.passes(b));
      if (books.length === 0) continue;
      body.appendChild(
        this.shelf(a.author, books.map((b) => this.bookCard(b))),
      );
    }

    const all = p.all.filter((b) => this.passes(b));
    if (all.length > 0) {
      body.appendChild(
        this.shelf("All Books", all.map((b) => this.bookCard(b))),
      );
    }

    // The unsorted pile: books on disk the pass hasn't resolved
    // yet. It SHRINKS live as identification lands — that shrink
    // is the room tidying itself.
    if (p.unsortedTotal > 0) {
      const cards = p.unsorted.map((u) => {
        const card = el("div", "bh-card bh-unsorted-card");
        card.append(
          el("div", "bh-spine bh-spine-waiting", u.name.replace(/\.[^.]+$/, "")),
          el("div", "bh-card-name", u.name),
          el("div", "bh-card-sub", "Waiting to be shelved"),
        );
        return card;
      });
      if (p.unsortedTotal > p.unsorted.length) {
        cards.push(
          el(
            "div",
            "bh-card bh-unsorted-card",
            `…and ${(p.unsortedTotal - p.unsorted.length).toLocaleString()} more`,
          ),
        );
      }
      body.appendChild(
        this.shelf(
          "Not Shelved Yet",
          cards,
          `${p.unsortedTotal.toLocaleString()} waiting — add the folder, then watch this shelf empty itself`,
        ),
      );
    }
  }

  private shelf(
    title: string,
    cards: HTMLElement[],
    note?: string,
  ): HTMLElement {
    const section = el("section", "bh-shelf");
    const head = el("div", "bh-shelf-head");
    head.appendChild(el("h2", "bh-shelf-title", title));
    if (note) head.appendChild(el("span", "bh-shelf-note", note));
    const rail = el("div", "bh-rail");
    for (const card of cards) {
      rail.appendChild(card);
      this.settleIn(card);
    }
    section.append(head, rail);
    return section;
  }

  /** Lively-but-calm settle: a card fades/rises into its place
   * once, honoring reduced motion (Welcome's convention). */
  private settleIn(card: HTMLElement): void {
    if (reducedMotion()) return;
    card.animate(
      [
        { opacity: 0, transform: "translateY(6px)" },
        { opacity: 1, transform: "translateY(0)" },
      ],
      { duration: 240, easing: "cubic-bezier(0.22, 1, 0.36, 1)" },
    );
  }

  // ------------------------------------------------------------ cards

  private openBook(card: BookCard): void {
    if (card.format === "pdf") openPdfReader(card.path);
    else openReader(card.path);
  }

  private stateBadge(card: BookCard): HTMLElement | null {
    if (!card.state) return null;
    return el("span", `bh-badge bh-badge-${card.state}`, STATE_LABEL[card.state]);
  }

  private stateMenu(card: BookCard, anchor: HTMLElement): void {
    // A tiny inline state picker under the card's button row.
    const row = anchor.parentElement;
    if (!row) return;
    row.querySelector(".bh-state-picker")?.remove();
    const picker = el("div", "bh-state-picker");
    for (const s of ["want", "reading", "read", null] as const) {
      const btn = el(
        "button",
        "bh-state-opt" + (card.state === s ? " on" : ""),
        s === null ? "No shelf mark" : STATE_LABEL[s],
      ) as HTMLButtonElement;
      btn.addEventListener("click", () => {
        void api
          .booksSetState(card.path, s)
          .then(() => this.refresh(false))
          .catch(() => {});
      });
      picker.appendChild(btn);
    }
    row.appendChild(picker);
  }

  private bookCard(card: BookCard): HTMLElement {
    const root = el("div", "bh-card");
    root.appendChild(this.coverEl(card));
    const title = el("div", "bh-card-title", card.title);
    title.title = card.title;
    root.appendChild(title);
    if (card.authorLine) {
      root.appendChild(el("div", "bh-card-sub", card.authorLine));
    }
    const meta: string[] = [];
    if (card.series) {
      meta.push(
        card.seriesIndex != null
          ? `${card.series} #${card.seriesIndex}`
          : card.series,
      );
    }
    if (card.year) meta.push(String(card.year));
    if (meta.length > 0) {
      root.appendChild(el("div", "bh-card-sub", meta.join(" · ")));
    }
    const badge = this.stateBadge(card);
    if (badge) root.appendChild(badge);
    const actions = el("div", "bh-card-actions");
    const mark = el(
      "button",
      "bh-mini-btn",
      card.state ? "Shelf mark ✓" : "Shelf mark",
    ) as HTMLButtonElement;
    mark.title = "Want to Read / Reading / Read";
    mark.addEventListener("click", (e) => {
      e.stopPropagation();
      this.stateMenu(card, mark);
    });
    actions.appendChild(mark);
    root.appendChild(actions);
    root.addEventListener("click", () => this.openBook(card));
    root.title = `${card.title} — open in the reader`;
    return root;
  }

  private continueCard(row: BookContinueRow): HTMLElement {
    const card = row.card;
    const root = el("div", "bh-card bh-continue");
    root.appendChild(this.coverEl(card));
    const title = el("div", "bh-card-title", card.title);
    title.title = card.title;
    root.appendChild(title);
    if (card.authorLine) {
      root.appendChild(el("div", "bh-card-sub", card.authorLine));
    }
    // Honest position: chapter/page against the known total, and
    // the last-read time the reader itself recorded.
    const where: string[] = [];
    if (row.chapter != null) {
      where.push(
        row.total != null
          ? `Chapter ${row.chapter + 1} of ${row.total}`
          : `Chapter ${row.chapter + 1}`,
      );
    }
    if (row.page != null) {
      where.push(
        row.total != null
          ? `Page ${row.page + 1} of ${row.total}`
          : `Page ${row.page + 1}`,
      );
    }
    if (row.updated) {
      const when = new Date(row.updated);
      if (!Number.isNaN(when.getTime())) {
        where.push(`last read ${when.toLocaleDateString()}`);
      }
    }
    root.appendChild(el("div", "bh-card-sub", where.join(" · ")));
    if (row.fraction != null) {
      const bar = el("div", "bh-progress");
      const fill = el("div", "bh-progress-fill");
      fill.style.width = `${Math.round(row.fraction * 100)}%`;
      bar.appendChild(fill);
      root.appendChild(bar);
    }
    const badge = this.stateBadge(card);
    if (badge) root.appendChild(badge);
    if (row.finishCandidate) {
      // Finishing is OFFERED, never auto-declared (DESIGN P1).
      const actions = el("div", "bh-card-actions");
      const done = el("button", "bh-mini-btn bh-finish", "Mark as Read") as HTMLButtonElement;
      done.addEventListener("click", (e) => {
        e.stopPropagation();
        void api
          .booksSetState(card.path, "read")
          .then(() => this.refresh(false))
          .catch(() => {});
      });
      actions.appendChild(done);
      root.appendChild(actions);
    }
    root.addEventListener("click", () => this.openBook(card));
    root.title = `${card.title} — pick up where you left off`;
    return root;
  }

  private coverEl(card: BookCard): HTMLElement {
    if (card.coverPath) {
      const img = document.createElement("img");
      img.className = "bh-cover";
      img.alt = "";
      img.loading = "lazy";
      img.src = fileSrc(card.coverPath);
      // A stale cover path must never render a broken glyph —
      // fall back to the typographic spine.
      img.addEventListener("error", () => {
        img.replaceWith(this.spineEl(card));
      });
      return img;
    }
    return this.spineEl(card);
  }

  private spineEl(card: BookCard): HTMLElement {
    const spine = el("div", "bh-spine");
    spine.append(
      el("span", "bh-spine-title", card.title),
      el("span", "bh-spine-author", card.authorLine || card.format.toUpperCase()),
    );
    return spine;
  }
}
