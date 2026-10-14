/**
 * MetadataDialog — view/edit embedded metadata (exiftool) and run
 * the Washing Machine: strip everything identifying, or make a
 * clean re-encoded photo copy with nothing attached.
 */
import { api } from "./api";
import type { Item, MetaTag } from "./api";
import { Toast, errText} from "./toast";

const WRITABLE = new Set([
  "Artist", "Author", "Creator", "Copyright", "Make", "Model",
  "Software", "DateTimeOriginal", "CreateDate", "ModifyDate",
  "ImageDescription", "Comment", "Title", "Subject", "Keywords",
  "GPSLatitude", "GPSLongitude", "GPSAltitude", "SerialNumber",
  "LensModel", "CameraModelName",
]);

const IDENTIFYING = /gps|serial|make|model|artist|author|creator|datetime|createdate|modifydate|software|lens/i;

const IMAGE_EXTS = new Set([
  "jpg", "jpeg", "png", "webp", "gif", "tif", "tiff", "heic", "heif", "bmp",
]);

export class MetadataDialog {
  readonly element: HTMLDivElement;
  private body: HTMLDivElement;
  private titleEl: HTMLHeadingElement;
  private statusEl: HTMLDivElement;
  private item: Item | null = null;
  private onChanged: (() => void) | null = null;
  private edits = new Map<string, HTMLInputElement>();

  constructor() {
    this.element = document.createElement("div");
    this.element.className = "props-overlay";
    this.element.hidden = true;
    const dialog = document.createElement("div");
    dialog.className = "meta-dialog";
    dialog.setAttribute("role", "dialog");
    this.titleEl = document.createElement("h2");
    this.titleEl.className = "meta-title";
    const washRow = document.createElement("div");
    washRow.className = "meta-washrow";
    const washCopy = document.createElement("button");
    washCopy.className = "pill-btn";
    washCopy.textContent = "🧺 Wash a Copy";
    washCopy.title = "Copy the file, then strip ALL embedded metadata from the copy";
    washCopy.addEventListener("click", () => void this.wash(true));
    const washPlace = document.createElement("button");
    washPlace.className = "pill-btn";
    washPlace.textContent = "🧺 Wash In Place";
    washPlace.title = "Strip ALL embedded metadata from this file itself";
    washPlace.addEventListener("click", () => void this.wash(false));
    const cleanBtn = document.createElement("button");
    cleanBtn.className = "pill-btn";
    cleanBtn.textContent = "📸 Clean Photo Copy";
    cleanBtn.title =
      "Re-encode the pixels from scratch — nothing attached survives, brand-new hash";
    cleanBtn.addEventListener("click", () => void this.cleanCopy());
    washRow.append(washCopy, washPlace, cleanBtn);
    this.statusEl = document.createElement("div");
    this.statusEl.className = "meta-status";
    this.body = document.createElement("div");
    this.body.className = "meta-body";
    const foot = document.createElement("div");
    foot.className = "meta-foot";
    const save = document.createElement("button");
    save.className = "pill-btn primary";
    save.textContent = "Save Edits";
    save.addEventListener("click", () => void this.saveEdits());
    const close = document.createElement("button");
    close.className = "pill-btn";
    close.textContent = "Close";
    close.addEventListener("click", () => this.close());
    foot.append(save, close);
    dialog.append(this.titleEl, washRow, this.statusEl, this.body, foot);
    this.element.appendChild(dialog);
    this.element.addEventListener("mousedown", (e) => {
      if (e.target === this.element) this.close();
    });
  }

  async open(item: Item, onChanged: () => void): Promise<void> {
    this.item = item;
    this.onChanged = onChanged;
    this.titleEl.textContent = `Metadata — ${item.name}`;
    this.body.replaceChildren();
    this.statusEl.textContent = "Reading tags…";
    this.element.hidden = false;
    await this.load();
  }

  close(): void {
    this.element.hidden = true;
    this.item = null;
  }

  private async load(): Promise<void> {
    if (!this.item) return;
    this.edits.clear();
    try {
      const tags = await api.readMetadata(this.item.id);
      this.render(tags);
    } catch (err) {
      this.statusEl.textContent = `Could not read metadata: ${errText(err)}`;
    }
  }

  private render(tags: MetaTag[]): void {
    if (!this.item) return;
    const ext = this.item.name.split(".").pop()?.toLowerCase() ?? "";
    const identifying = tags.filter((t) => IDENTIFYING.test(t.tag)).length;
    this.statusEl.textContent =
      `${tags.length} tags · ${identifying} identifying ` +
      `(camera, GPS, serials, names, dates). ` +
      (IMAGE_EXTS.has(ext)
        ? "Clean Photo Copy gives you a re-encoded twin with zero tags."
        : "Wash strips embedded metadata; filesystem times are not inside the file.");
    const table = document.createElement("div");
    table.className = "meta-table";
    let lastGroup = "";
    for (const t of tags) {
      if (t.group !== lastGroup) {
        lastGroup = t.group;
        const gh = document.createElement("div");
        gh.className = "meta-group";
        gh.textContent = t.group;
        table.appendChild(gh);
      }
      const row = document.createElement("div");
      row.className =
        "meta-row" + (IDENTIFYING.test(t.tag) ? " identifying" : "");
      const name = document.createElement("span");
      name.className = "meta-tag";
      name.textContent = t.tag;
      row.appendChild(name);
      if (WRITABLE.has(t.tag)) {
        const input = document.createElement("input");
        input.className = "meta-input";
        input.value = t.value;
        input.placeholder = "(empty clears the tag)";
        this.edits.set(`${t.group}:${t.tag}`, input);
        row.appendChild(input);
      } else {
        const val = document.createElement("span");
        val.className = "meta-value";
        val.textContent = t.value;
        row.appendChild(val);
      }
      table.appendChild(row);
    }
    this.body.replaceChildren(table);
  }

  private async saveEdits(): Promise<void> {
    if (!this.item) return;
    const edits: Array<[string, string]> = [];
    for (const [key, input] of this.edits) {
      edits.push([key, input.value]);
    }
    try {
      await api.editMetadata(this.item.id, edits);
      Toast.success("Metadata saved.");
      await this.load();
      this.onChanged?.();
    } catch (err) {
      Toast.error(`Save failed: ${errText(err)}`);
    }
  }

  private async wash(copy: boolean): Promise<void> {
    if (!this.item) return;
    try {
      const r = await api.washMetadata(this.item.id, copy);
      Toast.success(
        `Washed: ${r.tags_before} → ${r.tags_after} tags` +
          (copy ? ` — saved as ${r.path.split("/").pop()}` : ""),
      );
      await this.load();
      this.onChanged?.();
    } catch (err) {
      Toast.error(`Wash failed: ${errText(err)}`);
    }
  }

  private async cleanCopy(): Promise<void> {
    if (!this.item) return;
    try {
      const path = await api.cleanPhotoCopy(this.item.id);
      Toast.success(`Clean copy saved: ${path.split("/").pop()}`);
      this.onChanged?.();
    } catch (err) {
      Toast.error(`Clean copy failed: ${errText(err)}`);
    }
  }
}
