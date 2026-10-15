/**
 * Fable Reader — the built-in ebook reader (Phase 6, spec §25).
 *
 * A reading surface, not a file view: one calm column of text, the
 * book's own title overhead, chapter navigation underfoot. Built on
 * the backend's EPUB decoder (spine order, DRM honesty); progress is
 * saved per book so a book reopens where it was left. The surface is
 * adaptive per the multi-format law — a measure-capped centered
 * column on a desktop window, full-bleed on a narrow one — and the
 * type steppers size the page text.
 *
 * Entry points are explicit (Quick Look's Read button, Open With ▸
 * Fable Reader): double-click keeps opening the OS default app.
 */
import { api, type BookOpen } from "./api";
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

class Reader {
  private overlay: HTMLElement | null = null;
  private pageEl: HTMLElement | null = null;
  private bodyEl: HTMLElement | null = null;
  private posEl: HTMLElement | null = null;
  private selectEl: HTMLSelectElement | null = null;
  private prevBtn: HTMLButtonElement | null = null;
  private nextBtn: HTMLButtonElement | null = null;
  private path = "";
  private book: BookOpen | null = null;
  private chapter = 0;
  private chapterTitle: string | null = null;
  private fontScale = 1;

  async open(path: string): Promise<void> {
    this.close();
    this.path = path;
    const overlay = el("div", "reader-overlay");
    const shell = el("div", "reader-shell");
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
    const closeBtn = el("button", "icon-btn reader-tool", "✕");
    closeBtn.title = "Close reader (Esc)";
    closeBtn.addEventListener("click", () => this.close());
    tools.append(smaller, bigger, closeBtn);
    head.append(titles, tools);
    this.bodyEl = el("div", "reader-body");
    this.pageEl = el("article", "reader-page");
    this.bodyEl.appendChild(this.pageEl);
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
    shell.append(head, this.bodyEl, foot);
    overlay.appendChild(shell);
    overlay.addEventListener("pointerdown", (e) => {
      if (e.target === overlay) this.close();
    });
    document.body.appendChild(overlay);
    this.overlay = overlay;
    document.addEventListener("keydown", this.keyHandler);

    try {
      const book = await api.openBook(path);
      this.book = book;
      this.fontScale = book.fontScale;
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
      await this.go(book.startChapter, true);
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

  private stepType(dir: number): void {
    this.fontScale = Math.min(2.2, Math.max(0.7, Math.round((this.fontScale + dir) * 10) / 10));
    this.applyType();
    void api.setReadingProgress(this.path, this.chapter, this.fontScale).catch(() => {});
  }

  private applyType(): void {
    if (this.pageEl) this.pageEl.style.fontSize = `${this.fontScale * 1.125}rem`;
  }

  private renderMessage(message: string): void {
    if (!this.pageEl) return;
    this.pageEl.replaceChildren(el("p", "reader-note", message));
    this.applyType();
  }

  private async go(index: number, first = false): Promise<void> {
    if (!this.book || this.book.chapterCount === 0) return;
    const target = Math.min(Math.max(index, 0), this.book.chapterCount - 1);
    if (!first && target === this.chapter && this.pageEl?.childElementCount) return;
    try {
      const view = await api.readChapter(this.path, target);
      this.chapter = view.index;
      this.chapterTitle = view.title;
      this.renderChapter(view.title, view.text);
      this.updateChrome();
      void api.setReadingProgress(this.path, this.chapter, this.fontScale).catch(() => {});
    } catch (err) {
      Toast.error(`Couldn't load that chapter: ${errText(err)}`);
    }
  }

  private renderChapter(title: string | null, text: string): void {
    if (!this.pageEl || !this.bodyEl) return;
    this.pageEl.replaceChildren();
    if (title) this.pageEl.appendChild(el("h2", "reader-chapter-title", title));
    const trimmed = text.trim();
    if (!trimmed) {
      this.pageEl.appendChild(
        el("p", "reader-note", "This chapter has no readable text — it may be pictures, or a part of the book the decoder can't read."),
      );
    } else {
      for (const block of trimmed.split(/\n\s*\n/)) {
        const p = el("p", "reader-para");
        const lines = block.split("\n");
        lines.forEach((line, i) => {
          if (i > 0) p.appendChild(document.createElement("br"));
          p.appendChild(document.createTextNode(line));
        });
        this.pageEl.appendChild(p);
      }
    }
    this.applyType();
    this.bodyEl.scrollTop = 0;
  }

  private updateChrome(): void {
    const count = this.book?.chapterCount ?? 0;
    if (this.posEl) {
      this.posEl.textContent = count > 0 ? `Chapter ${this.chapter + 1} of ${count}` : "";
    }
    if (this.prevBtn) this.prevBtn.disabled = count === 0 || this.chapter <= 0;
    if (this.nextBtn) this.nextBtn.disabled = count === 0 || this.chapter >= count - 1;
    if (this.selectEl) {
      if (this.selectEl.childElementCount !== count) {
        this.selectEl.replaceChildren();
        for (let i = 0; i < count; i++) {
          const o = document.createElement("option");
          o.value = String(i);
          o.textContent = `Chapter ${i + 1}`;
          this.selectEl.appendChild(o);
        }
      }
      const current = this.selectEl.options[this.chapter];
      if (current && this.chapterTitle) current.textContent = this.chapterTitle;
      this.selectEl.value = String(this.chapter);
      this.selectEl.style.display = count > 1 ? "" : "none";
    }
  }

  close(): void {
    if (this.overlay) {
      if (this.path && this.book && this.book.chapterCount > 0) {
        void api
          .setReadingProgress(this.path, this.chapter, this.fontScale)
          .catch(() => {});
      }
      this.overlay.remove();
      this.overlay = null;
      document.removeEventListener("keydown", this.keyHandler);
    }
    this.book = null;
    this.pageEl = null;
    this.bodyEl = null;
  }
}

const reader = new Reader();

export function openReader(path: string): void {
  void reader.open(path);
}
