/**
 * Quick Look: Space (or double-click on a file) opens a preview pane anchored
 * to the right of the selected file. The pane slides in from the right and
 * dismisses back to the right — enter and exit along the same path.
 *
 * Interruptibility: the pane's X offset is driven by a single spring keyed
 * "quicklook-x". Re-opening mid-close simply re-targets the live spring, so
 * the pane reverses from its on-screen position — it never finishes closing
 * first. Esc or Space again dismisses.
 *
 * MVP content: name, kind label (Native/Media/Document/Archive/Metadata/Hex),
 * size, modified, fetched via the `preview` command. A failed preview still
 * shows the metadata card (backend stub returns not_implemented today).
 */
import {
  api,
  formatBytes,
  formatDate,
  PREVIEW_LABELS,
  type Item,
  type PreviewKind,
} from "./api";
import { fadeTo, reducedMotion, springTo } from "./motion";

const FALLBACK_PANEL_WIDTH = 340;

export class QuickLook {
  readonly el: HTMLElement;
  private openItem: Item | null = null;
  private requestToken = 0;

  constructor(private onDismiss: () => void) {
    this.el = document.createElement("aside");
    this.el.className = "quicklook";
    this.el.setAttribute("aria-label", "Preview");
    this.el.hidden = true;
    this.el.innerHTML = `
      <div class="ql-head">
        <span class="ql-title">Preview</span>
        <button class="ql-close icon-btn" aria-label="Close preview">✕</button>
      </div>
      <div class="ql-body"></div>`;
    this.el
      .querySelector(".ql-close")!
      .addEventListener("click", () => this.close());
  }

  get isOpen(): boolean {
    return this.openItem !== null;
  }

  get item(): Item | null {
    return this.openItem;
  }

  async open(item: Item): Promise<void> {
    const token = ++this.requestToken;
    this.openItem = item;
    this.el.hidden = false;
    this.renderCard(item, null);
    // Slide in from the right — from the LIVE offset (interruptible).
    this.slideTo(0);
    try {
      const kind = await api.preview(item.id);
      if (token === this.requestToken && this.openItem === item) {
        this.renderCard(item, kind);
      }
    } catch {
      if (token === this.requestToken && this.openItem === item) {
        this.renderCard(item, null); // metadata card survives backend gaps
      }
    }
  }

  close(): void {
    if (!this.openItem) return;
    this.requestToken++;
    this.openItem = null;
    this.slideTo(this.closedX(), () => {
      // A re-open mid-close re-targets the same spring; its stale onDone
      // must not hide a pane that is open again.
      if (this.openItem) return;
      this.el.hidden = true;
      this.el.style.transform = "";
    });
    this.onDismiss();
  }

  toggle(item: Item | null): void {
    if (this.isOpen && (!item || item.id === this.openItem?.id)) {
      this.close();
    } else if (item) {
      void this.open(item);
    }
  }

  private slideTo(x: number, onDone?: () => void): void {
    if (reducedMotion()) {
      // Reduced motion: 120ms opacity fade, no travel.
      if (x === 0) {
        this.el.style.opacity = "0";
        fadeTo(this.el, 1, onDone);
      } else {
        fadeTo(this.el, 0, onDone);
      }
      return;
    }
    const from = this.liveX();
    springTo(
      "quicklook-x",
      x,
      (v) => {
        this.el.style.transform = `translateX(${v}px)`;
      },
      { from, onDone },
    );
  }

  /** Off-screen X: the pane's live width, so CSS and motion never drift. */
  private closedX(): number {
    return this.el.offsetWidth > 0 ? this.el.offsetWidth : FALLBACK_PANEL_WIDTH;
  }

  private liveX(): number {
    const t = this.el.style.transform;
    const m = t.match(/translateX\((-?[\d.]+)px\)/);
    return m ? parseFloat(m[1]) : this.closedX();
  }

  private async renderCard(item: Item, kind: PreviewKind | null): Promise<void> {
    const body = this.el.querySelector(".ql-body")!;
    body.innerHTML = "";
    const card = document.createElement("div");
    card.className = "ql-card";

    const badge = document.createElement("span");
    badge.className = "ql-kind";
    badge.textContent = kind ? PREVIEW_LABELS[kind] : "Metadata";
    card.appendChild(badge);

    const name = document.createElement("h2");
    name.className = "ql-name";
    name.textContent = item.name;
    card.appendChild(name);

    const rows: Array<[string, string]> = [
      ["Kind", item.kind === "folder" ? "Folder" : "File"],
      ["Size", item.kind === "folder" ? "—" : formatBytes(item.size_bytes)],
      ["Modified", formatDate(item.modified)],
    ];
    if (item.provider_key) rows.push(["Path", item.provider_key]);
    if (item.starred) rows.push(["Starred", "Yes"]);
    if (item.labels.length > 0) rows.push(["Labels", item.labels.join(", ")]);

    const dl = document.createElement("dl");
    dl.className = "ql-meta";
    for (const [k, v] of rows) {
      const dt = document.createElement("dt");
      dt.textContent = k;
      const dd = document.createElement("dd");
      dd.textContent = v;
      dl.appendChild(dt);
      dl.appendChild(dd);
    }
    card.appendChild(dl);

    const hint = document.createElement("p");
    hint.className = "ql-hint";
    hint.textContent = kind
      ? `Preview kind: ${PREVIEW_LABELS[kind]}. Rich renderers arrive with the preview milestone.`
      : "Preview service unavailable — showing catalog metadata.";
    card.appendChild(hint);

    body.appendChild(card);

    // Fetch text content for text-like files
    if (kind === "Native" && item.kind !== "folder") {
      const ext = item.name.split(".").pop()?.toLowerCase() ?? "";
      const textExts = ["txt", "md", "csv", "tsv", "log", "srt", "vtt", "sub", "ssa", "ass", "nfo", "json", "xml", "yaml", "yml", "toml", "ini", "cfg", "sh", "py", "rs", "ts", "js"];
      if (textExts.includes(ext)) {
        try {
          const result = await api.previewText(item.id, 50);
          const pre = document.createElement("pre");
          pre.className = "ql-text-preview";
          pre.textContent = result.lines.join("\n") + (result.truncated ? "\n… (truncated)" : "");
          card.appendChild(pre);
        } catch {
          // Text preview failed — metadata card stands alone
        }
      }
    }
  }
}
