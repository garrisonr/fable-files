/**
 * TorrentDialog — right-click → Create Torrent…
 * Any file or folder. Asks the two questions Dickles specified:
 * run the Washing Machine first? And for images, the extra
 * nuclear step — a pixel re-encode with absolutely everything
 * stripped? The torrent is built from a staged washed/clean copy;
 * originals are never modified.
 */
import { api, formatBytes } from "./api";
import type { Item } from "./api";
import { Toast, errText} from "./toast";

const DEFAULT_TRACKERS = [
  "udp://tracker.opentrackr.org:1337/announce",
  "udp://tracker.openbittorrent.com:6969/announce",
  "udp://exodus.desync.com:6969/announce",
].join("\n");

const IMAGE_EXTS = [
  "jpg", "jpeg", "png", "gif", "webp", "bmp", "ico", "tif", "tiff",
  "avif", "heic",
];

export class TorrentDialog {
  readonly element: HTMLDivElement;
  private body: HTMLDivElement;
  private item: Item | null = null;
  onCreated: (() => void) | null = null;

  constructor() {
    this.element = document.createElement("div");
    this.element.className = "props-overlay";
    this.element.hidden = true;
    const dialog = document.createElement("div");
    dialog.className = "torrent-dialog";
    dialog.setAttribute("role", "dialog");
    dialog.setAttribute("aria-label", "Create Torrent");
    const title = document.createElement("h2");
    title.textContent = "🧲 Create Torrent";
    this.body = document.createElement("div");
    this.body.className = "torrent-body";
    dialog.append(title, this.body);
    this.element.appendChild(dialog);
    this.element.addEventListener("mousedown", (e) => {
      if (e.target === this.element) this.close();
    });
  }

  async open(item: Item): Promise<void> {
    this.item = item;
    this.element.hidden = false;
    this.body.textContent = "Checking the toolbox…";
    let hasTool = false;
    try {
      hasTool = await api.mktorrentStatus();
    } catch {
      hasTool = false;
    }
    if (!this.item) return;
    if (!hasTool) {
      this.renderInstall();
      return;
    }
    this.renderForm();
  }

  close(): void {
    this.element.hidden = true;
    this.item = null;
  }

  private renderInstall(): void {
    this.body.replaceChildren();
    const note = document.createElement("div");
    note.className = "torrent-note";
    note.textContent =
      "Torrents are made with mktorrent, and it isn't installed yet. " +
      "One click installs it (you'll get the usual password prompt) — no silent degraded modes here.";
    const row = document.createElement("div");
    row.className = "torrent-row";
    const install = document.createElement("button");
    install.className = "pill-btn primary";
    install.textContent = "Install mktorrent";
    install.addEventListener("click", async () => {
      install.disabled = true;
      install.textContent = "Installing…";
      try {
        await api.installMktorrent();
        Toast.success("mktorrent installed.");
        this.renderForm();
      } catch (err) {
        Toast.error(`Install failed: ${errText(err)}`);
        install.disabled = false;
        install.textContent = "Install mktorrent";
      }
    });
    const cancel = document.createElement("button");
    cancel.className = "pill-btn";
    cancel.textContent = "Cancel";
    cancel.addEventListener("click", () => this.close());
    row.append(install, cancel);
    this.body.append(note, row);
  }

  private renderForm(): void {
    const item = this.item;
    if (!item) return;
    this.body.replaceChildren();
    const ext = item.name.includes(".")
      ? item.name.split(".").pop()!.toLowerCase()
      : "";
    const isImage = IMAGE_EXTS.includes(ext);
    const isFolder = item.kind === "folder";

    const head = document.createElement("div");
    head.className = "torrent-head";
    head.textContent =
      `${isFolder ? "📁" : "📄"} ${item.name}` +
      (item.size_bytes ? ` · ${formatBytes(item.size_bytes)}` : "");
    this.body.appendChild(head);

    // The Washing Machine question.
    const washLabel = document.createElement("label");
    washLabel.className = "torrent-check";
    const washBox = document.createElement("input");
    washBox.type = "checkbox";
    washBox.checked = true;
    const washText = document.createElement("span");
    washText.textContent =
      "🧼 Run the Washing Machine first — strip identifiable metadata " +
      "(the torrent is built from a washed copy; your original stays untouched)";
    washLabel.append(washBox, washText);
    this.body.appendChild(washLabel);

    // The extra image step.
    const cleanLabel = document.createElement("label");
    cleanLabel.className = "torrent-check";
    const cleanBox = document.createElement("input");
    cleanBox.type = "checkbox";
    cleanBox.checked = isImage;
    cleanBox.disabled = !isImage && !isFolder;
    const cleanText = document.createElement("span");
    cleanText.textContent = isFolder
      ? "🖼 Images inside: also re-encode them from pixels — absolutely everything stripped, new file hashes"
      : "🖼 Extra step for images: re-encode from pixels — absolutely everything stripped, brand-new file hash";
    cleanLabel.append(cleanBox, cleanText);
    if (!isImage && !isFolder) {
      cleanLabel.classList.add("disabled");
      cleanText.textContent += " (images only)";
    }
    this.body.appendChild(cleanLabel);

    const privLabel = document.createElement("label");
    privLabel.className = "torrent-check";
    const privBox = document.createElement("input");
    privBox.type = "checkbox";
    const privText = document.createElement("span");
    privText.textContent =
      "🔒 Private torrent (for private trackers — disables DHT/PeX in clients)";
    privLabel.append(privBox, privText);
    this.body.appendChild(privLabel);

    const trackersLabel = document.createElement("label");
    trackersLabel.className = "torrent-label";
    trackersLabel.textContent = "Trackers (one per line):";
    const trackersArea = document.createElement("textarea");
    trackersArea.className = "torrent-trackers";
    trackersArea.rows = 3;
    trackersArea.value = DEFAULT_TRACKERS;
    trackersLabel.appendChild(trackersArea);
    this.body.appendChild(trackersLabel);

    const row = document.createElement("div");
    row.className = "torrent-row";
    const create = document.createElement("button");
    create.className = "pill-btn primary";
    create.textContent = "Create Torrent";
    create.addEventListener("click", async () => {
      create.disabled = true;
      create.textContent =
        washBox.checked || cleanBox.checked
          ? "Washing + building…"
          : "Building…";
      try {
        const result = await api.createTorrent(
          item.id,
          washBox.checked,
          cleanBox.checked,
          trackersArea.value.split("\n"),
          privBox.checked,
        );
        this.onCreated?.();
        this.renderResult(
          result.torrent_path,
          result.payload_path,
          result.washed_files,
          result.cleaned_images,
          washBox.checked || cleanBox.checked,
        );
      } catch (err) {
        Toast.error(`Torrent failed: ${errText(err)}`);
        create.disabled = false;
        create.textContent = "Create Torrent";
      }
    });
    const cancel = document.createElement("button");
    cancel.className = "pill-btn";
    cancel.textContent = "Cancel";
    cancel.addEventListener("click", () => this.close());
    row.append(create, cancel);
    this.body.appendChild(row);
  }

  private renderResult(
    torrentPath: string,
    payloadPath: string,
    washed: number,
    cleaned: number,
    staged: boolean,
  ): void {
    this.body.replaceChildren();
    const ok = document.createElement("div");
    ok.className = "torrent-note";
    const parts = [`✅ Torrent created: ${torrentPath}`];
    if (staged) {
      parts.push(
        `It points at the staged copy: ${payloadPath} — seed THAT in your torrent client. Your original was not modified.`,
      );
      if (washed > 0)
        parts.push(`Washed ${washed} file${washed === 1 ? "" : "s"}.`);
      if (cleaned > 0)
        parts.push(
          `Re-encoded ${cleaned} image${cleaned === 1 ? "" : "s"} from pixels.`,
        );
    } else {
      parts.push("It points at the original file, as-is.");
    }
    ok.textContent = parts.join("\n");
    const row = document.createElement("div");
    row.className = "torrent-row";
    const done = document.createElement("button");
    done.className = "pill-btn primary";
    done.textContent = "Done";
    done.addEventListener("click", () => this.close());
    row.appendChild(done);
    this.body.append(ok, row);
  }
}
