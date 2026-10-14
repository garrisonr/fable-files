/**
 * BurnStudio — the old Mac burn folder, grown up. Saved projects
 * remember a set of files + a target (CD/DVD/Blu-ray/USB/ISO) and
 * can be re-stamped at any time: build the ISO fresh, optionally
 * verify every file's checksum, optionally mount it, write it to a
 * USB stick, or burn a real disc.
 */
import { api } from "./api";
import { FolderPicker } from "./picker";
import { Toast, promptDialog, errText} from "./toast";
import { Dropdown } from "./dropdown";

export type BurnTarget = "cd" | "dvd" | "bluray" | "iso" | "usb";

export interface BurnFile {
  path: string;
  size: number | null; // null = unknown (typed-in path)
}

export interface BurnProject {
  name: string;
  target: BurnTarget;
  files: BurnFile[];
  verify: boolean;
  mountAfter: boolean;
}

const KEY = "ff-burn-projects";
const CAPACITY: Record<BurnTarget, number | null> = {
  cd: 700 * 1024 * 1024,
  dvd: 4700 * 1024 * 1024,
  bluray: 25_000 * 1024 * 1024,
  iso: null,
  usb: null,
};

function loadProjects(): BurnProject[] {
  try {
    const parsed = JSON.parse(localStorage.getItem(KEY) ?? "[]") as BurnProject[];
    if (Array.isArray(parsed)) return parsed;
  } catch {
    // fall through
  }
  return [];
}

function fmt(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
  return `${(bytes / 1024 ** 3).toFixed(2)} GB`;
}

export interface BurnHooks {
  /** Current file-view selection as absolute paths (for Add Selected). */
  getSelectionPaths(): Promise<BurnFile[]>;
  /** Open a folder location in the file view (after mount). */
  openPathLocation(path: string, label: string): Promise<void>;
  refresh(): void;
}

export class BurnStudio {
  readonly element: HTMLDivElement;
  private projects: BurnProject[] = loadProjects();
  private current: BurnProject | null = null;
  private listEl: HTMLDivElement;
  private filesEl: HTMLDivElement;
  private nameInput: HTMLInputElement;
  private targetSel!: Dropdown;
  private verifyChk: HTMLInputElement;
  private mountChk: HTMLInputElement;
  private meterEl: HTMLDivElement;
  private statusEl: HTMLDivElement;
  private picker = new FolderPicker();
  private lastIso = new Map<string, string>();

  constructor(private hooks: BurnHooks) {
    this.element = document.createElement("div");
    this.element.className = "props-overlay";
    this.element.hidden = true;
    const dialog = document.createElement("div");
    dialog.className = "burn-dialog";
    dialog.setAttribute("role", "dialog");
    dialog.setAttribute("aria-label", "Burn Studio");
    const title = document.createElement("h2");
    title.className = "burn-title";
    title.textContent = "💿 Burn Studio";

    const cols = document.createElement("div");
    cols.className = "burn-cols";
    // Left: project list
    const left = document.createElement("div");
    left.className = "burn-left";
    this.listEl = document.createElement("div");
    this.listEl.className = "burn-projects";
    const newBtn = document.createElement("button");
    newBtn.className = "pill-btn";
    newBtn.textContent = "+ New Project";
    newBtn.addEventListener("click", () => this.newProject());
    left.append(this.listEl, newBtn);
    // Right: editor
    const right = document.createElement("div");
    right.className = "burn-right";
    this.nameInput = document.createElement("input");
    this.nameInput.className = "burn-name";
    this.nameInput.placeholder = "Project name (Omarchy Installer, HR Records…)";
    this.nameInput.addEventListener("input", () => {
      if (this.current) {
        this.current.name = this.nameInput.value;
        this.save();
        this.renderList();
      }
    });
    const targetRow = document.createElement("div");
    targetRow.className = "burn-targetrow";
    const tLabel = document.createElement("span");
    tLabel.textContent = "Target:";
    this.targetSel = new Dropdown(
      [
        { value: "cd", label: "CD \u00b7 700 MB" },
        { value: "dvd", label: "DVD \u00b7 4.7 GB" },
        { value: "bluray", label: "Blu-ray \u00b7 25 GB" },
        { value: "usb", label: "USB flash drive" },
        { value: "iso", label: "ISO image file" },
      ],
      "dvd",
      (v) => {
        if (this.current) {
          this.current.target = v as BurnTarget;
          this.save();
          this.renderMeter();
        }
      },
    );
    targetRow.append(tLabel, this.targetSel.el);
    this.filesEl = document.createElement("div");
    this.filesEl.className = "burn-files";
    const addRow = document.createElement("div");
    addRow.className = "burn-addrow";
    const addSel = document.createElement("button");
    addSel.className = "pill-btn";
    addSel.textContent = "Add Selected Files";
    addSel.addEventListener("click", () => void this.addSelected());
    const addPath = document.createElement("button");
    addPath.className = "pill-btn";
    addPath.textContent = "Add Path…";
    addPath.addEventListener("click", () => void this.addTypedPath());
    addRow.append(addSel, addPath);
    this.meterEl = document.createElement("div");
    this.meterEl.className = "burn-meter";
    const optRow = document.createElement("div");
    optRow.className = "burn-optrow";
    this.verifyChk = document.createElement("input");
    this.verifyChk.type = "checkbox";
    this.verifyChk.checked = true;
    const vLabel = document.createElement("label");
    vLabel.append(this.verifyChk, document.createTextNode(" Verify files after building"));
    this.verifyChk.addEventListener("change", () => {
      if (this.current) {
        this.current.verify = this.verifyChk.checked;
        this.save();
      }
    });
    this.mountChk = document.createElement("input");
    this.mountChk.type = "checkbox";
    const mLabel = document.createElement("label");
    mLabel.append(this.mountChk, document.createTextNode(" Mount when done"));
    this.mountChk.addEventListener("change", () => {
      if (this.current) {
        this.current.mountAfter = this.mountChk.checked;
        this.save();
      }
    });
    optRow.append(vLabel, mLabel);
    this.statusEl = document.createElement("div");
    this.statusEl.className = "burn-status";
    const actionRow = document.createElement("div");
    actionRow.className = "burn-actions";
    const buildBtn = document.createElement("button");
    buildBtn.className = "pill-btn primary";
    buildBtn.textContent = "Build ISO…";
    buildBtn.addEventListener("click", () => void this.build());
    const usbBtn = document.createElement("button");
    usbBtn.className = "pill-btn";
    usbBtn.textContent = "Write to USB…";
    usbBtn.addEventListener("click", () => void this.writeUsb());
    const discBtn = document.createElement("button");
    discBtn.className = "pill-btn";
    discBtn.textContent = "Burn to Disc";
    discBtn.addEventListener("click", () => void this.burnDisc());
    const delBtn = document.createElement("button");
    delBtn.className = "pill-btn danger";
    delBtn.textContent = "Delete Project";
    delBtn.addEventListener("click", () => this.deleteProject());
    const closeBtn = document.createElement("button");
    closeBtn.className = "pill-btn";
    closeBtn.textContent = "Close";
    closeBtn.addEventListener("click", () => this.close());
    actionRow.append(buildBtn, usbBtn, discBtn, delBtn, closeBtn);
    right.append(
      this.nameInput, targetRow, this.filesEl, addRow,
      this.meterEl, optRow, this.statusEl, actionRow,
    );
    cols.append(left, right);
    dialog.append(title, cols);
    dialog.appendChild(this.picker.element);
    this.element.appendChild(dialog);
    this.element.addEventListener("mousedown", (e) => {
      if (e.target === this.element) this.close();
    });
  }

  open(): void {
    if (this.projects.length === 0) this.newProject(false);
    this.current = this.projects[0] ?? null;
    this.renderAll();
    this.element.hidden = false;
  }

  close(): void {
    this.element.hidden = true;
  }

  /** Open with files pre-added (context-menu "Add to Burn Project"). */
  async openWith(files: BurnFile[]): Promise<void> {
    this.open();
    if (!this.current) return;
    for (const f of files) {
      if (!this.current.files.some((x) => x.path === f.path)) {
        this.current.files.push(f);
      }
    }
    this.save();
    this.renderFiles();
    this.renderMeter();
  }

  private newProject(render = true): void {
    const p: BurnProject = {
      name: `Disc ${this.projects.length + 1}`,
      target: "dvd",
      files: [],
      verify: true,
      mountAfter: false,
    };
    this.projects.push(p);
    this.current = p;
    this.save();
    if (render) this.renderAll();
  }

  private deleteProject(): void {
    if (!this.current) return;
    this.projects = this.projects.filter((p) => p !== this.current);
    this.current = this.projects[0] ?? null;
    this.save();
    this.renderAll();
  }

  private save(): void {
    try {
      localStorage.setItem(KEY, JSON.stringify(this.projects));
    } catch {
      // storage full/unavailable — projects live for the session
    }
  }

  private renderAll(): void {
    this.renderList();
    if (this.current) {
      this.nameInput.value = this.current.name;
      this.targetSel.setValue(this.current.target);
      this.verifyChk.checked = this.current.verify;
      this.mountChk.checked = this.current.mountAfter;
    }
    this.renderFiles();
    this.renderMeter();
  }

  private renderList(): void {
    this.listEl.replaceChildren();
    for (const p of this.projects) {
      const row = document.createElement("button");
      row.className =
        "burn-project" + (p === this.current ? " active" : "");
      row.textContent = `${p.target === "usb" ? "🔑" : "💿"} ${p.name}`;
      row.addEventListener("click", () => {
        this.current = p;
        this.renderAll();
      });
      this.listEl.appendChild(row);
    }
  }

  private renderFiles(): void {
    this.filesEl.replaceChildren();
    if (!this.current || this.current.files.length === 0) {
      const empty = document.createElement("div");
      empty.className = "burn-empty";
      empty.textContent =
        "No files yet — select files in the browser and hit “Add Selected Files”, or add a path.";
      this.filesEl.appendChild(empty);
      return;
    }
    for (const f of this.current.files) {
      const row = document.createElement("div");
      row.className = "burn-file";
      const name = document.createElement("span");
      name.className = "burn-file-name";
      name.textContent = f.path;
      name.title = f.path;
      const size = document.createElement("span");
      size.className = "burn-file-size";
      size.textContent = f.size != null ? fmt(f.size) : "—";
      const x = document.createElement("button");
      x.className = "icon-btn";
      x.textContent = "✕";
      x.addEventListener("click", () => {
        if (!this.current) return;
        this.current.files = this.current.files.filter((y) => y !== f);
        this.save();
        this.renderFiles();
        this.renderMeter();
      });
      row.append(name, size, x);
      this.filesEl.appendChild(row);
    }
  }

  private renderMeter(): void {
    if (!this.current) {
      this.meterEl.textContent = "";
      return;
    }
    const total = this.current.files.reduce((a, f) => a + (f.size ?? 0), 0);
    const unknown = this.current.files.some((f) => f.size == null);
    const cap = CAPACITY[this.current.target];
    let text = `Total: ${unknown ? "~" : ""}${fmt(total)}`;
    if (cap) {
      const pct = Math.min(100, (total / cap) * 100);
      text += ` of ${fmt(cap)} (${pct.toFixed(0)}%)${total > cap ? " — ⚠ OVER CAPACITY" : ""}`;
    }
    this.meterEl.textContent = text;
  }

  private async addSelected(): Promise<void> {
    if (!this.current) return;
    const files = await this.hooks.getSelectionPaths();
    if (files.length === 0) {
      Toast.error("Nothing selected in the file view.");
      return;
    }
    for (const f of files) {
      if (!this.current.files.some((x) => x.path === f.path)) {
        this.current.files.push(f);
      }
    }
    this.save();
    this.renderFiles();
    this.renderMeter();
  }

  private async addTypedPath(): Promise<void> {
    const path = await promptDialog(
      "Add a file or folder path:",
      "/home/you/some/folder",
    );
    if (!path || !this.current) return;
    this.current.files.push({ path: path.trim(), size: null });
    this.save();
    this.renderFiles();
    this.renderMeter();
  }

  private volumeLabel(): string {
    const raw = (this.current?.name ?? "FABLE")
      .toUpperCase()
      .replace(/[^A-Z0-9_]/g, "_")
      .slice(0, 32);
    return raw || "FABLE";
  }

  private async build(): Promise<string | null> {
    if (!this.current || this.current.files.length === 0) {
      Toast.error("The project is empty.");
      return null;
    }
    const dir = await this.picker.pick("Save the ISO into…", [], null);
    if (!dir) return null;
    const isoPath = `${dir}/${this.volumeLabel().toLowerCase().replace(/_/g, "-")}.iso`;
    this.statusEl.textContent = "Building ISO…";
    try {
      const paths = this.current.files.map((f) => f.path);
      await api.buildIso(paths, isoPath, this.volumeLabel());
      this.lastIso.set(this.current.name, isoPath);
      this.statusEl.textContent = `Built ${isoPath}`;
      this.hooks.refresh();
      if (this.current.verify) {
        this.statusEl.textContent = "Verifying every file's checksum…";
        const v = await api.verifyIso(isoPath, paths);
        this.statusEl.textContent =
          v.failed.length === 0
            ? `✓ Verified: ${v.ok}/${v.checked} files match their sources.`
            : `⚠ ${v.failed.length} file(s) differ: ${v.failed.slice(0, 3).join(", ")}${v.failed.length > 3 ? "…" : ""}`;
      }
      if (this.current.mountAfter) {
        await this.hooks.openPathLocation(isoPath, this.current.name);
      }
      return isoPath;
    } catch (err) {
      this.statusEl.textContent = `Build failed: ${errText(err)}`;
      Toast.error(`ISO build failed: ${errText(err)}`);
      return null;
    }
  }

  private async ensureIso(): Promise<string | null> {
    if (!this.current) return null;
    return this.lastIso.get(this.current.name) ?? (await this.build());
  }

  private async writeUsb(): Promise<void> {
    const iso = await this.ensureIso();
    if (!iso) return;
    let devices;
    try {
      devices = await api.listBlockDevices();
    } catch (err) {
      Toast.error(`Device scan failed: ${errText(err)}`);
      return;
    }
    if (devices.length === 0) {
      Toast.error("No USB/removable drives found. Plug one in and retry.");
      return;
    }
    const d = devices[0];
    if (devices.length > 1) {
      Toast.info(
        `Multiple drives found — using the first: ${d.path}. (v1 limit, sorry.)`,
      );
    }
    const typed = await promptDialog(
      `⚠ This ERASES ${d.path} (${d.model}, ${d.size}). Type the device path to confirm:`,
      d.path,
    );
    if (typed?.trim() !== d.path) {
      Toast.error("Confirmation didn't match — write cancelled.");
      return;
    }
    this.statusEl.textContent = `Writing to ${d.path}… (watch the jobs card, bottom-right)`;
    try {
      const jobId = await api.writeUsb(iso, d.path);
      Toast.info(`USB write started (job ${jobId.slice(0, 8)}…) — progress in the jobs card.`);
    } catch (err) {
      Toast.error(`USB write failed: ${errText(err)}`);
    }
  }

  private async burnDisc(): Promise<void> {
    const iso = await this.ensureIso();
    if (!iso) return;
    this.statusEl.textContent = "Burning… keep the drive closed.";
    try {
      const drive = await api.burnDisc(iso);
      this.statusEl.textContent = `Burned to ${drive}. ✓`;
      Toast.success("Disc burned.");
    } catch (err) {
      this.statusEl.textContent = `Burn failed: ${errText(err)}`;
      Toast.error(`Burn failed: ${errText(err)}`);
    }
  }
}
