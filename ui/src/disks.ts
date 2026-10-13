/**
 * DiskUtility — format, erase and resize disks/partitions, with
 * manners: the disk you booted from (and anything holding /, /boot
 * or /home) is locked with a polite explanation, mounted volumes
 * must be unmounted first, encrypted partitions warn about their
 * headers, and every destructive action needs the device path
 * typed back before it will run.
 */
import { api } from "./api";
import type { DiskInfo, DiskPart } from "./api";
import { Toast } from "./toast";

function fmt(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
  if (bytes < 1024 ** 4) return `${(bytes / 1024 ** 3).toFixed(2)} GB`;
  return `${(bytes / 1024 ** 4).toFixed(2)} TB`;
}

export class DiskUtility {
  readonly element: HTMLDivElement;
  private listEl: HTMLDivElement;
  private filesystems: string[] = [];
  private hintPath: string | null = null;
  onChanged: (() => void) | null = null;

  constructor() {
    this.element = document.createElement("div");
    this.element.className = "props-overlay";
    this.element.hidden = true;
    const dialog = document.createElement("div");
    dialog.className = "disk-dialog";
    dialog.setAttribute("role", "dialog");
    dialog.setAttribute("aria-label", "Disk Utility");
    const title = document.createElement("h2");
    title.textContent = "🛠 Disk Utility";
    const sub = document.createElement("p");
    sub.className = "disk-sub";
    sub.textContent =
      "Format, erase and resize. The disk this computer booted from is protected — the utility will tell you why, politely, instead of letting anything bad happen.";
    this.listEl = document.createElement("div");
    this.listEl.className = "disk-list";
    const foot = document.createElement("div");
    foot.className = "disk-foot";
    const refresh = document.createElement("button");
    refresh.className = "pill-btn";
    refresh.textContent = "Refresh";
    refresh.addEventListener("click", () => void this.load());
    const close = document.createElement("button");
    close.className = "pill-btn";
    close.textContent = "Close";
    close.addEventListener("click", () => this.close());
    foot.append(refresh, close);
    dialog.append(title, sub, this.listEl, foot);
    this.element.appendChild(dialog);
    this.element.addEventListener("mousedown", (e) => {
      if (e.target === this.element) this.close();
    });
  }

  /** Open; `hintPath` (a mount path) highlights its partition. */
  async open(hintPath?: string): Promise<void> {
    this.hintPath = hintPath ?? null;
    this.element.hidden = false;
    await this.load();
  }

  close(): void {
    this.element.hidden = true;
  }

  private async load(): Promise<void> {
    this.listEl.textContent = "Reading disks…";
    try {
      const [disks, fss] = await Promise.all([
        api.listDisks(),
        api.availableFilesystems(),
      ]);
      this.filesystems = fss;
      this.render(disks);
    } catch (err) {
      this.listEl.textContent = `Could not read disks: ${err}`;
    }
  }

  private render(disks: DiskInfo[]): void {
    this.listEl.replaceChildren();
    if (disks.length === 0) {
      this.listEl.textContent = "No disks found. A diskless computer — very zen.";
      return;
    }
    for (const disk of disks) this.listEl.appendChild(this.diskCard(disk));
  }

  private diskCard(disk: DiskInfo): HTMLDivElement {
    const card = document.createElement("div");
    card.className = "disk-card";
    const head = document.createElement("div");
    head.className = "disk-head";
    const icon = disk.removable || disk.transport === "usb" ? "🔑" : "💾";
    head.textContent =
      `${icon} ${disk.model} · ${fmt(disk.size_bytes)} · ${disk.path}` +
      (disk.transport ? ` · ${disk.transport}` : "") +
      (disk.removable ? " · removable" : "");
    card.appendChild(head);
    if (disk.locked && disk.lock_reason) {
      const note = document.createElement("div");
      note.className = "disk-locknote";
      note.textContent = `🔒 ${disk.lock_reason}`;
      card.appendChild(note);
    }
    for (const part of disk.partitions) {
      card.appendChild(this.partRow(disk, part));
    }
    if (disk.partitions.length === 0) {
      const empty = document.createElement("div");
      empty.className = "disk-noparts";
      empty.textContent = "No partitions — a blank slate.";
      card.appendChild(empty);
    }
    // Whole-disk erase
    const eraseRow = document.createElement("div");
    eraseRow.className = "disk-erase-row";
    if (!disk.locked) {
      const erase = document.createElement("button");
      erase.className = "pill-btn danger";
      erase.textContent = "Erase Entire Disk…";
      erase.addEventListener("click", () => {
        this.toggleForm(card, () =>
          this.eraseForm(disk),
        );
      });
      eraseRow.appendChild(erase);
    }
    card.appendChild(eraseRow);
    return card;
  }

  private partRow(disk: DiskInfo, part: DiskPart): HTMLDivElement {
    const row = document.createElement("div");
    row.className =
      "disk-part" +
      (this.hintPath && part.mountpoints.includes(this.hintPath)
        ? " hinted"
        : "");
    const info = document.createElement("div");
    info.className = "disk-part-info";
    const fsLabel =
      part.fstype === "crypto_LUKS"
        ? "🔐 encrypted (LUKS)"
        : (part.fstype ?? "no filesystem");
    info.textContent =
      `${part.path} · ${fmt(part.size_bytes)} · ${fsLabel}` +
      (part.label ? ` · “${part.label}”` : "") +
      (part.mountpoints.length
        ? ` · mounted at ${part.mountpoints.join(", ")}`
        : "");
    row.appendChild(info);
    const actions = document.createElement("div");
    actions.className = "disk-part-actions";
    if (part.locked) {
      const lock = document.createElement("span");
      lock.className = "disk-lock";
      lock.textContent = "🔒 protected";
      lock.title = part.lock_reason ?? "";
      actions.appendChild(lock);
    } else {
      if (part.mountpoints.length > 0) {
        const unmount = document.createElement("button");
        unmount.className = "pill-btn";
        unmount.textContent = "Unmount";
        unmount.addEventListener("click", async () => {
          try {
            await api.unmountPath(part.mountpoints[0]);
            Toast.success(`Unmounted ${part.path}.`);
            this.onChanged?.();
            await this.load();
          } catch (err) {
            Toast.error(`Unmount failed: ${err}`);
          }
        });
        actions.appendChild(unmount);
      }
      const format = document.createElement("button");
      format.className = "pill-btn";
      format.textContent = "Format…";
      format.disabled = part.mountpoints.length > 0;
      format.title =
        part.mountpoints.length > 0
          ? "Unmount it first — formatting a mounted volume is how data gets hurt."
          : "Erase this partition and give it a fresh filesystem";
      format.addEventListener("click", () => {
        this.toggleForm(row, () => this.formatForm(part));
      });
      const resize = document.createElement("button");
      resize.className = "pill-btn";
      resize.textContent = "Resize…";
      resize.addEventListener("click", () => {
        this.toggleForm(row, () => this.resizeForm(part));
      });
      actions.append(format, resize);
    }
    row.appendChild(actions);
    if (part.locked && part.lock_reason) {
      const reason = document.createElement("div");
      reason.className = "disk-lockreason";
      reason.textContent = part.lock_reason;
      row.appendChild(reason);
    }
    void disk;
    return row;
  }

  /** Expand/collapse an inline form under a row/card. */
  private toggleForm(
    host: HTMLElement,
    make: () => Promise<HTMLElement>,
  ): void {
    const existing = host.querySelector(".disk-form");
    if (existing) {
      existing.remove();
      return;
    }
    void make().then((form) => {
      // close siblings' forms first
      host.parentElement
        ?.querySelectorAll(".disk-form")
        .forEach((f) => f.remove());
      host.appendChild(form);
    });
  }

  private fsSelect(): HTMLSelectElement {
    const sel = document.createElement("select");
    sel.className = "disk-select";
    const NAMES: Record<string, string> = {
      ext4: "ext4 — Linux standard",
      btrfs: "btrfs — modern Linux, snapshots",
      exfat: "exFAT — big files, every OS reads it",
      vfat: "FAT32 — maximum compatibility, 4 GB file limit",
      ntfs: "NTFS — Windows",
      xfs: "XFS — large Linux volumes",
      f2fs: "F2FS — flash-friendly",
    };
    for (const fs of this.filesystems) {
      const o = document.createElement("option");
      o.value = fs;
      o.textContent = NAMES[fs] ?? fs;
      sel.appendChild(o);
    }
    return sel;
  }

  private async formatForm(part: DiskPart): Promise<HTMLElement> {
    const form = document.createElement("div");
    form.className = "disk-form";
    const warn = document.createElement("div");
    warn.className = "disk-warn";
    warn.textContent =
      `⚠ Formatting erases EVERYTHING on ${part.path} — ` +
      `currently ${part.fstype ?? "unformatted"}, ${fmt(part.size_bytes)}` +
      (part.label ? `, labeled “${part.label}”` : "") +
      `. There's no undo and no recovery worth the name.` +
      (part.fstype === "crypto_LUKS"
        ? " This partition is LUKS-encrypted: formatting destroys the encryption header too, so the data is gone for good."
        : "");
    const fsSel = this.fsSelect();
    const labelInput = document.createElement("input");
    labelInput.className = "disk-input";
    labelInput.placeholder = "New volume label (optional)";
    const confirmInput = document.createElement("input");
    confirmInput.className = "disk-input";
    confirmInput.placeholder = `Type ${part.path} to confirm`;
    const run = document.createElement("button");
    run.className = "pill-btn danger";
    run.textContent = `Format ${part.path}`;
    run.disabled = true;
    confirmInput.addEventListener("input", () => {
      run.disabled = confirmInput.value.trim() !== part.path;
    });
    run.addEventListener("click", async () => {
      run.disabled = true;
      run.textContent = "Formatting…";
      try {
        await api.formatPartition(
          part.path,
          fsSel.value,
          labelInput.value,
        );
        Toast.success(`Formatted ${part.path} as ${fsSel.value}.`);
        this.onChanged?.();
        await this.load();
      } catch (err) {
        Toast.error(`Format failed: ${err}`);
        run.disabled = false;
        run.textContent = `Format ${part.path}`;
      }
    });
    form.append(warn, fsSel, labelInput, confirmInput, run);
    return form;
  }

  private async eraseForm(disk: DiskInfo): Promise<HTMLElement> {
    const form = document.createElement("div");
    form.className = "disk-form";
    const partsSummary = disk.partitions
      .map(
        (p) =>
          `${p.path} (${p.fstype ?? "blank"}, ${fmt(p.size_bytes)}${p.label ? `, “${p.label}”` : ""})`,
      )
      .join("; ");
    const warn = document.createElement("div");
    warn.className = "disk-warn";
    warn.textContent =
      `⚠ Erasing ${disk.path} (${disk.model}, ${fmt(disk.size_bytes)}) destroys its partition table and EVERY partition on it` +
      (partsSummary ? `: ${partsSummary}.` : ".") +
      ` It'll be replaced with one fresh partition filling the disk. Nothing on it survives.`;
    const fsSel = this.fsSelect();
    const labelInput = document.createElement("input");
    labelInput.className = "disk-input";
    labelInput.placeholder = "Volume label (optional)";
    const confirmInput = document.createElement("input");
    confirmInput.className = "disk-input";
    confirmInput.placeholder = `Type ${disk.path} to confirm`;
    const run = document.createElement("button");
    run.className = "pill-btn danger";
    run.textContent = `Erase ${disk.path}`;
    run.disabled = true;
    confirmInput.addEventListener("input", () => {
      run.disabled = confirmInput.value.trim() !== disk.path;
    });
    run.addEventListener("click", async () => {
      run.disabled = true;
      run.textContent = "Erasing…";
      try {
        await api.eraseDisk(disk.path, fsSel.value, labelInput.value);
        Toast.success(`Erased ${disk.path} — one fresh ${fsSel.value} partition.`);
        this.onChanged?.();
        await this.load();
      } catch (err) {
        Toast.error(`Erase failed: ${err}`);
        run.disabled = false;
        run.textContent = `Erase ${disk.path}`;
      }
    });
    form.append(warn, fsSel, labelInput, confirmInput, run);
    return form;
  }

  private async resizeForm(part: DiskPart): Promise<HTMLElement> {
    const form = document.createElement("div");
    form.className = "disk-form";
    const loading = document.createElement("div");
    loading.textContent = "Checking what's possible…";
    form.appendChild(loading);
    let info;
    try {
      info = await api.partitionResizeInfo(part.path);
    } catch (err) {
      loading.textContent = `Couldn't check: ${err}`;
      return form;
    }
    form.replaceChildren();
    const note = document.createElement("div");
    note.className = "disk-note";
    const maxBytes = info.size_bytes + info.free_after_bytes;
    note.textContent =
      `Now: ${fmt(info.size_bytes)}.` +
      (info.free_after_bytes > 0
        ? ` Free space right after it: ${fmt(info.free_after_bytes)} (max ${fmt(maxBytes)}).`
        : " No free space directly after it, so it can't grow.") +
      ` ${info.note}`;
    form.appendChild(note);
    if (!info.can_grow && !info.can_shrink) return form;
    const gbInput = document.createElement("input");
    gbInput.className = "disk-input";
    gbInput.type = "number";
    gbInput.step = "0.1";
    gbInput.min = "0.1";
    gbInput.value = (maxBytes / 1024 ** 3).toFixed(1);
    const gbLabel = document.createElement("label");
    gbLabel.className = "disk-label";
    gbLabel.append(document.createTextNode("New size (GB): "), gbInput);
    form.appendChild(gbLabel);
    if (info.can_grow && info.free_after_bytes > 0) {
      const allBtn = document.createElement("button");
      allBtn.className = "pill-btn";
      allBtn.textContent = "Use all free space";
      allBtn.addEventListener("click", () => {
        gbInput.value = (maxBytes / 1024 ** 3).toFixed(1);
      });
      form.appendChild(allBtn);
    }
    const warn = document.createElement("div");
    warn.className = "disk-warn";
    warn.textContent =
      "⚠ Resizing is generally safe, but a power cut mid-resize can corrupt the filesystem. Back up anything precious first — bears are careful with other people's data.";
    const run = document.createElement("button");
    run.className = "pill-btn primary";
    run.textContent = "Resize";
    run.addEventListener("click", async () => {
      const gb = Number(gbInput.value);
      if (!gb || gb <= 0) {
        Toast.error("Enter a size in GB.");
        return;
      }
      const bytes = Math.floor(gb * 1024 ** 3);
      if (Math.abs(bytes - info.size_bytes) < 1024 ** 2) {
        Toast.error("That's (basically) the current size.");
        return;
      }
      run.disabled = true;
      run.textContent = "Resizing… don't close the app.";
      try {
        await api.resizePartition(part.path, bytes);
        Toast.success(`Resized ${part.path} to ${fmt(bytes)}.`);
        this.onChanged?.();
        await this.load();
      } catch (err) {
        Toast.error(`Resize failed: ${err}`);
        run.disabled = false;
        run.textContent = "Resize";
      }
    });
    form.append(warn, run);
    return form;
  }
}
