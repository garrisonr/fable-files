/**
 * Fable Reader — the built-in ebook reader (Phase 6, spec §25).
 *
 * A reading surface, not a file view: one calm column, the book's
 * own title overhead, chapter navigation underfoot. Entry points
 * are explicit (Quick Look's Read button, Open With ▸ Fable
 * Reader): double-click keeps opening the OS default app.
 *
 * THE READING ROOM (Ebook P2, 2026-10-17): chapters render RICH —
 * the publisher's own sanitized XHTML and scoped CSS, images in
 * flow, footnote and chapter links live as navigation (the backend
 * strips every live href; clicks arrive here as data attributes).
 * When a chapter's markup doesn't parse, the P1 plain-text render
 * below is the fallback, unchanged. Text settings (typeface, size,
 * line spacing, measure, light/sepia/dark theme) are the reader's
 * own, stored globally in ui-prefs and applied instantly. Position
 * is chapter + within-chapter scroll fraction, so a book reopens
 * at the same place, not just the same chapter. The chapter TEXT
 * still arrives on every ChapterView beside the rich form — the
 * fallback today, the read-aloud path tomorrow.
 */
import { api, type BookOpen, type ChapterView } from "./api";
import { Toast, errText } from "./toast";

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

/** The reader's own text settings (ui-prefs key `readerSettings`). */
interface ReaderSettings {
  fontFamily: "serif" | "sans" | "mono";
  fontScale: number;
  lineHeight: number;
  measure: "narrow" | "normal" | "wide";
  theme: "light" | "sepia" | "dark";
}

const READER_DEFAULTS: ReaderSettings = {
  fontFamily: "serif",
  fontScale: 1,
  lineHeight: 1.65,
  measure: "normal",
  theme: "dark",
};

const FONT_STACKS: Record<ReaderSettings["fontFamily"], string> = {
  serif: 'ui-serif, Georgia, "Times New Roman", serif',
  sans: 'ui-sans-serif, system-ui, "Segoe UI", Roboto, "Helvetica Neue", sans-serif',
  mono: 'ui-monospace, "Cascadia Mono", "JetBrains Mono", Consolas, monospace',
};

const MEASURES: Record<ReaderSettings["measure"], string> = {
  narrow: "30rem",
  normal: "38rem",
  wide: "48rem",
};

const LINE_HEIGHTS = [1.35, 1.5, 1.65, 1.85];

/** Validate a stored settings blob field by field; anything
 * unrecognized falls back to the default (or, for the type scale
 * only, to the book's legacy per-book scale — P1's one setting,
 * adopted once into the global one). */
function normalizeSettings(raw: unknown, legacyScale?: number): ReaderSettings {
  const s: ReaderSettings = { ...READER_DEFAULTS };
  if (legacyScale !== undefined && Number.isFinite(legacyScale)) {
    s.fontScale = Math.min(2.2, Math.max(0.7, legacyScale));
  }
  if (typeof raw !== "object" || raw === null) return s;
  const r = raw as Record<string, unknown>;
  if (r.fontFamily === "serif" || r.fontFamily === "sans" || r.fontFamily === "mono")
    s.fontFamily = r.fontFamily;
  if (typeof r.fontScale === "number" && Number.isFinite(r.fontScale))
    s.fontScale = Math.min(2.2, Math.max(0.7, r.fontScale));
  if (typeof r.lineHeight === "number" && LINE_HEIGHTS.includes(r.lineHeight))
    s.lineHeight = r.lineHeight;
  if (r.measure === "narrow" || r.measure === "normal" || r.measure === "wide")
    s.measure = r.measure;
  if (r.theme === "light" || r.theme === "sepia" || r.theme === "dark")
    s.theme = r.theme;
  return s;
}

interface GoOpts {
  first?: boolean;
  frac?: number;
  frag?: string;
}

class Reader {
  private overlay: HTMLElement | null = null;
  private shellEl: HTMLElement | null = null;
  private pageEl: HTMLElement | null = null;
  private bodyEl: HTMLElement | null = null;
  private posEl: HTMLElement | null = null;
  private selectEl: HTMLSelectElement | null = null;
  private prevBtn: HTMLButtonElement | null = null;
  private nextBtn: HTMLButtonElement | null = null;
  private styleEl: HTMLStyleElement | null = null;
  private settingsEl: HTMLElement | null = null;
  private path = "";
  private book: BookOpen | null = null;
  private chapter = 0;
  private prefs: Record<string, unknown> = {};
  private settings: ReaderSettings = { ...READER_DEFAULTS };
  private currentFrac = 0;
  private lastScrollSave = 0;

  async open(path: string): Promise<void> {
    this.close();
    this.path = path;
    const overlay = el("div", "reader-overlay");
    const shell = el("div", "reader-shell");
    this.shellEl = shell;
    shell.setAttribute("role", "dialog");
    shell.setAttribute("aria-label", "Book reader");
    const head = el("div", "reader-head");
    const titles = el("div", "reader-titles");
    const titleEl = el("div", "reader-title", "Opening book…");
    const authorEl = el("div", "reader-author");
    titles.append(titleEl, authorEl);
    const tools = el("div", "reader-tools");
    const smaller = el("button", "pill-btn reader-tool", "A−");
    smaller.title = "Smaller type";
    smaller.addEventListener("click", () => this.stepType(-0.1));
    const bigger = el("button", "pill-btn reader-tool", "A+");
    bigger.title = "Larger type";
    bigger.addEventListener("click", () => this.stepType(0.1));
    const settingsBtn = el("button", "pill-btn reader-tool", "Aa");
    settingsBtn.title = "Text settings";
    settingsBtn.addEventListener("click", () => this.toggleSettings());
    const closeBtn = el("button", "icon-btn reader-tool", "✕");
    closeBtn.title = "Close reader (Esc)";
    closeBtn.addEventListener("click", () => this.close());
    tools.append(smaller, bigger, settingsBtn, closeBtn);
    head.append(titles, tools);
    this.bodyEl = el("div", "reader-body");
    this.bodyEl.addEventListener("scroll", () => this.onScroll());
    this.pageEl = el("article", "reader-page");
    this.pageEl.addEventListener("click", (e) => this.onPageClick(e));
    this.bodyEl.appendChild(this.pageEl);
    this.settingsEl = el("div", "reader-settings");
    this.settingsEl.style.display = "none";
    const foot = el("div", "reader-foot");
    this.prevBtn = el("button", "pill-btn", "← Previous") as HTMLButtonElement;
    this.prevBtn.addEventListener("click", () => void this.go(this.chapter - 1));
    this.posEl = el("span", "reader-pos");
    this.selectEl = el("select", "reader-select") as HTMLSelectElement;
    this.selectEl.addEventListener("change", () => {
      void this.go(Number(this.selectEl!.value));
    });
    this.nextBtn = el("button", "pill-btn", "Next →") as HTMLButtonElement;
    this.nextBtn.addEventListener("click", () => void this.go(this.chapter + 1));
    foot.append(this.prevBtn, this.posEl, this.selectEl, this.nextBtn);
    shell.append(head, this.bodyEl, this.settingsEl, foot);
    overlay.appendChild(shell);
    overlay.addEventListener("pointerdown", (e) => {
      if (e.target === overlay) this.close();
      else if (
        this.settingsEl &&
        this.settingsEl.style.display !== "none" &&
        !this.settingsEl.contains(e.target as Node) &&
        (e.target as HTMLElement).textContent !== "Aa"
      )
        this.settingsEl.style.display = "none";
    });
    document.body.appendChild(overlay);
    this.overlay = overlay;
    document.addEventListener("keydown", this.keyHandler);

    try {
      this.prefs = await api.getUiPrefs().catch(() => ({}));
      const book = await api.openBook(path);
      this.book = book;
      // The global settings win; P1's per-book scale is adopted
      // only when no global settings exist yet.
      this.settings = normalizeSettings(
        this.prefs.readerSettings,
        this.prefs.readerSettings === undefined ? book.fontScale : undefined,
      );
      this.applySettings();
      titleEl.textContent = book.title ?? path.split("/").pop() ?? "Book";
      authorEl.textContent = book.author ?? "";
      if (book.drmProtected || book.chapterCount === 0) {
        this.renderMessage(
          book.note ??
            "This book has no readable chapters for the built-in reader. It will still open in your usual reader app.",
        );
        this.updateChrome();
        return;
      }
      if (book.note) Toast.show(book.note);
      await this.go(book.startChapter, {
        first: true,
        frac: book.startScrollFrac ?? undefined,
      });
    } catch (err) {
      this.renderMessage(`Couldn't open that book: ${errText(err)}`);
      this.updateChrome();
    }
  }

  private keyHandler = (e: KeyboardEvent): void => {
    if (!this.overlay) return;
    if (e.key === "Escape") this.close();
    else if (e.key === "ArrowLeft" && (e.target as HTMLElement)?.tagName !== "SELECT")
      void this.go(this.chapter - 1);
    else if (e.key === "ArrowRight" && (e.target as HTMLElement)?.tagName !== "SELECT")
      void this.go(this.chapter + 1);
  };

  // -- text settings -------------------------------------------------

  private stepType(dir: number): void {
    this.settings.fontScale =
      Math.round(
        Math.min(2.2, Math.max(0.7, this.settings.fontScale + dir)) * 10,
      ) / 10;
    this.applySettings();
    this.persistSettings();
    this.refreshSettingsUi();
  }

  private applySettings(): void {
    if (this.shellEl) this.shellEl.dataset.rtheme = this.settings.theme;
    if (!this.pageEl) return;
    const before = this.currentFrac;
    this.pageEl.style.fontSize = `${this.settings.fontScale * 1.125}rem`;
    this.pageEl.style.setProperty("--reader-font", FONT_STACKS[this.settings.fontFamily]);
    this.pageEl.style.setProperty("--reader-lh", String(this.settings.lineHeight));
    this.pageEl.style.setProperty("--reader-measure", MEASURES[this.settings.measure]);
    // A layout change moves the text under the reader's eyes;
    // re-anchor to the same fraction of the chapter.
    if (this.bodyEl && before > 0) {
      requestAnimationFrame(() => this.restoreFrac(before));
    }
  }

  private persistSettings(): void {
    this.prefs = { ...this.prefs, readerSettings: { ...this.settings } };
    void api.setUiPrefs(this.prefs).catch(() => {});
  }

  private toggleSettings(): void {
    if (!this.settingsEl) return;
    const show = this.settingsEl.style.display === "none";
    this.settingsEl.style.display = show ? "" : "none";
    if (show) this.refreshSettingsUi();
  }

  private seg<T extends string>(
    options: { value: T; label: string }[],
    current: T,
    onPick: (v: T) => void,
  ): HTMLElement {
    const wrap = el("div", "reader-seg");
    for (const o of options) {
      const b = el(
        "button",
        "reader-seg-btn" + (o.value === current ? " on" : ""),
        o.label,
      );
      b.setAttribute("aria-pressed", o.value === current ? "true" : "false");
      b.addEventListener("click", () => {
        onPick(o.value);
        this.refreshSettingsUi();
      });
      wrap.appendChild(b);
    }
    return wrap;
  }

  private refreshSettingsUi(): void {
    if (!this.settingsEl) return;
    const s = this.settings;
    const host = this.settingsEl;
    host.replaceChildren();
    const row = (label: string, control: HTMLElement): void => {
      const r = el("div", "reader-set-row");
      r.append(el("span", "reader-set-label", label), control);
      host.appendChild(r);
    };
    row(
      "Typeface",
      this.seg(
        [
          { value: "serif", label: "Book Serif" },
          { value: "sans", label: "Clean Sans" },
          { value: "mono", label: "Mono" },
        ],
        s.fontFamily,
        (v) => {
          this.settings.fontFamily = v;
          this.applySettings();
          this.persistSettings();
        },
      ),
    );
    const sizeCtl = el("div", "reader-seg");
    const minus = el("button", "reader-seg-btn", "A−");
    minus.addEventListener("click", () => this.stepType(-0.1));
    const plus = el("button", "reader-seg-btn", "A+");
    plus.addEventListener("click", () => this.stepType(0.1));
    sizeCtl.append(
      minus,
      el("span", "reader-set-value", `${Math.round(s.fontScale * 100)}%`),
      plus,
    );
    row("Size", sizeCtl);
    row(
      "Spacing",
      this.seg(
        LINE_HEIGHTS.map((h) => ({
          value: String(h) as "1.35" | "1.5" | "1.65" | "1.85",
          label: String(h),
        })),
        String(s.lineHeight) as "1.35" | "1.5" | "1.65" | "1.85",
        (v) => {
          this.settings.lineHeight = Number(v);
          this.applySettings();
          this.persistSettings();
        },
      ),
    );
    row(
      "Margins",
      this.seg(
        [
          { value: "narrow", label: "Narrow" },
          { value: "normal", label: "Normal" },
          { value: "wide", label: "Wide" },
        ],
        s.measure,
        (v) => {
          this.settings.measure = v;
          this.applySettings();
          this.persistSettings();
        },
      ),
    );
    row(
      "Theme",
      this.seg(
        [
          { value: "light", label: "Light" },
          { value: "sepia", label: "Sepia" },
          { value: "dark", label: "Dark" },
        ],
        s.theme,
        (v) => {
          this.settings.theme = v;
          this.applySettings();
          this.persistSettings();
        },
      ),
    );
  }

  // -- position ------------------------------------------------------

  private scrollFracNow(): number {
    if (!this.bodyEl) return 0;
    const max = this.bodyEl.scrollHeight - this.bodyEl.clientHeight;
    if (max <= 0) return 0;
    return Math.min(1, Math.max(0, this.bodyEl.scrollTop / max));
  }

  private onScroll(): void {
    this.currentFrac = this.scrollFracNow();
    // Durability, not a live feature: a timestamp-throttled save
    // (every 5 s of scrolling at most) plus the guaranteed saves
    // on navigation and close. No timer drives anything here.
    const now = Date.now();
    if (now - this.lastScrollSave >= 5000) {
      this.lastScrollSave = now;
      this.saveProgress();
    }
  }

  private saveProgress(): void {
    if (!this.path || !this.book || this.book.chapterCount === 0) return;
    void api
      .setReadingProgress(this.path, this.chapter, undefined, this.currentFrac)
      .catch(() => {});
  }

  private restoreFrac(frac: number): void {
    if (!this.bodyEl) return;
    const max = this.bodyEl.scrollHeight - this.bodyEl.clientHeight;
    this.bodyEl.scrollTop = Math.round(frac * Math.max(0, max));
    this.currentFrac = this.scrollFracNow();
  }

  private scrollToFrag(frag: string): void {
    if (!this.pageEl || !this.bodyEl) return;
    const target = this.pageEl.querySelector(`#${CSS.escape(frag)}`);
    if (!target) return;
    target.scrollIntoView({ block: "start" });
    target.classList.add("fable-flash");
    window.setTimeout(() => target.classList.remove("fable-flash"), 1600);
    requestAnimationFrame(() => {
      this.currentFrac = this.scrollFracNow();
    });
  }

  // -- links -----------------------------------------------------------

  private onPageClick(e: MouseEvent): void {
    const anchor = (e.target as HTMLElement).closest?.("a");
    if (!anchor) return;
    const ext = anchor.getAttribute("data-fable-ext");
    if (ext) {
      e.preventDefault();
      void api.openUrl(ext).catch(() => {
        Toast.show(`Couldn't open that link: ${ext}`);
      });
      return;
    }
    const link = anchor.getAttribute("data-fable-link");
    if (link === null) return; // a dead link kept as text — inert
    e.preventDefault();
    const idx = Number(link);
    const frag = anchor.getAttribute("data-fable-frag") ?? undefined;
    if (Number.isNaN(idx)) return;
    if (idx === this.chapter) {
      if (frag) this.scrollToFrag(frag);
      else if (this.bodyEl) this.bodyEl.scrollTop = 0;
    } else {
      void this.go(idx, { frag });
    }
  }

  // -- rendering -------------------------------------------------------

  private renderMessage(message: string): void {
    if (!this.pageEl) return;
    this.styleEl?.remove();
    this.styleEl = null;
    this.pageEl.replaceChildren(el("p", "reader-note", message));
  }

  private async go(index: number, opts: GoOpts = {}): Promise<void> {
    if (!this.book || this.book.chapterCount === 0) return;
    const target = Math.min(Math.max(index, 0), this.book.chapterCount - 1);
    if (!opts.first && target === this.chapter && this.pageEl?.childElementCount)
      return;
    try {
      const view = await api.readChapter(this.path, target);
      this.chapter = view.index;
      this.renderChapter(view);
      this.updateChrome();
      // Position after paint: a fragment target wins, then a
      // saved place, else the chapter top. The progress save
      // follows the positioning, so what lands on disk is the
      // place the reader actually sees.
      this.currentFrac = 0;
      if (this.bodyEl) this.bodyEl.scrollTop = 0;
      if (opts.frag) {
        requestAnimationFrame(() =>
          requestAnimationFrame(() => {
            this.scrollToFrag(opts.frag!);
            this.saveProgress();
          }),
        );
      } else if (opts.frac !== undefined && opts.frac > 0) {
        const frac = opts.frac;
        requestAnimationFrame(() =>
          requestAnimationFrame(() => {
            this.restoreFrac(frac);
            this.saveProgress();
          }),
        );
      } else {
        this.saveProgress();
      }
    } catch (err) {
      Toast.error(`Couldn't load that chapter: ${errText(err)}`);
    }
  }

  private renderChapter(view: ChapterView): void {
    if (!this.pageEl || !this.bodyEl) return;
    this.styleEl?.remove();
    this.styleEl = null;
    this.pageEl.replaceChildren();
    if (view.html && view.html.trim()) {
      // The rich surface: publisher structure, style, and images,
      // sanitized and scoped by the backend.
      const pub = el("div", "reader-pub");
      pub.innerHTML = view.html;
      const firstTag = pub.firstElementChild?.tagName;
      if (view.title && firstTag !== "H1" && firstTag !== "H2") {
        this.pageEl.appendChild(el("h2", "reader-chapter-title", view.title));
      }
      if (view.css) {
        this.styleEl = document.createElement("style");
        this.styleEl.textContent = view.css;
        this.pageEl.appendChild(this.styleEl);
      }
      this.pageEl.appendChild(pub);
    } else if (view.text.trim()) {
      // The P1 surface, unchanged: plain text in calm paragraphs.
      if (view.title)
        this.pageEl.appendChild(el("h2", "reader-chapter-title", view.title));
      const trimmed = view.text.trim();
      for (const block of trimmed.split(/\n\s*\n/)) {
        const p = el("p", "reader-para");
        const lines = block.split("\n");
        lines.forEach((line, i) => {
          if (i > 0) p.appendChild(document.createElement("br"));
          p.appendChild(document.createTextNode(line));
        });
        this.pageEl.appendChild(p);
      }
    } else {
      this.pageEl.appendChild(
        el(
          "p",
          "reader-note",
          "This chapter has no readable text — it may be pictures, or a part of the book the decoder can't read.",
        ),
      );
    }
    this.bodyEl.scrollTop = 0;
  }

  private updateChrome(): void {
    const count = this.book?.chapterCount ?? 0;
    if (this.posEl) {
      this.posEl.textContent =
        count > 0 ? `Chapter ${this.chapter + 1} of ${count}` : "";
    }
    if (this.prevBtn) this.prevBtn.disabled = count === 0 || this.chapter <= 0;
    if (this.nextBtn)
      this.nextBtn.disabled = count === 0 || this.chapter >= count - 1;
    if (this.selectEl) {
      if (this.selectEl.childElementCount !== count) {
        this.selectEl.replaceChildren();
        for (let i = 0; i < count; i++) {
          const o = document.createElement("option");
          o.value = String(i);
          o.textContent = this.book?.chapterTitles[i] ?? `Chapter ${i + 1}`;
          this.selectEl.appendChild(o);
        }
      }
      this.selectEl.value = String(this.chapter);
      this.selectEl.style.display = count > 1 ? "" : "none";
    }
  }

  close(): void {
    if (this.overlay) {
      this.saveProgress();
      this.overlay.remove();
      this.overlay = null;
      document.removeEventListener("keydown", this.keyHandler);
    }
    this.book = null;
    this.pageEl = null;
    this.bodyEl = null;
    this.shellEl = null;
    this.settingsEl = null;
    this.styleEl = null;
  }
}

const reader = new Reader();

export function openReader(path: string): void {
  void reader.open(path);
}
