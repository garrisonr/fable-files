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
import {
  api,
  type BookOpen,
  type ChapterView,
  type ModelSnapshot,
  type TtsSnapshot,
} from "./api";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
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
  /** Ebook P3 — Listen defaults: the narrator he chose last. */
  ttsVoice: string;
  ttsSpeed: number;
  ttsSleep: string;
}

/** The seven narrators of the Kokoro v1_0 cast (the crate's
 * inventory, mirrored: ids + labels must agree with ufm-tts). */
const TTS_VOICES: Array<{ id: string; label: string }> = [
  { id: "af_heart", label: "Heart · US, female" },
  { id: "af_bella", label: "Bella · US, female" },
  { id: "am_adam", label: "Adam · US, male" },
  { id: "am_michael", label: "Michael · US, male" },
  { id: "bf_emma", label: "Emma · British, female" },
  { id: "bm_george", label: "George · British, male" },
  { id: "bm_fable", label: "Fable · British, male" },
];

const TTS_SPEEDS = [0.75, 1, 1.25, 1.5, 1.75, 2];
const TTS_SLEEPS = ["off", "15", "30", "45", "60", "chapter"];

const READER_DEFAULTS: ReaderSettings = {
  fontFamily: "serif",
  fontScale: 1,
  lineHeight: 1.65,
  measure: "normal",
  theme: "dark",
  ttsVoice: "af_heart",
  ttsSpeed: 1,
  ttsSleep: "off",
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
  // Listen defaults (P3): additive — a prefs blob written by P2
  // parses to exactly the same settings it always did.
  if (typeof r.ttsVoice === "string" && TTS_VOICES.some((v) => v.id === r.ttsVoice))
    s.ttsVoice = r.ttsVoice;
  if (typeof r.ttsSpeed === "number" && TTS_SPEEDS.includes(r.ttsSpeed))
    s.ttsSpeed = r.ttsSpeed;
  if (typeof r.ttsSleep === "string" && TTS_SLEEPS.includes(r.ttsSleep))
    s.ttsSleep = r.ttsSleep;
  return s;
}

interface GoOpts {
  first?: boolean;
  frac?: number;
  frag?: string;
  /** A narration-driven chapter turn: the view follows the ear,
   * but the EYE mark is not saved — reading position and
   * listening position never write each other's stores. */
  narration?: boolean;
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
  // -- Listen (Ebook P3) ------------------------------------------
  // Playback truth lives in Rust; this class only paints the
  // engine's snapshots and sentence events. The highlight follows
  // the ear only after he has touched Listen in this session —
  // opening a book to READ while narration plays elsewhere must
  // never yank the page away from his eyes.
  private listenToggle: HTMLButtonElement | null = null;
  private listenStatus: HTMLElement | null = null;
  private voiceSel: HTMLSelectElement | null = null;
  private speedSel: HTMLSelectElement | null = null;
  private sleepSel: HTMLSelectElement | null = null;
  private modelEl: HTMLElement | null = null;
  private modelMsg: HTMLElement | null = null;
  private tts: TtsSnapshot | null = null;
  private view: ChapterView | null = null;
  private followEar = false;
  private ttsWired = false;
  private markEl: HTMLElement | null = null;
  private hlKey = "";
  private hlStream: { text: string; nodes: Text[]; offs: number[] } | null =
    null;
  private hlPtr = 0;
  private hlSentence = -1;

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
    shell.append(head, this.bodyEl, this.settingsEl, this.buildListenBar(), foot);
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
    // The Now Playing strip reads this to yield while the
    // reader for the same book is open.
    overlay.dataset.bookPath = path;
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
      this.followEar = false;
      this.syncListenControls();
      this.wireTtsEvents();
      void this.refreshTts();
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
      this.view = view;
      this.renderChapter(view);
      this.updateChrome();
      // A new chapter on the page: the highlight machinery
      // starts from a clean slate.
      this.markEl = null;
      this.hlKey = "";
      this.hlPtr = 0;
      this.hlSentence = -1;
      // Position after paint: a fragment target wins, then a
      // saved place, else the chapter top. The progress save
      // follows the positioning, so what lands on disk is the
      // place the reader actually sees. Narration-driven turns
      // never save: the ear is not the eye.
      const save = (): void => {
        if (!opts.narration) this.saveProgress();
      };
      this.currentFrac = 0;
      if (this.bodyEl) this.bodyEl.scrollTop = 0;
      if (opts.frag) {
        requestAnimationFrame(() =>
          requestAnimationFrame(() => {
            this.scrollToFrag(opts.frag!);
            save();
          }),
        );
      } else if (opts.frac !== undefined && opts.frac > 0) {
        const frac = opts.frac;
        requestAnimationFrame(() =>
          requestAnimationFrame(() => {
            this.restoreFrac(frac);
            save();
          }),
        );
      } else {
        save();
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

  // -- Listen: the bar ------------------------------------------------

  private buildListenBar(): HTMLElement {
    const bar = el("div", "listen-bar");
    this.listenToggle = el(
      "button",
      "pill-btn listen-toggle",
      "▶ Listen",
    ) as HTMLButtonElement;
    this.listenToggle.title = "Listen to this book";
    this.listenToggle.addEventListener("click", () => void this.toggleListen());
    const stop = el("button", "pill-btn listen-btn", "■") as HTMLButtonElement;
    stop.title = "Stop listening";
    stop.addEventListener("click", () => {
      this.followEar = false;
      void this.adoptCmd(api.ttsStop());
    });
    const prevS = el("button", "pill-btn listen-btn", "‹ Sentence") as HTMLButtonElement;
    prevS.title = "Previous sentence";
    prevS.addEventListener("click", () => void this.adoptCmd(api.ttsPrevSentence()));
    const nextS = el("button", "pill-btn listen-btn", "Sentence ›") as HTMLButtonElement;
    nextS.title = "Next sentence";
    nextS.addEventListener("click", () => void this.adoptCmd(api.ttsNextSentence()));
    const prevC = el("button", "pill-btn listen-btn", "« Chapter") as HTMLButtonElement;
    prevC.title = "Previous chapter";
    prevC.addEventListener("click", () => void this.adoptCmd(api.ttsPrevChapter()));
    const nextC = el("button", "pill-btn listen-btn", "Chapter »") as HTMLButtonElement;
    nextC.title = "Next chapter";
    nextC.addEventListener("click", () => void this.adoptCmd(api.ttsNextChapter()));

    this.voiceSel = el("select", "reader-select listen-sel") as HTMLSelectElement;
    this.voiceSel.title = "Narrator";
    for (const v of TTS_VOICES) {
      const o = document.createElement("option");
      o.value = v.id;
      o.textContent = v.label;
      this.voiceSel.appendChild(o);
    }
    this.voiceSel.addEventListener("change", () => {
      this.settings.ttsVoice = this.voiceSel!.value;
      this.persistSettings();
      if (this.tts?.path === this.path)
        void this.adoptCmd(api.ttsSetVoice(this.settings.ttsVoice));
    });

    this.speedSel = el("select", "reader-select listen-sel") as HTMLSelectElement;
    this.speedSel.title = "Narration speed";
    for (const s of TTS_SPEEDS) {
      const o = document.createElement("option");
      o.value = String(s);
      o.textContent = `${s}×`;
      this.speedSel.appendChild(o);
    }
    this.speedSel.addEventListener("change", () => {
      this.settings.ttsSpeed = Number(this.speedSel!.value);
      this.persistSettings();
      if (this.tts?.path === this.path)
        void this.adoptCmd(api.ttsSetSpeed(this.settings.ttsSpeed));
    });

    this.sleepSel = el("select", "reader-select listen-sel") as HTMLSelectElement;
    this.sleepSel.title = "Sleep timer";
    const sleepLabels: Record<string, string> = {
      off: "Sleep: off",
      "15": "Sleep: 15 min",
      "30": "Sleep: 30 min",
      "45": "Sleep: 45 min",
      "60": "Sleep: 60 min",
      chapter: "Sleep: end of chapter",
    };
    for (const s of TTS_SLEEPS) {
      const o = document.createElement("option");
      o.value = s;
      o.textContent = sleepLabels[s];
      this.sleepSel.appendChild(o);
    }
    this.sleepSel.addEventListener("change", () => {
      this.settings.ttsSleep = this.sleepSel!.value;
      this.persistSettings();
      void this.adoptCmd(api.ttsSetSleep(this.settings.ttsSleep));
    });

    this.listenStatus = el("span", "listen-status");
    bar.append(
      this.listenToggle,
      stop,
      prevS,
      nextS,
      prevC,
      nextC,
      this.voiceSel,
      this.speedSel,
      this.sleepSel,
      this.listenStatus,
    );

    // The model disclosure: shown when the narrator's voice
    // hasn't been downloaded yet — size up front, his tap.
    this.modelEl = el("div", "listen-model");
    this.modelEl.style.display = "none";
    this.modelMsg = el("span", "listen-model-msg");
    const dl = el("button", "pill-btn", "Download the voice") as HTMLButtonElement;
    dl.addEventListener("click", () => {
      void api
        .ttsModelDownload()
        .then((m) => this.showModel(m))
        .catch(() => {});
    });
    this.modelEl.append(this.modelMsg, dl);
    const wrap = el("div", "listen-wrap");
    wrap.append(bar, this.modelEl);
    return wrap;
  }

  private syncListenControls(): void {
    if (this.voiceSel) this.voiceSel.value = this.settings.ttsVoice;
    if (this.speedSel) this.speedSel.value = String(this.settings.ttsSpeed);
    if (this.sleepSel) this.sleepSel.value = this.settings.ttsSleep;
  }

  // -- Listen: engine conversation -------------------------------------

  private wireTtsEvents(): void {
    if (this.ttsWired) return;
    this.ttsWired = true;
    void listen<TtsSnapshot>("tts-state", (e) => this.adopt(e.payload)).catch(
      () => {},
    );
    void listen<{ path: string | null; chapter: number; sentence: number }>(
      "tts-sentence",
      (e) => {
        const p = e.payload;
        if (!this.overlay || !this.followEar) return;
        if (!p.path || p.path !== this.path) return;
        if (p.chapter !== this.chapter) {
          void this.go(p.chapter, { narration: true }).then(() =>
            this.applyHighlight(p.sentence),
          );
        } else {
          this.applyHighlight(p.sentence);
        }
      },
    ).catch(() => {});
    void listen<ModelSnapshot>("tts-model", (e) =>
      this.showModel(e.payload),
    ).catch(() => {});
  }

  private async refreshTts(): Promise<void> {
    const snap = await api.ttsSnapshot().catch(() => null);
    if (snap) this.adopt(snap);
  }

  private async adoptCmd(p: Promise<TtsSnapshot>): Promise<void> {
    const snap = await p.catch(() => null);
    if (snap) this.adopt(snap);
  }

  /** Adopt a snapshot wholesale — the engine's word is the UI. */
  private adopt(snap: TtsSnapshot): void {
    this.tts = snap;
    if (!this.listenToggle || !this.listenStatus) return;
    const mine = snap.path !== null && snap.path === this.path;
    if (mine && (snap.state === "playing" || snap.state === "loading")) {
      this.listenToggle.textContent =
        snap.state === "playing" ? "❚❚ Pause" : "Loading…";
    } else if (mine && snap.state === "paused") {
      this.listenToggle.textContent = "▶ Resume";
    } else {
      this.listenToggle.textContent = "▶ Listen";
    }
    if (snap.state === "stopped" || snap.state === "idle") {
      this.followEar = false;
      this.clearHighlight();
    }
    if (mine) {
      if (this.voiceSel) this.voiceSel.value = snap.voice;
      if (this.speedSel) this.speedSel.value = String(snap.speed);
      if (this.sleepSel) this.sleepSel.value = snap.sleep;
    }
    let status = "";
    if (snap.message && (mine || snap.state === "error")) {
      status = snap.message;
    } else if (
      mine &&
      (snap.state === "playing" || snap.state === "paused" || snap.state === "loading")
    ) {
      status =
        `Chapter ${(snap.chapter ?? 0) + 1} of ${snap.chapterCount}` +
        (snap.sentenceCount > 0
          ? ` · sentence ${(snap.sentence ?? 0) + 1} of ${snap.sentenceCount}`
          : "");
    } else if (!mine && snap.path && snap.state !== "idle" && snap.state !== "stopped") {
      status = "The narrator is reading another book";
    } else if (this.book?.listenChapter != null) {
      status = `Continue listening — Chapter ${this.book.listenChapter + 1}`;
    }
    this.listenStatus.textContent = status;
  }

  private async toggleListen(): Promise<void> {
    const snap = this.tts ?? (await api.ttsSnapshot().catch(() => null));
    if (snap && snap.path === this.path) {
      if (snap.state === "playing") {
        await this.adoptCmd(api.ttsPause());
        return;
      }
      if (snap.state === "paused") {
        this.followEar = true;
        await this.adoptCmd(api.ttsResume());
        return;
      }
      if (snap.state === "loading") {
        this.followEar = false;
        await this.adoptCmd(api.ttsStop());
        return;
      }
    }
    await this.startListen();
  }

  private async startListen(): Promise<void> {
    const m = await api.ttsModelStatus().catch(() => null);
    if (!m || m.state !== "ready") {
      this.showModel(m);
      return;
    }
    if (this.modelEl) this.modelEl.style.display = "none";
    await this.pushDefaults();
    this.followEar = true;
    await this.adoptCmd(api.ttsPlay(this.path));
  }

  /** His saved narrator choices become the engine's, before a
   * fresh start (a resume keeps whatever the engine holds). */
  private async pushDefaults(): Promise<void> {
    const snap = this.tts ?? (await api.ttsSnapshot().catch(() => null));
    if (!snap) return;
    if (snap.voice !== this.settings.ttsVoice)
      await this.adoptCmd(api.ttsSetVoice(this.settings.ttsVoice));
    if (Math.abs(snap.speed - this.settings.ttsSpeed) > 0.001)
      await this.adoptCmd(api.ttsSetSpeed(this.settings.ttsSpeed));
    if (snap.sleep !== this.settings.ttsSleep)
      await this.adoptCmd(api.ttsSetSleep(this.settings.ttsSleep));
  }

  private showModel(m: ModelSnapshot | null): void {
    if (!this.modelEl || !this.modelMsg) return;
    if (m && m.state === "ready") {
      this.modelEl.style.display = "none";
      // He asked to listen; the voice just landed — begin.
      if (this.overlay) void this.startListen();
      return;
    }
    this.modelEl.style.display = "";
    if (!m) {
      this.modelMsg.textContent =
        "The narrator's voice isn't here yet. It downloads once — 334 MB — then it's yours, offline. Deleting it later is just deleting a folder.";
      return;
    }
    if (m.state === "downloading") {
      const pct =
        m.total > 0 ? Math.floor((m.downloaded / m.total) * 100) : 0;
      this.modelMsg.textContent = `Downloading the narrator's voice… ${pct}%`;
    } else if (m.state === "verifying") {
      this.modelMsg.textContent = "Downloaded — checking it against its fingerprint…";
    } else if (m.state === "extracting") {
      this.modelMsg.textContent = "Unpacking the voice…";
    } else if (m.state === "error") {
      this.modelMsg.textContent =
        m.message ?? "The download didn't finish — try again.";
    } else {
      this.modelMsg.textContent =
        "The narrator's voice isn't here yet. It downloads once — 334 MB — then it's yours, offline. Deleting it later is just deleting a folder.";
    }
  }

  // -- Listen: the sentence highlight -----------------------------------
  // The rendered page is not the chapter text: rich HTML has its
  // own whitespace. So the search runs on a whitespace-collapsed
  // stream of the page's text nodes, mapped back to (node,
  // offset) pairs, and moves FORWARD from the last find —
  // repeated sentences land in order. A sentence that can't be
  // found gets no mark; a wrong mark is worse than none.

  private static normWs(s: string): string {
    return s.replace(/\s+/g, " ");
  }

  private streamFor(view: ChapterView): {
    text: string;
    nodes: Text[];
    offs: number[];
  } {
    const key = `${view.index}:${view.text.length}`;
    if (this.hlStream && this.hlKey === key) return this.hlStream;
    const chars: string[] = [];
    const nodes: Text[] = [];
    const offs: number[] = [];
    if (this.pageEl) {
      const walker = document.createTreeWalker(
        this.pageEl,
        NodeFilter.SHOW_TEXT,
      );
      let node: Node | null;
      let lastWasSpace = true; // trims the stream's leading edge
      while ((node = walker.nextNode())) {
        const parent = node.parentElement;
        if (!parent) continue;
        if (parent.closest("style, script, .reader-chapter-title")) continue;
        const text = node as Text;
        const data = text.data;
        for (let i = 0; i < data.length; i++) {
          const ch = data[i];
          if (/\s/.test(ch)) {
            if (lastWasSpace) continue;
            chars.push(" ");
            nodes.push(text);
            offs.push(i);
            lastWasSpace = true;
          } else {
            chars.push(ch);
            nodes.push(text);
            offs.push(i);
            lastWasSpace = false;
          }
        }
      }
    }
    this.hlStream = { text: chars.join(""), nodes, offs };
    this.hlKey = key;
    return this.hlStream;
  }

  private clearHighlight(): void {
    if (this.markEl) {
      const m = this.markEl;
      m.replaceWith(...Array.from(m.childNodes));
      this.markEl = null;
      this.pageEl?.normalize();
      // The DOM just changed shape; the cached stream is stale.
      this.hlKey = "";
    }
    this.hlSentence = -1;
  }

  private applyHighlight(idx: number): void {
    if (!this.pageEl || !this.bodyEl || !this.view) return;
    if (idx === this.hlSentence && this.markEl) return;
    this.clearHighlight();
    const view = this.view;
    const range = view.sentences[idx];
    if (!range) return;
    const needle = Reader.normWs(view.text.slice(range.start, range.end)).trim();
    if (!needle) return;
    const stream = this.streamFor(view);
    let at = stream.text.indexOf(needle, this.hlPtr);
    if (at < 0 && this.hlPtr > 0) at = stream.text.indexOf(needle);
    if (at < 0) return; // silent no-op — no mark beats a wrong mark
    const endAt = at + needle.length;
    try {
      const r = document.createRange();
      r.setStart(stream.nodes[at], stream.offs[at]);
      r.setEnd(stream.nodes[endAt - 1], stream.offs[endAt - 1] + 1);
      const mark = el("mark", "tts-mark");
      mark.appendChild(r.extractContents());
      r.insertNode(mark);
      this.markEl = mark;
      this.hlSentence = idx;
      this.hlPtr = endAt;
      const mr = mark.getBoundingClientRect();
      const br = this.bodyEl.getBoundingClientRect();
      if (mr.top < br.top || mr.bottom > br.bottom) {
        mark.scrollIntoView({ block: "nearest" });
      }
    } catch {
      // A range that won't wrap cleanly leaves the page as it was.
    }
  }

  close(): void {
    if (this.overlay) {
      this.saveProgress();
      this.overlay.remove();
      this.overlay = null;
      document.removeEventListener("keydown", this.keyHandler);
    }
    // Closing the reader never stops the narrator — playback
    // lives in Rust and survives the room (the Now Playing
    // strip in the shell takes over the display).
    this.book = null;
    this.view = null;
    this.markEl = null;
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
