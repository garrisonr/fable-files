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
import { onPlayerClosed } from "./api";
import { VideoPlayer, isVideoName } from "./video";
import { fadeTo, reducedMotion, springTo } from "./motion";

const FALLBACK_PANEL_WIDTH = 340;

export class QuickLook {
  readonly el: HTMLElement;
  private openItem: Item | null = null;
  private requestToken = 0;
  /** Path whose video is currently playing in a pop-out window. */
  private detachedFor: string | null = null;
  private currentKind: PreviewKind | null = null;

  /** Set by the shell: cast the previewed media item. */
  onCast: ((item: Item) => void) | null = null;
  private castBtn: HTMLButtonElement | null = null;

  constructor(private onDismiss: () => void) {
    this.el = document.createElement("aside");
    this.el.className = "quicklook";
    this.el.setAttribute("aria-label", "Preview");
    this.el.hidden = true;
    this.el.innerHTML = `
      <div class="ql-head">
        <span class="ql-title">Preview</span>
        <button class="ql-cast icon-btn" aria-label="Cast to device" title="Cast to…" hidden>📡</button>
        <button class="ql-close icon-btn" aria-label="Close preview">✕</button>
      </div>
      <div class="ql-body"></div>`;
    this.castBtn = this.el.querySelector(".ql-cast");
    this.castBtn?.addEventListener("click", () => {
      if (this.openItem && this.onCast) this.onCast(this.openItem);
    });
    this.el
      .querySelector(".ql-close")!
      .addEventListener("click", () => this.close());
    // When a pop-out player window closes, the video comes home.
    void onPlayerClosed((path) => {
      if (this.detachedFor === path) {
        this.detachedFor = null;
        if (this.openItem) void this.renderCard(this.openItem, this.currentKind);
      }
    }).catch(() => undefined);
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
    if (this.castBtn) {
      const ext = item.name.split(".").pop()?.toLowerCase() ?? "";
      const AUDIO = ["mp3", "flac", "ogg", "oga", "wav", "m4a", "opus", "aac", "aif", "aiff"];
      this.castBtn.hidden = !(
        this.onCast &&
        (AUDIO.includes(ext) ||
          ["mp4", "mkv", "webm", "mov", "avi", "m4v", "mpg", "mpeg"].includes(ext))
      );
    }
    this.el.hidden = false;
    this.renderCard(item, null);
    // Slide in from the right — from the LIVE offset (interruptible).
    this.slideTo(0);
    try {
      const kind = await api.preview(item.id);
      if (token === this.requestToken && this.openItem === item) {
        this.currentKind = kind;
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

    body.appendChild(card);

    // Rich content preview. Driven by extension (known synchronously from
    // the item name) rather than the async preview-kind round-trip, with
    // the item's provider path as a fallback for id resolution.
    if (item.kind !== "folder") {
      const ext = item.name.split(".").pop()?.toLowerCase() ?? "";
      const textExts = ["txt", "md", "csv", "tsv", "log", "srt", "vtt", "sub", "ssa", "ass", "nfo", "json", "xml", "yaml", "yml", "toml", "ini", "cfg", "sh", "py", "rs", "ts", "js"];
      if (kind === "native" && textExts.includes(ext)) {
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
      // Image preview — the photo IS the preview: hero at the top of
      // the card, as large as the pane allows (full image, not a thumb).
      // Video: the full player lives at the top of the card —
      // transport controls on hover, pop-out, fullscreen.
      if (isVideoName(item.name) && item.provider_key) {
        const vpath = item.provider_key;
        if (this.detachedFor === vpath) {
          const holder = document.createElement("div");
          holder.className = "ql-detached";
          const p = document.createElement("p");
          p.textContent = "🎬 Playing in the pop-out window";
          const btn = document.createElement("button");
          btn.textContent = "Bring it back here";
          btn.addEventListener("click", () => {
            void api.closePlayerWindow(vpath);
          });
          holder.append(p, btn);
          card.insertBefore(holder, dl);
        } else {
          const player = new VideoPlayer({
            path: vpath,
            onDetach: () => {
              void api
                .openPlayerWindow(vpath, item.name)
                .then(() => {
                  this.detachedFor = vpath;
                  if (this.openItem === item) {
                    void this.renderCard(item, this.currentKind);
                  }
                })
                .catch(() => undefined);
            },
          });
          card.insertBefore(player.el, dl);
        }
      }
      const imgExts = ["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "tif", "tiff", "ico"];
      if (imgExts.includes(ext)) {
        try {
          const dataUrl = await api.getImagePreview(item.id, item.provider_key);
          const img = document.createElement("img");
          img.className = "ql-image-preview";
          img.src = dataUrl;
          img.alt = item.name;
          card.insertBefore(img, card.firstChild);
          card.classList.add("has-photo");
        } catch (err) {
          const note = document.createElement("p");
          note.className = "ql-preview-error";
          note.textContent = `Image preview unavailable: ${err}`;
          card.appendChild(note);
        }
      }
    }
  }
}
