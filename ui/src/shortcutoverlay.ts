/**
 * shortcutoverlay.ts — the shortcuts overlay (Dickles, 2026-10-16:
 * "one of those super cool overlay things"). It renders the registry
 * from shortcuts.ts — there is no other list, so it cannot lie —
 * behind a glass card with a live filter and, pinned at its foot,
 * the export bar: Save as PDF · Save as JPG · Print.
 *
 * Doors (all toggle): "?" outside text fields, Mod+/, Mod+? (the
 * original door), the main-menu item, and the Preferences row.
 * Esc closes — full stop, even mid-filter. Focus is trapped
 * politely among the card's controls and returned on close.
 */

import {
  filterShortcuts,
  formatBinding,
  isMacPlatform,
} from "./shortcuts";
import {
  saveCheatSheet,
  stageSheetPdfForPrint,
  type SheetExportDeps,
} from "./cheatsheet";
import { reducedMotion, springTo, cancelSpring } from "./motion";
import { errText } from "./toast";

export class ShortcutOverlay {
  private deps: SheetExportDeps;
  private root: HTMLDivElement | null = null;
  private card: HTMLDivElement | null = null;
  private bodyEl: HTMLDivElement | null = null;
  private filterEl: HTMLInputElement | null = null;
  private printRowEl: HTMLDivElement | null = null;
  private lastFocus: HTMLElement | null = null;
  private keyHandler: ((e: KeyboardEvent) => void) | null = null;
  private busy = false;

  constructor(deps: SheetExportDeps) {
    this.deps = deps;
  }

  get isOpen(): boolean {
    return this.root !== null;
  }

  toggle(): void {
    if (this.isOpen) this.close();
    else this.open();
  }

  open(): void {
    if (this.isOpen) return;
    this.lastFocus = (document.activeElement as HTMLElement) ?? null;

    const root = document.createElement("div");
    root.className = "sc-overlay";
    root.addEventListener("mousedown", (e) => {
      if (e.target === root) this.close();
    });
    const card = document.createElement("div");
    card.className = "sc-card";
    card.setAttribute("role", "dialog");
    card.setAttribute("aria-modal", "true");
    card.setAttribute("aria-label", "Keyboard shortcuts");

    // Header: mark + title + filter + close.
    const head = document.createElement("div");
    head.className = "sc-head";
    const mark = document.createElement("div");
    mark.className = "sc-mark";
    mark.setAttribute("aria-hidden", "true");
    for (let i = 0; i < 5; i++) {
      const book = document.createElement("span");
      if (i === 2) book.className = "sc-book-out";
      mark.appendChild(book);
    }
    const shelf = document.createElement("i");
    mark.appendChild(shelf);
    const title = document.createElement("h2");
    title.className = "sc-title";
    title.textContent = "Keyboard Shortcuts";
    const filter = document.createElement("input");
    filter.className = "sc-filter";
    filter.type = "search";
    filter.placeholder = "Filter — try “trash”";
    filter.setAttribute("aria-label", "Filter shortcuts");
    filter.addEventListener("input", () => this.renderBody());
    const closeBtn = document.createElement("button");
    closeBtn.className = "icon-btn sc-close";
    closeBtn.textContent = "✕";
    closeBtn.title = "Close (Esc)";
    closeBtn.addEventListener("click", () => this.close());
    head.append(mark, title, filter, closeBtn);

    const body = document.createElement("div");
    body.className = "sc-body";

    // Footer: honesty line + printer row (hidden) + export bar.
    const foot = document.createElement("div");
    foot.className = "sc-foot";
    const note = document.createElement("span");
    note.className = "sc-note";
    note.textContent = "Drawn from the shortcuts this build actually runs.";
    const printRow = document.createElement("div");
    printRow.className = "sc-printrow";
    printRow.hidden = true;
    const exportsEl = document.createElement("div");
    exportsEl.className = "sc-exports";
    const mkBtn = (label: string, kind: "pdf" | "jpg" | "print") => {
      const b = document.createElement("button");
      b.className = "pill-btn sc-export-btn";
      b.textContent = label;
      b.addEventListener("click", () => void this.onExport(kind, b));
      return b;
    };
    exportsEl.append(
      mkBtn("Save as PDF", "pdf"),
      mkBtn("Save as JPG", "jpg"),
      mkBtn("Print", "print"),
    );
    foot.append(note, printRow, exportsEl);

    card.append(head, body, foot);
    root.appendChild(card);
    document.body.appendChild(root);
    this.root = root;
    this.card = card;
    this.bodyEl = body;
    this.filterEl = filter;
    this.printRowEl = printRow;
    this.renderBody();

    // Entrance: a spring, per house motion law; reduced motion
    // gets the calm fade instead.
    if (reducedMotion()) {
      card.classList.add("sc-in-fade");
    } else {
      springTo("sc-overlay-in", 1, (v) => {
        card.style.opacity = String(0.35 + 0.65 * v);
        card.style.transform = `translateY(${(1 - v) * 16}px) scale(${0.965 + 0.035 * v})`;
      }, { from: 0, stiffness: 300, damping: 27 });
    }

    this.keyHandler = (e: KeyboardEvent) => this.onKeyCapture(e);
    document.addEventListener("keydown", this.keyHandler, true);
    filter.focus();
  }

  close(): void {
    if (!this.root) return;
    cancelSpring("sc-overlay-in");
    if (this.keyHandler) {
      document.removeEventListener("keydown", this.keyHandler, true);
      this.keyHandler = null;
    }
    this.root.remove();
    this.root = null;
    this.card = null;
    this.bodyEl = null;
    this.filterEl = null;
    this.printRowEl = null;
    if (this.lastFocus) {
      this.lastFocus.focus();
      this.lastFocus = null;
    }
  }

  /** Capture-phase keys while open: Esc closes (full stop), the
   * modifier doors toggle shut, Tab stays inside the card. The
   * shell's own dispatch never sees these — stopPropagation. */
  private onKeyCapture(e: KeyboardEvent): void {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      this.close();
      return;
    }
    if ((e.ctrlKey || e.metaKey) && (e.key === "?" || e.key === "/")) {
      e.preventDefault();
      e.stopPropagation();
      this.close();
      return;
    }
    if (e.key === "Tab" && this.card) {
      const focusables = this.card.querySelectorAll<HTMLElement>(
        "button, input, [tabindex]",
      );
      const list = [...focusables].filter((el) => !el.hidden && el.offsetParent !== null || el === this.filterEl);
      if (list.length === 0) return;
      const first = list[0];
      const last = list[list.length - 1];
      const active = document.activeElement as HTMLElement;
      if (e.shiftKey && (active === first || !this.card.contains(active))) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && active === last) {
        e.preventDefault();
        first.focus();
      }
    }
  }

  private renderBody(): void {
    if (!this.bodyEl || !this.filterEl) return;
    const mac = isMacPlatform();
    const groups = filterShortcuts(this.filterEl.value, mac);
    const body = this.bodyEl;
    body.innerHTML = "";
    if (groups.length === 0) {
      const none = document.createElement("p");
      none.className = "sc-none";
      none.textContent = `No shortcuts match “${this.filterEl.value}”.`;
      body.appendChild(none);
      return;
    }
    for (const { group, rows } of groups) {
      const sec = document.createElement("section");
      sec.className = "sc-group";
      const h = document.createElement("h3");
      h.className = "sc-group-title";
      h.textContent = group.title;
      sec.appendChild(h);
      for (const b of rows) {
        const row = document.createElement("div");
        row.className = "sc-row";
        const lab = document.createElement("span");
        lab.className = "sc-row-label";
        lab.textContent = b.label;
        const ctx = document.createElement("span");
        ctx.className = "sc-row-ctx";
        ctx.textContent = b.surfaces.join(" · ");
        lab.appendChild(ctx);
        const keys = document.createElement("span");
        keys.className = "sc-row-keys";
        for (const part of formatBinding(b, mac).split(" · ")) {
          const k = document.createElement("kbd");
          k.className = "sc-kbd";
          k.textContent = part;
          keys.appendChild(k);
        }
        row.append(lab, keys);
        sec.appendChild(row);
        if (b.note) {
          const n = document.createElement("div");
          n.className = "sc-row-note";
          n.textContent = b.note;
          sec.appendChild(n);
        }
      }
      body.appendChild(sec);
    }
  }

  private async onExport(kind: "pdf" | "jpg" | "print", btn: HTMLButtonElement): Promise<void> {
    if (this.busy) return;
    this.busy = true;
    const label = btn.textContent;
    btn.textContent = kind === "print" ? "Preparing…" : "Saving…";
    try {
      if (kind === "print") {
        await this.startPrint();
      } else {
        const out = await saveCheatSheet(kind, isMacPlatform(), this.deps);
        if (out.cancelled) this.deps.toastError("Save cancelled — nothing written.");
      }
    } catch (err) {
      this.deps.toastError(`Cheat sheet failed: ${errText(err)}`);
    } finally {
      this.busy = false;
      btn.textContent = label;
    }
  }

  /** Print: stage the PDF, then offer the machine's printers
   * inline — the same CUPS flow Fable uses for files. */
  private async startPrint(): Promise<void> {
    const path = await stageSheetPdfForPrint(isMacPlatform(), this.deps);
    const printers = await this.deps.listPrinters();
    const row = this.printRowEl;
    if (!row) return;
    row.innerHTML = "";
    row.hidden = false;
    if (printers.length === 0) {
      const none = document.createElement("span");
      none.className = "sc-print-none";
      none.textContent =
        "No printers are set up on this computer yet — the sheet is staged, so Save as PDF and print it later.";
      row.appendChild(none);
      return;
    }
    const lead = document.createElement("span");
    lead.className = "sc-print-lead";
    lead.textContent = "Print to:";
    row.appendChild(lead);
    for (const p of printers) {
      const b = document.createElement("button");
      b.className = "pill-btn sc-printer-btn";
      b.textContent = p.name + (p.is_default ? " (default)" : "");
      b.addEventListener("click", async () => {
        b.textContent = "Sending…";
        try {
          const line = await this.deps.printFilePath(path, p.name, 1);
          this.deps.toastSuccess(line || `Sent to ${p.name}.`);
          row.hidden = true;
        } catch (err) {
          this.deps.toastError(`Print failed: ${errText(err)}`);
          b.textContent = p.name;
        }
      });
      row.appendChild(b);
    }
    const cancel = document.createElement("button");
    cancel.className = "pill-btn sc-printer-btn";
    cancel.textContent = "Cancel";
    cancel.addEventListener("click", () => {
      row.hidden = true;
    });
    row.appendChild(cancel);
  }
}
