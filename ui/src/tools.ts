/**
 * Tools — the Total Commander utility belt:
 * checksums (calculate / create / verify), synchronize folders,
 * split & join large files, and the duplicate file finder.
 */
import { api } from "./api";
import type { Item } from "./api";
import { FolderPicker } from "./picker";
import { Toast, errText} from "./toast";

function fmt(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
  return `${(bytes / 1024 ** 3).toFixed(2)} GB`;
}

function baseDialog(cls: string, label: string): {
  overlay: HTMLDivElement;
  dialog: HTMLDivElement;
} {
  const overlay = document.createElement("div");
  overlay.className = "props-overlay";
  overlay.hidden = true;
  const dialog = document.createElement("div");
  dialog.className = cls;
  dialog.setAttribute("role", "dialog");
  dialog.setAttribute("aria-label", label);
  overlay.appendChild(dialog);
  overlay.addEventListener("mousedown", (e) => {
    if (e.target === overlay) overlay.hidden = true;
  });
  return { overlay, dialog };
}

// ---------------------------------------------------------------------------
// Checksums
// ---------------------------------------------------------------------------

const CHECKSUM_EXTS = new Set(["md5", "sha1", "sha256", "sha512", "crc32"]);

export class ChecksumDialog {
  readonly element: HTMLDivElement;
  private out: HTMLDivElement;
  private algoSel: HTMLSelectElement;

  constructor() {
    const { overlay, dialog } = baseDialog("tools-dialog", "Checksums");
    this.element = overlay;
    const title = document.createElement("h2");
    title.textContent = "🔢 Checksums";
    this.algoSel = document.createElement("select");
    for (const a of ["sha256", "md5", "sha1", "sha512", "crc32"]) {
      const o = document.createElement("option");
      o.value = a;
      o.textContent = a.toUpperCase();
      this.algoSel.appendChild(o);
    }
    this.out = document.createElement("div");
    this.out.className = "tools-out";
    dialog.append(title, this.algoSel, this.out);
  }

  private item: Item | null = null;

  async open(item: Item): Promise<void> {
    this.item = item;
    this.element.hidden = false;
    this.out.replaceChildren();
    const ext = item.name.split(".").pop()?.toLowerCase() ?? "";
    const isChecksumFile = CHECKSUM_EXTS.has(ext);
    if (isChecksumFile) this.algoSel.value = ext;
    const row = document.createElement("div");
    row.className = "tools-row";
    const calc = document.createElement("button");
    calc.className = "pill-btn primary";
    calc.textContent = "Calculate";
    calc.addEventListener("click", () => void this.calculate());
    const create = document.createElement("button");
    create.className = "pill-btn";
    create.textContent = "Create checksum file";
    create.addEventListener("click", () => void this.create());
    row.append(calc, create);
    if (isChecksumFile) {
      const verify = document.createElement("button");
      verify.className = "pill-btn";
      verify.textContent = "Verify all entries";
      verify.addEventListener("click", () => void this.verify());
      row.appendChild(verify);
    }
    const close = document.createElement("button");
    close.className = "pill-btn";
    close.textContent = "Close";
    close.addEventListener("click", () => (this.element.hidden = true));
    row.appendChild(close);
    this.out.append(row);
    const hint = document.createElement("div");
    hint.className = "tools-hint";
    hint.textContent = isChecksumFile
      ? `“${item.name}” is a checksum file — Verify checks every file it lists.`
      : `Calculate the ${"checksum"} of “${item.name}”, or write a checksum file beside it.`;
    this.out.appendChild(hint);
  }

  private async calculate(): Promise<void> {
    if (!this.item) return;
    const algo = this.algoSel.value;
    this.show(`Hashing with ${algo.toUpperCase()}…`);
    try {
      const hash = await api.checksumItem(this.item.id, algo);
      this.show(`${algo}: ${hash}`, true);
    } catch (err) {
      this.show(`Failed: ${errText(err)}`);
    }
  }

  private async create(): Promise<void> {
    if (!this.item) return;
    try {
      const path = await api.createChecksumFile(this.item.id, this.algoSel.value);
      Toast.success(`Wrote ${path.split("/").pop()}`);
    } catch (err) {
      Toast.error(`Create failed: ${errText(err)}`);
    }
  }

  private async verify(): Promise<void> {
    if (!this.item) return;
    this.show("Verifying…");
    try {
      const results = await api.verifyChecksumFile(this.item.id);
      this.out.replaceChildren();
      let okCount = 0;
      for (const r of results) {
        const row = document.createElement("div");
        row.className = "tools-verify-row" + (r.ok ? " ok" : " bad");
        row.textContent = `${r.ok ? "✓" : "✗"} ${r.file}${r.ok ? "" : ` (expected ${r.expected.slice(0, 12)}…, got ${r.actual.slice(0, 12) || "missing"}…)`}`;
        if (r.ok) okCount++;
        this.out.appendChild(row);
      }
      const sum = document.createElement("div");
      sum.className = "tools-hint";
      sum.textContent = `${okCount}/${results.length} files OK.`;
      this.out.appendChild(sum);
    } catch (err) {
      this.show(`Verify failed: ${errText(err)}`);
    }
  }

  private show(text: string, mono = false): void {
    this.out.replaceChildren();
    const div = document.createElement("div");
    div.className = mono ? "tools-mono" : "tools-hint";
    div.textContent = text;
    this.out.appendChild(div);
  }
}

// ---------------------------------------------------------------------------
// Synchronize folders
// ---------------------------------------------------------------------------

interface SyncRow {
  rel: string;
  state: string;
  size_a: number;
  size_b: number;
}

export class SyncDialog {
  readonly element: HTMLDivElement;
  private aInput: HTMLInputElement;
  private bInput: HTMLInputElement;
  private listEl: HTMLDivElement;
  private statusEl: HTMLDivElement;
  private picker = new FolderPicker();
  private rows: SyncRow[] = [];
  private checks = new Map<string, HTMLInputElement>();
  private schedDir!: HTMLSelectElement;
  private schedEvery!: HTMLSelectElement;
  private schedList!: HTMLDivElement;
  onDone: (() => void) | null = null;

  constructor() {
    const { overlay, dialog } = baseDialog("tools-dialog wide", "Synchronize Folders");
    this.element = overlay;
    const title = document.createElement("h2");
    title.textContent = "🔀 Synchronize Folders";
    const mkPathRow = (label: string) => {
      const wrap = document.createElement("div");
      wrap.className = "tools-pathrow";
      const span = document.createElement("span");
      span.textContent = label;
      const input = document.createElement("input");
      input.className = "tools-path";
      const browse = document.createElement("button");
      browse.className = "pill-btn";
      browse.textContent = "Browse…";
      browse.addEventListener("click", async () => {
        const picked = await this.picker.pick(`Choose folder ${label}`, [], null);
        if (picked) input.value = picked;
      });
      wrap.append(span, input, browse);
      dialog.appendChild(wrap);
      return input;
    };
    dialog.appendChild(title);
    this.aInput = mkPathRow("A");
    this.bInput = mkPathRow("B");
    const cmpRow = document.createElement("div");
    cmpRow.className = "tools-row";
    const cmp = document.createElement("button");
    cmp.className = "pill-btn primary";
    cmp.textContent = "Compare";
    cmp.addEventListener("click", () => void this.compare());
    const toB = document.createElement("button");
    toB.className = "pill-btn";
    toB.textContent = "Copy checked A → B";
    toB.addEventListener("click", () => void this.copy("a2b"));
    const toA = document.createElement("button");
    toA.className = "pill-btn";
    toA.textContent = "Copy checked B → A";
    toA.addEventListener("click", () => void this.copy("b2a"));
    const close = document.createElement("button");
    close.className = "pill-btn";
    close.textContent = "Close";
    close.addEventListener("click", () => (this.element.hidden = true));
    cmpRow.append(cmp, toB, toA, close);
    this.statusEl = document.createElement("div");
    this.statusEl.className = "tools-hint";
    this.listEl = document.createElement("div");
    this.listEl.className = "tools-list";
    // ——— Scheduled sync ———
    const schedWrap = document.createElement("div");
    schedWrap.className = "tools-sched";
    const schedTitle = document.createElement("h3");
    schedTitle.textContent = "⏰ Scheduled sync";
    const schedNote = document.createElement("p");
    schedNote.className = "tools-hint";
    schedNote.textContent =
      "Scheduled syncs run even when Fable Files isn't open. Turning on a schedule also turns on “start with the system” for Fable Files, so the timer can fire after you log in. Removing the last schedule turns autostart back off.";
    const schedRow = document.createElement("div");
    schedRow.className = "tools-row";
    this.schedDir = document.createElement("select");
    this.schedDir.className = "tools-path";
    for (const [v, label] of [
      ["a2b", "A → B"],
      ["b2a", "B → A"],
      ["both", "Both ways (newer wins)"],
    ] as const) {
      const o = document.createElement("option");
      o.value = v;
      o.textContent = label;
      this.schedDir.appendChild(o);
    }
    this.schedEvery = document.createElement("select");
    this.schedEvery.className = "tools-path";
    for (const [v, label] of [
      [15, "Every 15 minutes"],
      [60, "Every hour"],
      [360, "Every 6 hours"],
      [1440, "Every day"],
    ] as const) {
      const o = document.createElement("option");
      o.value = String(v);
      o.textContent = label;
      this.schedEvery.appendChild(o);
    }
    const schedBtn = document.createElement("button");
    schedBtn.className = "pill-btn";
    schedBtn.textContent = "Turn on schedule";
    schedBtn.addEventListener("click", () => void this.addSchedule());
    schedRow.append(this.schedDir, this.schedEvery, schedBtn);
    this.schedList = document.createElement("div");
    this.schedList.className = "tools-list";
    schedWrap.append(schedTitle, schedNote, schedRow, this.schedList);
    dialog.append(cmpRow, schedWrap, this.statusEl, this.listEl);
    dialog.appendChild(this.picker.element);
  }

  open(startPath: string): void {
    this.aInput.value = startPath;
    this.element.hidden = false;
    void this.renderSchedules();
  }

  private async addSchedule(): Promise<void> {
    const a = this.aInput.value.trim();
    const b = this.bInput.value.trim();
    if (!a || !b) {
      Toast.error("Set both folders first.");
      return;
    }
    try {
      await api.saveSyncSchedule(a, b, this.schedDir.value, Number(this.schedEvery.value));
      Toast.success("Schedule on — Fable Files will start with the system so syncs can run.");
      await this.renderSchedules();
    } catch (err) {
      Toast.error(`Could not schedule: ${errText(err)}`);
    }
  }

  private async renderSchedules(): Promise<void> {
    try {
      const list = await api.listSyncSchedules();
      this.schedList.replaceChildren();
      const arrow: Record<string, string> = { a2b: "→", b2a: "←", both: "⇄" };
      const every = (m: number) =>
        m >= 1440 ? "daily" : m >= 60 ? `every ${m / 60}h` : `every ${m} min`;
      for (const sc of list) {
        const row = document.createElement("div");
        row.className = "tools-sync-row";
        const text = document.createElement("span");
        text.textContent = `${sc.a} ${arrow[sc.direction] ?? "→"} ${sc.b} · ${every(sc.interval_min)}`;
        const rm = document.createElement("button");
        rm.className = "pill-btn";
        rm.textContent = "Remove";
        rm.addEventListener("click", async () => {
          await api.deleteSyncSchedule(sc.id);
          await this.renderSchedules();
        });
        row.append(text, rm);
        this.schedList.appendChild(row);
      }
      if (list.length === 0) {
        const p = document.createElement("p");
        p.className = "tools-hint";
        p.textContent = "No schedules yet.";
        this.schedList.appendChild(p);
      }
    } catch {
      // schedule list is best-effort UI
    }
  }

  private async compare(): Promise<void> {
    const a = this.aInput.value.trim();
    const b = this.bInput.value.trim();
    if (!a || !b) {
      Toast.error("Set both folders first.");
      return;
    }
    this.statusEl.textContent = "Comparing…";
    try {
      this.rows = await api.compareDirs(a, b);
      this.render();
      const diff = this.rows.filter((r) => r.state !== "same").length;
      this.statusEl.textContent =
        `${this.rows.length} files compared · ${diff} differ · ${this.rows.length - diff} identical.`;
    } catch (err) {
      this.statusEl.textContent = `Compare failed: ${errText(err)}`;
    }
  }

  private render(): void {
    this.listEl.replaceChildren();
    this.checks.clear();
    const ICON: Record<string, string> = {
      only_a: "→ only in A",
      only_b: "← only in B",
      different: "≠ different",
      same: "= same",
    };
    for (const r of this.rows) {
      if (r.state === "same") continue;
      const row = document.createElement("label");
      row.className = "tools-sync-row";
      const chk = document.createElement("input");
      chk.type = "checkbox";
      chk.checked = true;
      this.checks.set(r.rel, chk);
      const text = document.createElement("span");
      text.textContent = `${r.rel} · ${ICON[r.state]} · ${fmt(r.size_a)} / ${fmt(r.size_b)}`;
      row.append(chk, text);
      this.listEl.appendChild(row);
    }
    if (this.listEl.childElementCount === 0) {
      this.listEl.textContent = "The folders are in sync. 🎉";
    }
  }

  private async copy(dir: "a2b" | "b2a"): Promise<void> {
    const rels = [...this.checks.entries()]
      .filter(([, c]) => c.checked)
      .map(([rel]) => rel)
      // For b2a only rows that exist in B; for a2b rows that exist in A.
      .filter((rel) => {
        const row = this.rows.find((r) => r.rel === rel);
        if (!row) return false;
        return dir === "a2b" ? row.state !== "only_b" : row.state !== "only_a";
      });
    if (rels.length === 0) {
      Toast.error("Nothing checked to copy in that direction.");
      return;
    }
    try {
      const n =
        dir === "a2b"
          ? await api.syncCopy(this.aInput.value.trim(), this.bInput.value.trim(), rels)
          : await api.syncCopy(this.bInput.value.trim(), this.aInput.value.trim(), rels);
      Toast.success(`Copied ${n} file(s).`);
      this.onDone?.();
      await this.compare();
    } catch (err) {
      Toast.error(`Copy failed: ${errText(err)}`);
    }
  }
}

// ---------------------------------------------------------------------------
// Split
// ---------------------------------------------------------------------------

export class SplitDialog {
  readonly element: HTMLDivElement;
  private sizeSel: HTMLSelectElement;
  private customInput: HTMLInputElement;
  private item: Item | null = null;
  onDone: (() => void) | null = null;

  constructor() {
    const { overlay, dialog } = baseDialog("tools-dialog", "Split File");
    this.element = overlay;
    const title = document.createElement("h2");
    title.textContent = "✂ Split File";
    const hint = document.createElement("div");
    hint.className = "tools-hint";
    hint.textContent =
      "Creates name.001, name.002, … beside the original. Join later by right-clicking the .001 part.";
    this.sizeSel = document.createElement("select");
    for (const [label, mb] of [
      ["10 MB", 10],
      ["100 MB", 100],
      ["650 MB (CD)", 650],
      ["1.4 GB", 1400],
      ["4.4 GB (DVD)", 4480],
      ["Custom…", -1],
    ] as Array<[string, number]>) {
      const o = document.createElement("option");
      o.value = String(mb);
      o.textContent = label;
      this.sizeSel.appendChild(o);
    }
    this.sizeSel.value = "100";
    this.customInput = document.createElement("input");
    this.customInput.className = "tools-path";
    this.customInput.placeholder = "Custom size in MB";
    this.customInput.hidden = true;
    this.sizeSel.addEventListener("change", () => {
      this.customInput.hidden = this.sizeSel.value !== "-1";
    });
    const row = document.createElement("div");
    row.className = "tools-row";
    const go = document.createElement("button");
    go.className = "pill-btn primary";
    go.textContent = "Split";
    go.addEventListener("click", () => void this.split());
    const close = document.createElement("button");
    close.className = "pill-btn";
    close.textContent = "Cancel";
    close.addEventListener("click", () => (this.element.hidden = true));
    row.append(go, close);
    dialog.append(title, hint, this.sizeSel, this.customInput, row);
  }

  open(item: Item): void {
    this.item = item;
    this.element.hidden = false;
  }

  private async split(): Promise<void> {
    if (!this.item) return;
    const mb =
      this.sizeSel.value === "-1"
        ? Number(this.customInput.value)
        : Number(this.sizeSel.value);
    if (!mb || mb <= 0) {
      Toast.error("Pick a part size.");
      return;
    }
    try {
      const parts = await api.splitFile(this.item.id, Math.floor(mb * 1024 * 1024));
      Toast.success(`Split into ${parts.length} part(s).`);
      this.element.hidden = true;
      this.onDone?.();
    } catch (err) {
      Toast.error(`Split failed: ${errText(err)}`);
    }
  }
}

// ---------------------------------------------------------------------------
// Duplicate finder
// ---------------------------------------------------------------------------

interface DupGroup {
  size: number;
  hash: string;
  paths: string[];
}

export class DuplicatesDialog {
  readonly element: HTMLDivElement;
  private listEl: HTMLDivElement;
  private statusEl: HTMLDivElement;
  private minSel: HTMLSelectElement;
  private rootPath = "";
  private checks = new Map<string, HTMLInputElement>();
  onDone: (() => void) | null = null;
  /** Set by the shell (inc-2 §3.4): route a started job to the
   * JobsPanel with a completion continuation. */
  onTrackJob:
    | ((
        jobId: string,
        label: string,
        onDone: (error: string | null) => Promise<void>,
      ) => void)
    | null = null;

  constructor() {
    const { overlay, dialog } = baseDialog("tools-dialog wide", "Duplicate Finder");
    this.element = overlay;
    const title = document.createElement("h2");
    title.textContent = "👯 Duplicate Finder";
    const ctrl = document.createElement("div");
    ctrl.className = "tools-row";
    const minLabel = document.createElement("span");
    minLabel.textContent = "Minimum size:";
    this.minSel = document.createElement("select");
    for (const [label, bytes] of [
      ["Any size", 0],
      ["1 KB", 1024],
      ["100 KB", 100 * 1024],
      ["1 MB", 1024 * 1024],
      ["10 MB", 10 * 1024 * 1024],
    ] as Array<[string, number]>) {
      const o = document.createElement("option");
      o.value = String(bytes);
      o.textContent = label;
      this.minSel.appendChild(o);
    }
    this.minSel.value = String(1024 * 1024);
    const find = document.createElement("button");
    find.className = "pill-btn primary";
    find.textContent = "Find Duplicates";
    find.addEventListener("click", () => void this.find());
    const trash = document.createElement("button");
    trash.className = "pill-btn danger";
    trash.textContent = "Trash Checked Copies";
    trash.addEventListener("click", () => void this.trashChecked());
    const close = document.createElement("button");
    close.className = "pill-btn";
    close.textContent = "Close";
    close.addEventListener("click", () => (this.element.hidden = true));
    ctrl.append(minLabel, this.minSel, find, trash, close);
    this.statusEl = document.createElement("div");
    this.statusEl.className = "tools-hint";
    this.listEl = document.createElement("div");
    this.listEl.className = "tools-list";
    dialog.append(title, ctrl, this.statusEl, this.listEl);
  }

  open(rootPath: string): void {
    this.rootPath = rootPath;
    this.statusEl.textContent = `Searching under ${rootPath} — files are only grouped when their full SHA-256 matches.`;
    this.element.hidden = false;
  }

  private async find(): Promise<void> {
    this.statusEl.textContent = "Hashing… big folders take a moment.";
    this.listEl.replaceChildren();
    try {
      const groups = await api.findDuplicates(
        this.rootPath,
        Number(this.minSel.value),
      );
      this.render(groups);
    } catch (err) {
      this.statusEl.textContent = `Search failed: ${errText(err)}`;
    }
  }

  private render(groups: DupGroup[]): void {
    this.checks.clear();
    this.listEl.replaceChildren();
    let wasted = 0;
    for (const g of groups) {
      wasted += g.size * (g.paths.length - 1);
      const head = document.createElement("div");
      head.className = "tools-dup-head";
      head.textContent = `${fmt(g.size)} × ${g.paths.length} copies — ${fmt(g.size * (g.paths.length - 1))} reclaimable`;
      this.listEl.appendChild(head);
      const sorted = [...g.paths].sort();
      sorted.forEach((p, i) => {
        const row = document.createElement("label");
        row.className = "tools-dup-row";
        const chk = document.createElement("input");
        chk.type = "checkbox";
        chk.checked = i > 0; // keep the first copy by default
        this.checks.set(p, chk);
        const text = document.createElement("span");
        text.textContent = p + (i === 0 ? "  (kept)" : "");
        row.append(chk, text);
        this.listEl.appendChild(row);
      });
    }
    this.statusEl.textContent =
      groups.length === 0
        ? "No duplicates found. A tidy bear den."
        : `${groups.length} duplicate group(s) — ${fmt(wasted)} reclaimable. Checked copies go to the Trash.`;
  }

  private async trashChecked(): Promise<void> {
    const chosen = [...this.checks.entries()]
      .filter(([, c]) => c.checked)
      .map(([p]) => p);
    if (chosen.length === 0) {
      Toast.error("No copies checked.");
      return;
    }
    // Inc-2 §3.4: ONE delete job for the whole selection (pausable /
    // cancellable like every other transfer) instead of per-path
    // trashFilePath calls. Bonus fix: the job executor writes the
    // .trashinfo Path= RAW, so these items now restore to the right
    // place via untrash (trash_path_internal URL-encoded it — §7.4).
    try {
      const jobId = await api.startDeleteFromPaths(chosen, true);
      const count = chosen.length;
      if (this.onTrackJob) {
        this.onTrackJob(jobId, `Trash ${count} duplicates`, async (error) => {
          if (error) {
            Toast.error(`Could not trash duplicates: ${errText(error)}`);
          } else {
            Toast.success(
              `Trashed ${count} duplicate cop${count === 1 ? "y" : "ies"}.`,
            );
          }
          this.onDone?.();
          await this.find();
        });
      } else {
        Toast.success(
          `Trashing ${count} duplicate cop${count === 1 ? "y" : "ies"}…`,
        );
        this.onDone?.();
      }
    } catch (err) {
      Toast.error(`Could not trash duplicates: ${errText(err)}`);
    }
  }
}
