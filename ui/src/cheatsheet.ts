/**
 * cheatsheet.ts — the exportable shortcut cheat sheet. ONE layout,
 * three outputs (Dickles, 2026-10-16: "an offer to save a shortcut
 * cheatsheet as a PDF, a JPG or to print it"):
 *
 *   layout (pure)  → a draw-list of pages, from the registry in
 *                    shortcuts.ts — the sheet can no more drift than
 *                    the overlay can.
 *   JPG            → a canvas rasterizer executes the draw-list
 *                    (pages stacked into one tall image).
 *   PDF            → a minimal PDF 1.4 writer embeds those same
 *                    JPEG pages (DCTDecode). Real, openable PDF;
 *                    the page is a designed image, so its text is
 *                    not selectable — the honest trade for one
 *                    layout that looks identical in all three
 *                    outputs (no PDF text writer exists anywhere
 *                    in the workspace's dependency tree).
 *   Print          → the PDF is staged to the OS temp dir and sent
 *                    through Fable's CUPS flow (print_file_path,
 *                    mirroring print_item's proven `lp` call).
 *
 * The draw-list is the contract: the harness renders it with PIL
 * to proof legibility, and both renderers consume identical ops.
 */

import {
  SHORTCUTS,
  SHORTCUT_GROUPS,
  formatBinding,
  type ShortcutBinding,
} from "./shortcuts";

/** Same version string the About dialog shows (tauri.conf.json). */
export const SHEET_VERSION = "0.1.0";

// ------------------------------------------------------------ draw-list

export type DrawOp =
  | { t: "rect"; x: number; y: number; w: number; h: number; fill: string }
  | { t: "rrect"; x: number; y: number; w: number; h: number; r: number; fill: string; stroke?: string }
  | { t: "line"; x1: number; y1: number; x2: number; y2: number; color: string; width: number }
  | { t: "text"; x: number; y: number; text: string; size: number; weight: number; color: string; align: "left" | "center" | "right" }
  | { t: "mark"; x: number; y: number; w: number; h: number };

export interface SheetPage {
  width: number;
  height: number;
  ops: DrawOp[];
}

export const PAGE_W = 1240;
export const PAGE_H = 1754;

const INK = "#211d19";
const SUB = "#6f695f";
const FAINT = "#a39a8b";
const AMBER = "#d97706";
const AMBER_DARK = "#96650c";
const PAPER = "#fbf9f4";
const CHIP_BG = "#efe9dd";
const CHIP_LINE = "#d9d0bd";
const HEAD_LINE = "#e3dccb";

const MARGIN = 72;
const GUTTER = 56;
const COL_W = (PAGE_W - MARGIN * 2 - GUTTER) / 2;
const CONTENT_TOP = 252;
const CONTENT_BOTTOM = PAGE_H - 118;

/** Deterministic text-width estimate (DejaVu-led stack, ~0.52em
 * average advance). Used for chip sizing and label truncation —
 * deliberately generous so chips never clip in either renderer. */
export function estTextWidth(text: string, size: number, weight: number): number {
  const f = weight >= 700 ? 0.56 : weight >= 600 ? 0.54 : 0.52;
  return text.length * size * f;
}

interface FlowItem {
  kind: "head" | "row";
  height: number;
  groupTitle?: string;
  binding?: ShortcutBinding;
  plan?: RowPlan;
}

export interface RowPlan {
  /** Label wrapped to at most two lines — never truncated. */
  lines: string[];
  chords: string;
  height: number;
}

/** Plan one sheet row: the label wraps beside its chord chip
 * instead of being cut. Both the flow (heights) and the drawing
 * use this, so they can never disagree. */
export function planRow(b: ShortcutBinding, mac: boolean): RowPlan {
  const chords = formatBinding(b, mac);
  const chipW = Math.min(estTextWidth(chords, 22, 600) + 30, COL_W * 0.52);
  const maxLabelW = COL_W - chipW - 18;
  const words = b.label.split(" ");
  const lines: string[] = [];
  let cur = "";
  for (const w of words) {
    const next = cur ? `${cur} ${w}` : w;
    if (estTextWidth(next, 24, 400) <= maxLabelW || !cur) cur = next;
    else {
      lines.push(cur);
      cur = w;
    }
  }
  if (cur) lines.push(cur);
  if (lines.length > 2) {
    const first = lines[0];
    let second = lines.slice(1).join(" ");
    while (second.length > 4 && estTextWidth(second + "…", 24, 400) > maxLabelW) {
      second = second.slice(0, -2);
    }
    lines.length = 0;
    lines.push(first, second.trimEnd() + "…");
  }
  const height = 44 + (lines.length - 1) * 26 + (b.note ? 24 : 0);
  return { lines, chords, height };
}

function flowItems(mac: boolean): FlowItem[] {
  const items: FlowItem[] = [];
  for (const g of SHORTCUT_GROUPS) {
    const rows = SHORTCUTS.filter((b) => b.group === g.id);
    if (!rows.length) continue;
    items.push({ kind: "head", height: 54, groupTitle: g.title });
    for (const b of rows) {
      const plan = planRow(b, mac);
      items.push({ kind: "row", height: plan.height, binding: b, plan });
    }
  }
  return items;
}

/** Lay the whole registry out on A4 pages. Pure — no DOM. */
export function layoutCheatSheet(opts: { mac: boolean; date: string }): SheetPage[] {
  const items = flowItems(opts.mac);
  const pages: SheetPage[] = [];
  let ops: DrawOp[] = [];
  let col = 0;
  let y = CONTENT_TOP;

  const colX = (c: number) => MARGIN + c * (COL_W + GUTTER);

  const startPage = () => {
    ops = [];
    ops.push({ t: "rect", x: 0, y: 0, w: PAGE_W, h: PAGE_H, fill: PAPER });
    // Header: the Fable mark — one book out on a tidy shelf.
    ops.push({ t: "mark", x: MARGIN, y: 58, w: 132, h: 104 });
    ops.push({ t: "text", x: MARGIN + 164, y: 106, text: "Keyboard Shortcuts", size: 54, weight: 700, color: INK, align: "left" });
    const subtitle = pages.length === 0
      ? "Fable Files — every shortcut, one sheet"
      : "Fable Files — every shortcut, one sheet · continued";
    ops.push({ t: "text", x: MARGIN + 166, y: 148, text: subtitle, size: 25, weight: 400, color: SUB, align: "left" });
    ops.push({ t: "line", x1: MARGIN, y1: 196, x2: PAGE_W - MARGIN, y2: 196, color: AMBER, width: 4 });
    col = 0;
    y = CONTENT_TOP;
  };

  const footer = (pageNo: number) => {
    ops.push({ t: "line", x1: MARGIN, y1: PAGE_H - 96, x2: PAGE_W - MARGIN, y2: PAGE_H - 96, color: HEAD_LINE, width: 2 });
    ops.push({
      t: "text", x: PAGE_W / 2, y: PAGE_H - 56,
      text: `Fable Files ${SHEET_VERSION} · Generated ${opts.date} · Read from the app's live shortcut registry.`,
      size: 19, weight: 400, color: FAINT, align: "center",
    });
    // Page-number op carries a placeholder total, patched once
    // pagination is known (and dropped entirely for a single page).
    ops.push({ t: "text", x: PAGE_W - MARGIN, y: PAGE_H - 56, text: `${pageNo} / @TOTAL@`, size: 19, weight: 400, color: FAINT, align: "right" });
  };

  let pageBroke = false;
  const advanceColumn = () => {
    if (col === 0) {
      col = 1;
      y = CONTENT_TOP;
    } else {
      footer(pages.length + 1);
      pages.push({ width: PAGE_W, height: PAGE_H, ops });
      startPage();
      pageBroke = true;
    }
  };

  const drawHead = (title: string, x: number, yPos: number) => {
    ops.push({ t: "text", x, y: yPos + 32, text: title, size: 29, weight: 700, color: AMBER_DARK, align: "left" });
    ops.push({ t: "line", x1: x, y1: yPos + 44, x2: x + COL_W, y2: yPos + 44, color: HEAD_LINE, width: 2 });
  };

  let lastGroupTitle: string | null = null;
  startPage();
  for (let i = 0; i < items.length; i++) {
    const it = items[i];
    // A group head never strands at a column foot: it needs its
    // first row beneath it.
    const need = it.kind === "head" ? it.height + (items[i + 1]?.height ?? 0) : it.height;
    if (y + need > CONTENT_BOTTOM) advanceColumn();
    const x = colX(col);
    if (pageBroke) {
      pageBroke = false;
      // A group split across pages repeats its head on the new
      // page, marked (continued) — a spilled row never floats
      // without its group's name.
      if (it.kind === "row" && lastGroupTitle) {
        drawHead(`${lastGroupTitle} (continued)`, x, y);
        y += 54;
      }
    }
    if (it.kind === "head") {
      lastGroupTitle = it.groupTitle!;
      drawHead(it.groupTitle!, x, y);
    } else {
      const b = it.binding!;
      const plan = it.plan!;
      const chipW = Math.min(estTextWidth(plan.chords, 22, 600) + 30, COL_W * 0.52);
      const chipX = x + COL_W - chipW;
      plan.lines.forEach((line, li) => {
        ops.push({ t: "text", x, y: y + 28 + li * 26, text: line, size: 24, weight: 400, color: INK, align: "left" });
      });
      const chipY = y + 4 + (plan.lines.length - 1) * 13;
      ops.push({ t: "rrect", x: chipX, y: chipY, w: chipW, h: 36, r: 9, fill: CHIP_BG, stroke: CHIP_LINE });
      ops.push({ t: "text", x: chipX + chipW / 2, y: chipY + 25, text: plan.chords, size: 22, weight: 600, color: INK, align: "center" });
      if (b.note) {
        ops.push({ t: "text", x, y: y + 30 + plan.lines.length * 26, text: b.note, size: 18, weight: 400, color: SUB, align: "left" });
      }
    }
    y += it.height;
  }
  footer(pages.length + 1);
  pages.push({ width: PAGE_W, height: PAGE_H, ops });

  // Patch page totals now that pagination is known; a single-page
  // sheet drops the page-number op entirely.
  for (const p of pages) {
    p.ops = p.ops.filter((op) => !(op.t === "text" && op.text.includes("@TOTAL@")) || pages.length > 1);
    for (const op of p.ops) {
      if (op.t === "text" && op.text.includes("@TOTAL@")) {
        op.text = op.text.replace("@TOTAL@", String(pages.length));
      }
    }
  }
  return pages;
}

// ------------------------------------------------------------------ PDF

const te = new TextEncoder();

function concatBytes(parts: Uint8Array[]): Uint8Array {
  const total = parts.reduce((n, p) => n + p.length, 0);
  const out = new Uint8Array(total);
  let off = 0;
  for (const p of parts) {
    out.set(p, off);
    off += p.length;
  }
  return out;
}

/**
 * Minimal, correct PDF 1.4: one full-bleed image XObject per page
 * (the page's JPEG, DCTDecode), A4 MediaBox. Byte-exact xref.
 * Pure — no DOM.
 */
export function buildPdf(jpegs: Array<{ bytes: Uint8Array; w: number; h: number }>): Uint8Array {
  const A4_W = 595.28;
  const A4_H = 841.89;
  const objects: Uint8Array[] = [];
  const push = (bytes: Uint8Array) => objects.push(bytes);
  const str = (s: string) => te.encode(s);

  // 1: Catalog, 2: Pages, then per page: Page, Content, Image.
  const pageObjNums = jpegs.map((_, i) => 3 + i * 3);
  push(str("<< /Type /Catalog /Pages 2 0 R >>"));
  push(str(`<< /Type /Pages /Kids [${pageObjNums.map((n) => `${n} 0 R`).join(" ")}] /Count ${jpegs.length} >>`));
  jpegs.forEach((j, i) => {
    const pageNum = 3 + i * 3;
    const contentNum = pageNum + 1;
    const imageNum = pageNum + 2;
    push(str(
      `<< /Type /Page /Parent 2 0 R /MediaBox [0 0 ${A4_W} ${A4_H}] ` +
      `/Resources << /XObject << /Im${i} ${imageNum} 0 R >> >> /Contents ${contentNum} 0 R >>`,
    ));
    const content = `q ${A4_W} 0 0 ${A4_H} 0 0 cm /Im${i} Do Q`;
    push(concatBytes([str(`<< /Length ${content.length} >>\nstream\n`), str(content), str("\nendstream")]));
    push(concatBytes([
      str(`<< /Type /XObject /Subtype /Image /Width ${j.w} /Height ${j.h} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length ${j.bytes.length} >>\nstream\n`),
      j.bytes,
      str("\nendstream"),
    ]));
  });

  const chunks: Uint8Array[] = [str("%PDF-1.4\n%\xE2\xE3\xCF\xD3\n")];
  const offsets: number[] = [0];
  let len = chunks[0].length;
  objects.forEach((body, i) => {
    offsets.push(len);
    const head = str(`${i + 1} 0 obj\n`);
    const tail = str("\nendobj\n");
    chunks.push(head, body, tail);
    len += head.length + body.length + tail.length;
  });
  const xrefAt = len;
  let xref = `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n`;
  for (let i = 1; i <= objects.length; i++) {
    xref += `${String(offsets[i]).padStart(10, "0")} 00000 n \n`;
  }
  xref += `trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${xrefAt}\n%%EOF\n`;
  chunks.push(str(xref));
  return concatBytes(chunks);
}

// ------------------------------------------------------------- base64

export function bytesToBase64(bytes: Uint8Array): string {
  let bin = "";
  const CHUNK = 0x8000;
  for (let i = 0; i < bytes.length; i += CHUNK) {
    bin += String.fromCharCode(...bytes.subarray(i, i + CHUNK));
  }
  return btoa(bin);
}

// ------------------------------------------- canvas rasterizer (browser)

const FONT_STACK = `"DejaVu Sans","Liberation Sans","Noto Sans",system-ui,sans-serif`;

function drawMark(ctx: CanvasRenderingContext2D, x: number, y: number, w: number, h: number): void {
  // Five books on a shelf line; exactly one — amber — pulled out
  // and leaning. The approved Fable mark, in canvas ops.
  const shelfY = y + h - 6;
  ctx.fillStyle = INK;
  ctx.beginPath();
  ctx.roundRect(x, shelfY, w, 6, 3);
  ctx.fill();
  const bw = w * 0.155;
  const gap = w * 0.028;
  const baseX = x + w * 0.045;
  const heights = [0.8, 0.92, 0.86, 0.9, 0.78];
  heights.forEach((hf, i) => {
    const bx = baseX + i * (bw + gap);
    const bh = (h - 12) * hf;
    if (i === 2) {
      ctx.save();
      ctx.translate(bx + bw / 2, shelfY);
      ctx.rotate((-10 * Math.PI) / 180);
      ctx.fillStyle = AMBER;
      ctx.beginPath();
      ctx.roundRect(-bw / 2, -bh - h * 0.1, bw, bh, 3);
      ctx.fill();
      ctx.restore();
    } else {
      ctx.fillStyle = "#45403a";
      ctx.beginPath();
      ctx.roundRect(bx, shelfY - bh, bw, bh, 3);
      ctx.fill();
    }
  });
}

function execOps(ctx: CanvasRenderingContext2D, ops: DrawOp[]): void {
  for (const op of ops) {
    switch (op.t) {
      case "rect":
        ctx.fillStyle = op.fill;
        ctx.fillRect(op.x, op.y, op.w, op.h);
        break;
      case "rrect":
        ctx.beginPath();
        ctx.roundRect(op.x, op.y, op.w, op.h, op.r);
        ctx.fillStyle = op.fill;
        ctx.fill();
        if (op.stroke) {
          ctx.strokeStyle = op.stroke;
          ctx.lineWidth = 1.5;
          ctx.stroke();
        }
        break;
      case "line":
        ctx.beginPath();
        ctx.moveTo(op.x1, op.y1);
        ctx.lineTo(op.x2, op.y2);
        ctx.strokeStyle = op.color;
        ctx.lineWidth = op.width;
        ctx.stroke();
        break;
      case "text":
        ctx.font = `${op.weight} ${op.size}px ${FONT_STACK}`;
        ctx.fillStyle = op.color;
        ctx.textAlign = op.align;
        ctx.textBaseline = "alphabetic";
        ctx.fillText(op.text, op.x, op.y);
        break;
      case "mark":
        drawMark(ctx, op.x, op.y, op.w, op.h);
        break;
    }
  }
}

async function pageToJpeg(page: SheetPage, scale: number): Promise<{ bytes: Uint8Array; w: number; h: number }> {
  const canvas = document.createElement("canvas");
  canvas.width = page.width * scale;
  canvas.height = page.height * scale;
  const ctx = canvas.getContext("2d")!;
  ctx.scale(scale, scale);
  execOps(ctx, page.ops);
  const blob = await new Promise<Blob | null>((res) => canvas.toBlob(res, "image/jpeg", 0.92));
  if (!blob) throw new Error("the cheat-sheet image could not be rendered");
  return { bytes: new Uint8Array(await blob.arrayBuffer()), w: canvas.width, h: canvas.height };
}

/** All pages stacked into ONE tall JPEG (the shareable form). */
async function pagesToStackedJpeg(pages: SheetPage[], scale: number): Promise<{ bytes: Uint8Array; w: number; h: number }> {
  const canvas = document.createElement("canvas");
  canvas.width = PAGE_W * scale;
  canvas.height = PAGE_H * pages.length * scale;
  const ctx = canvas.getContext("2d")!;
  pages.forEach((page, i) => {
    ctx.save();
    ctx.translate(0, i * PAGE_H * scale);
    ctx.scale(scale, scale);
    execOps(ctx, page.ops);
    ctx.restore();
  });
  const blob = await new Promise<Blob | null>((res) => canvas.toBlob(res, "image/jpeg", 0.92));
  if (!blob) throw new Error("the cheat-sheet image could not be rendered");
  return { bytes: new Uint8Array(await blob.arrayBuffer()), w: canvas.width, h: canvas.height };
}

// ------------------------------------------------------------ export flow

export interface PrinterLike {
  name: string;
  is_default: boolean;
  state: string;
}

export interface SheetExportDeps {
  writeBinaryFile(path: string, base64: string): Promise<number>;
  printFilePath(path: string, printer: string | null, copies: number): Promise<string>;
  listPrinters(): Promise<PrinterLike[]>;
  /** Native save dialog. Resolves null = user cancelled.
   * Rejects when the dialog is unavailable → fallback save. */
  saveDialog?(defaultName: string, kind: "pdf" | "jpg"): Promise<string | null>;
  /** Directory for the fallback save (the user's Downloads). */
  fallbackDir(): Promise<string>;
  /** OS temp dir, for staging the print PDF. */
  tempDir(): Promise<string>;
  toastSuccess(msg: string): void;
  toastError(msg: string): void;
}

export interface SaveOutcome {
  saved: boolean;
  cancelled?: boolean;
  path?: string;
  fallback?: boolean;
}

function todayLabel(): string {
  return new Date().toLocaleDateString("en-US", { year: "numeric", month: "long", day: "numeric" });
}

export function sheetPages(mac: boolean): SheetPage[] {
  return layoutCheatSheet({ mac, date: todayLabel() });
}

async function renderPdfBytes(mac: boolean): Promise<Uint8Array> {
  const pages = sheetPages(mac);
  const jpegs = [];
  for (const p of pages) jpegs.push(await pageToJpeg(p, 2));
  return buildPdf(jpegs);
}

export async function saveCheatSheet(kind: "pdf" | "jpg", mac: boolean, deps: SheetExportDeps): Promise<SaveOutcome> {
  const bytes = kind === "pdf" ? await renderPdfBytes(mac) : (await pagesToStackedJpeg(sheetPages(mac), 2)).bytes;
  const defaultName = `fable-shortcuts.${kind}`;
  let path: string | null = null;
  let fallback = false;
  if (deps.saveDialog) {
    try {
      path = await deps.saveDialog(defaultName, kind);
      if (path === null) return { saved: false, cancelled: true };
    } catch {
      path = null; // dialog unavailable — fall through to the fallback
    }
  }
  if (!path) {
    path = `${await deps.fallbackDir()}/${defaultName}`;
    fallback = true;
  }
  await deps.writeBinaryFile(path, bytesToBase64(bytes));
  deps.toastSuccess(
    fallback
      ? `Cheat sheet saved to ${path}`
      : `Cheat sheet saved — ${path}`,
  );
  return { saved: true, path, fallback };
}

/** Render + stage the sheet PDF for printing; returns its path. */
export async function stageSheetPdfForPrint(mac: boolean, deps: SheetExportDeps): Promise<string> {
  const bytes = await renderPdfBytes(mac);
  const path = `${await deps.tempDir()}/fable-shortcuts-sheet.pdf`;
  await deps.writeBinaryFile(path, bytesToBase64(bytes));
  return path;
}
