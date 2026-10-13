/**
 * FolderPicker — in-app destination chooser for Move to… / Copy to… /
 * Extract to…. Sidebar-style shortcuts plus a drill-down folder browser
 * (history-stack navigation). Resolves to an absolute path.
 */
import { api } from "./api";

export interface PickerShortcut {
  label: string;
  locationId: string;
  dirId: string;
}

export class FolderPicker {
  private el: HTMLDivElement;
  private resolveFn: ((path: string | null) => void) | null = null;
  private currentPath = "";
  private history: string[] = [];

  constructor() {
    this.el = document.createElement("div");
    this.el.className = "picker-overlay";
    this.el.hidden = true;
    this.el.innerHTML = `
      <div class="picker-card" role="dialog" aria-label="Choose a folder">
        <h3 class="picker-title">Choose a folder</h3>
        <div class="picker-main">
          <div class="picker-shortcuts"></div>
          <div class="picker-list"></div>
        </div>
        <div class="picker-path"></div>
        <div class="picker-actions">
          <button class="picker-cancel">Cancel</button>
          <button class="picker-choose primary">Choose this folder</button>
        </div>
      </div>`;
    this.el
      .querySelector(".picker-cancel")!
      .addEventListener("click", () => this.close(null));
    this.el
      .querySelector(".picker-choose")!
      .addEventListener("click", () => this.close(this.currentPath || null));
    this.el.addEventListener("pointerdown", (e) => {
      if (e.target === this.el) this.close(null);
    });
  }

  get element(): HTMLElement {
    return this.el;
  }

  /** Show the picker; resolves to the chosen absolute path or null.
   * With a null start (and/or no shortcuts) it helps itself to the
   * location list: dialogs don't have to know the shell's state. */
  async pick(
    title: string,
    shortcuts: PickerShortcut[],
    start: { locationId: string; dirId: string } | null,
  ): Promise<string | null> {
    if (!start || shortcuts.length === 0) {
      try {
        const locs = await api.listLocations();
        if (shortcuts.length === 0) {
          shortcuts = locs.map((l) => ({
            label: l.display_name,
            locationId: l.id,
            dirId: l.root_item,
          }));
        }
        if (!start) {
          const home =
            locs.find((l) => l.display_name.toLowerCase().includes('home')) ??
            locs[0];
          if (home) {
            start = { locationId: home.id, dirId: home.root_item };
          }
        }
      } catch {
        // locations unavailable — fall through to the crash-free default
      }
    }
    if (!start) {
      return null;
    }
    this.el.querySelector(".picker-title")!.textContent = title;
    const sc = this.el.querySelector(".picker-shortcuts")!;
    sc.innerHTML = "";
    for (const s of shortcuts) {
      const b = document.createElement("button");
      b.className = "picker-shortcut";
      b.textContent = `☆ ${s.label}`;
      b.addEventListener("click", () => {
        this.history = [];
        void this.browse(s.locationId, s.dirId);
      });
      sc.appendChild(b);
    }
    this.history = [];
    this.el.hidden = false;
    await this.browse(start.locationId, start.dirId);
    return new Promise((resolve) => {
      this.resolveFn = resolve;
    });
  }

  private async browse(locationId: string, dirId: string): Promise<void> {
    const list = this.el.querySelector(".picker-list")!;
    list.innerHTML = "";
    try {
      const items = await api.listDir(locationId, dirId);
      this.currentPath = await api.getItemPath(dirId).catch(() => "");
      this.el.querySelector(".picker-path")!.textContent =
        this.currentPath || "(choose a folder)";
      if (this.history.length > 0) {
        const up = document.createElement("button");
        up.className = "picker-row picker-up";
        up.textContent = "⬆ Up one level";
        up.addEventListener("click", () => {
          const prev = this.history.pop()!;
          void this.browse(locationId, prev);
        });
        list.appendChild(up);
      }
      const folders = items
        .filter((i) => i.kind === "folder")
        .sort((a, b) => a.name.localeCompare(b.name));
      if (folders.length === 0) {
        const empty = document.createElement("div");
        empty.className = "picker-empty";
        empty.textContent = "No subfolders — you can still choose this folder";
        list.appendChild(empty);
      }
      for (const f of folders) {
        const row = document.createElement("button");
        row.className = "picker-row";
        row.textContent = `📁 ${f.name}`;
        row.addEventListener("click", () => {
          this.history.push(dirId);
          void this.browse(locationId, f.id);
        });
        list.appendChild(row);
      }
    } catch (err) {
      this.el.querySelector(".picker-path")!.textContent = `Error: ${err}`;
    }
  }

  private close(path: string | null): void {
    this.el.hidden = true;
    const r = this.resolveFn;
    this.resolveFn = null;
    if (r) r(path);
  }
}
