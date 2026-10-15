/**
 * Fable PDF Reader — paginated documents in the reader shell
 * (Dickles' ruling 2026-10-15: PDFs get a full reader too).
 *
 * The EPUB reader's calm shell, a page-image model underneath:
 * pages render on the backend (poppler, reading resolution, cached)
 * one at a time around the page being read, so a 500-page document
 * opens like a 5-page one. Navigation is buttons, a page field, the
 * document's own bookmarks, and the keyboard; zoom is Fit page /
 * Fit width / honest percents. Progress (page + zoom) persists per
 * document, so a PDF reopens on the page it was left.
 *
 * Entry points are explicit (Quick Look's Read button, Open With ▸
 * Fable Reader): double-click keeps opening the OS default app.
 */
import { api, type PdfOpen } from "./api";
import { Toast, errText } from "./toast";

const PERCENT_STEPS = ["50", "75", "100", "125", "150", "200"];

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

class PdfReader {
  private overlay: HTMLElement | null = null;
  private stageEl: HTMLElement | null = null;
  private imgEl: HTMLImageElement | null = null;
  private msgEl: HTMLElement | null = null;
  private pageInput: HTMLInputElement | null = null;
  private totalEl: HTMLElement | null = null;
  private outlineSel: HTMLSelectElement | null = null;
  private zoomSel: HTMLSelectElement | null = null;
  private prevBtn: HTMLButtonElement | null = null;
  private nextBtn: HTMLButtonElement | null = null;
  private path = "";
  private doc: PdfOpen | null = null;
  private page = 0;
  private zoomMode = "fit-width";
  private renderSeq = 0;
  private resizeTimer: number | undefined;

  async open(path: string): Promise<void> {
    this.close();
    this.path = path;
    const overlay = el("div", "reader-overlay");
    const shell = el("div", "reader-shell pdf-shell");
    shell.setAttribute("role", "dialog");
    shell.setAttribute("aria-label", "PDF reader");
    const head = el("div", "reader-head");
    const titles = el("div", "reader-titles");
    const titleEl = el("div", "reader-title", "Opening document…");
    const authorEl = el("div", "reader-author");
    titles.append(titleEl, authorEl);
    const tools = el("div", "reader-tools");
    const closeBtn = el("button", "icon-btn reader-tool", "✕");
    closeBtn.title = "Close reader (Esc)";
    closeBtn.addEventListener("click", () => this.close());
    tools.append(closeBtn);
    head.append(titles, tools);

    this.stageEl = el("div", "reader-body pdf-stage");
    const wrap = el("div", "pdf-page-wrap");
    this.imgEl = document.createElement("img");
    this.imgEl.className = "pdf-page-img";
    this.imgEl.alt = "PDF page";
    this.imgEl.style.display = "none";
    this.msgEl = el("p", "reader-note pdf-note", "Rendering page…");
    wrap.append(this.imgEl, this.msgEl);
    this.stageEl.appendChild(wrap);

    const foot = el("div", "reader-foot");
    this.prevBtn = el("button", "pill-btn", "← Previous") as HTMLButtonElement;
    this.prevBtn.addEventListener("click", () => void this.go(this.page - 1));
    const pos = el("span", "reader-pos pdf-pos");
    pos.append(document.createTextNode("Page "));
    this.pageInput = document.createElement("input");
    this.pageInput.type = "number";
    this.pageInput.className = "pdf-page-input";
    this.pageInput.min = "1";
    this.pageInput.addEventListener("change", () => {
      const n = Number(this.pageInput!.value);
      if (Number.isFinite(n)) void this.go(n - 1);
    });
    this.pageInput.addEventListener("keydown", (e) => {
      if (e.key === "Enter") (e.target as HTMLInputElement).blur();
      e.stopPropagation();
    });
    this.totalEl = el("span", "", "");
    pos.append(this.pageInput, this.totalEl);
    this.outlineSel = el("select", "reader-select pdf-outline") as HTMLSelectElement;
    this.outlineSel.title = "Bookmarks";
    this.outlineSel.addEventListener("change", () => {
      const entry = this.doc?.outline[Number(this.outlineSel!.value)];
      if (entry) void this.go(entry.page - 1);
    });
    this.zoomSel = el("select", "reader-select pdf-zoom") as HTMLSelectElement;
    this.zoomSel.title = "Zoom";
    const fitPage = document.createElement("option");
    fitPage.value = "fit-page";
    fitPage.textContent = "Fit page";
    const fitWidth = document.createElement("option");
    fitWidth.value = "fit-width";
    fitWidth.textContent = "Fit width";
    this.zoomSel.append(fitPage, fitWidth);
    for (const step of PERCENT_STEPS) {
      const o = document.createElement("option");
      o.value = step;
      o.textContent = `${step}%`;
      this.zoomSel.appendChild(o);
    }
    this.zoomSel.addEventListener("change", () => {
      this.zoomMode = this.zoomSel!.value;
      void this.go(this.page, true);
      this.saveProgress();
    });
    this.nextBtn = el("button", "pill-btn", "Next →") as HTMLButtonElement;
    this.nextBtn.addEventListener("click", () => void this.go(this.page + 1));
    foot.append(this.prevBtn, pos, this.outlineSel, this.zoomSel, this.nextBtn);
    shell.append(head, this.stageEl, foot);
    overlay.appendChild(shell);
    overlay.addEventListener("pointerdown", (e) => {
      if (e.target === overlay) this.close();
    });
    document.body.appendChild(overlay);
    this.overlay = overlay;
    document.addEventListener("keydown", this.keyHandler);
    window.addEventListener("resize", this.resizeHandler);

    try {
      const doc = await api.openPdf(path);
      this.doc = doc;
      this.zoomMode = doc.zoomMode || "fit-width";
      if (this.zoomSel) this.zoomSel.value = this.zoomMode;
      titleEl.textContent = doc.title ?? path.split("/").pop() ?? "Document";
      authorEl.textContent = doc.author ?? "";
      this.buildOutline();
      if (doc.note || doc.pageCount === 0) {
        this.showMessage(
          doc.note ??
            "This document has no readable pages for the built-in reader. It will still open in your usual reader app.",
        );
        this.updateChrome();
        return;
      }
      await this.go(doc.startPage, true);
    } catch (err) {
      this.showMessage(`Couldn't open that document: ${errText(err)}`);
      this.updateChrome();
    }
  }

  private keyHandler = (e: KeyboardEvent): void => {
    if (!this.overlay) return;
    const tag = (e.target as HTMLElement)?.tagName;
    if (tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA") return;
    if (e.key === "Escape") this.close();
    else if (e.key === "ArrowLeft" || e.key === "PageUp") void this.go(this.page - 1);
    else if (e.key === "ArrowRight" || e.key === "PageDown") void this.go(this.page + 1);
    else if (e.key === "Home") void this.go(0);
    else if (e.key === "End") void this.go((this.doc?.pageCount ?? 1) - 1);
  };

  private resizeHandler = (): void => {
    if (!this.overlay || !this.doc) return;
    if (this.zoomMode !== "fit-page" && this.zoomMode !== "fit-width") return;
    window.clearTimeout(this.resizeTimer);
    this.resizeTimer = window.setTimeout(() => void this.go(this.page, true), 180);
  };

  private buildOutline(): void {
    if (!this.outlineSel || !this.doc) return;
    this.outlineSel.replaceChildren();
    for (const [i, entry] of this.doc.outline.entries()) {
      const o = document.createElement("option");
      o.value = String(i);
      o.textContent = `${" ".repeat(entry.depth)}${entry.title} · ${entry.page}`;
      this.outlineSel.appendChild(o);
    }
    this.outlineSel.style.display = this.doc.outline.length > 0 ? "" : "none";
  }

  private aspect(): number {
    const d = this.doc;
    if (d?.pageWidthPts && d?.pageHeightPts) return d.pageWidthPts / d.pageHeightPts;
    return 612 / 792;
  }

  /** The CSS width the current zoom mode asks for, and the pixel
   * width to request from the renderer (CSS × devicePixelRatio). */
  private widths(): { css: number; px: number } {
    const stage = this.stageEl;
    const stageW = Math.max(280, (stage?.clientWidth ?? 900) - 48);
    const stageH = Math.max(280, (stage?.clientHeight ?? 700) - 48);
    let css: number;
    if (this.zoomMode === "fit-page") {
      css = Math.min(stageW, stageH * this.aspect());
    } else if (this.zoomMode === "fit-width") {
      css = stageW;
    } else {
      const pct = Number(this.zoomMode) || 100;
      css = ((this.doc?.pageWidthPts ?? 612) / 72) * 96 * (pct / 100);
    }
    return { css, px: Math.ceil(css * (window.devicePixelRatio || 1)) };
  }

  private showMessage(message: string): void {
    if (this.imgEl) this.imgEl.style.display = "none";
    if (this.msgEl) {
      this.msgEl.textContent = message;
      this.msgEl.style.display = "";
    }
  }

  private async go(index: number, force = false): Promise<void> {
    if (!this.doc || this.doc.pageCount === 0) return;
    const target = Math.min(Math.max(index, 0), this.doc.pageCount - 1);
    if (!force && target === this.page && this.imgEl?.style.display !== "none") return;
    const seq = ++this.renderSeq;
    const { css, px } = this.widths();
    try {
      const view = await api.readPdfPage(this.path, target, px);
      if (seq !== this.renderSeq || !this.overlay) return;
      this.page = view.index;
      if (this.imgEl) {
        this.imgEl.src = view.image;
        this.imgEl.style.width = `${css}px`;
        this.imgEl.style.display = "";
      }
      if (this.msgEl) this.msgEl.style.display = "none";
      if (this.stageEl) this.stageEl.scrollTop = 0;
      this.updateChrome();
      this.saveProgress();
      // Warm the cache for the next page — reading stays lazy, the
      // turn of the page doesn't pay the render.
      if (this.page + 1 < this.doc.pageCount) {
        void api.readPdfPage(this.path, this.page + 1, px).catch(() => {});
      }
    } catch (err) {
      if (seq !== this.renderSeq) return;
      Toast.error(`Couldn't render that page: ${errText(err)}`);
    }
  }

  private saveProgress(): void {
    if (!this.path || !this.doc || this.doc.pageCount === 0) return;
    void api.setPdfProgress(this.path, this.page, this.zoomMode).catch(() => {});
  }

  private updateChrome(): void {
    const count = this.doc?.pageCount ?? 0;
    if (this.pageInput) {
      this.pageInput.value = count > 0 ? String(this.page + 1) : "";
      this.pageInput.max = String(Math.max(count, 1));
      this.pageInput.disabled = count === 0;
    }
    if (this.totalEl) this.totalEl.textContent = count > 0 ? ` of ${count}` : "";
    if (this.prevBtn) this.prevBtn.disabled = count === 0 || this.page <= 0;
    if (this.nextBtn) this.nextBtn.disabled = count === 0 || this.page >= count - 1;
    if (this.outlineSel && this.doc && this.doc.outline.length > 0) {
      // Show the section the current page falls in.
      let current = 0;
      for (const [i, entry] of this.doc.outline.entries()) {
        if (entry.page - 1 <= this.page) current = i;
      }
      this.outlineSel.value = String(current);
    }
  }

  close(): void {
    if (this.overlay) {
      this.renderSeq++;
      this.saveProgress();
      this.overlay.remove();
      this.overlay = null;
      document.removeEventListener("keydown", this.keyHandler);
      window.removeEventListener("resize", this.resizeHandler);
    }
    this.doc = null;
    this.imgEl = null;
    this.stageEl = null;
  }
}

const pdfReader = new PdfReader();

export function openPdfReader(path: string): void {
  void pdfReader.open(path);
}
