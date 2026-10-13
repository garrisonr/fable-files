/**
 * Labels — Finder-style named color tags.
 *
 * Definitions (name + color) live in localStorage; assignments are
 * path-keyed in the catalog (backend). Applying a label color-grades
 * the item's icon in every view.
 *
 * Starter set is tailored to Dickles (2026-10-13): HR/recruiting and
 * office work, IT/homelab, his film archive, media and gaming.
 */
import { api } from "./api";

export interface LabelDef {
  name: string;
  color: string;
}

export const DEFAULT_LABELS: LabelDef[] = [
  { name: "Photos", color: "#8b5cf6" }, // grape — his example
  { name: "Film & Video", color: "#6366f1" },
  { name: "Music", color: "#ec4899" },
  { name: "Media", color: "#14b8a6" },
  { name: "Gaming", color: "#84cc16" },
  { name: "HR & Hiring", color: "#3b82f6" },
  { name: "Resumes", color: "#06b6d4" },
  { name: "Office & Admin", color: "#64748b" },
  { name: "Payroll", color: "#10b981" },
  { name: "Money", color: "#eab308" },
  { name: "Receipts", color: "#f59e0b" },
  { name: "IT & Homelab", color: "#f97316" },
  { name: "Personal", color: "#f43f5e" },
  { name: "Urgent", color: "#ef4444" },
];

const KEY = "ff-label-defs";

/** Combined labels Dickles asked to split (2026-10-13): the first
 * name keeps the old color; the sibling gets its own. */
export const LABEL_SPLITS: Array<{ old: string; first: string; second: string; secondColor: string }> = [
  { old: "Music & Media", first: "Music", second: "Media", secondColor: "#14b8a6" },
  { old: "Payroll & Money", first: "Payroll", second: "Money", secondColor: "#eab308" },
];

export function loadLabelDefs(): LabelDef[] {
  try {
    const raw = localStorage.getItem(KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as LabelDef[];
      if (Array.isArray(parsed) && parsed.length > 0) {
        let changed = false;
        const out: LabelDef[] = [];
        for (const d of parsed) {
          const split = LABEL_SPLITS.find((x) => x.old === d.name);
          if (split) {
            changed = true;
            if (!parsed.some((x) => x.name === split.first)) out.push({ name: split.first, color: d.color });
            if (!parsed.some((x) => x.name === split.second)) out.push({ name: split.second, color: split.secondColor });
          } else {
            out.push(d);
          }
        }
        if (changed) saveLabelDefs(out);
        return out;
      }
    }
  } catch {
    // fall through to defaults
  }
  return DEFAULT_LABELS.map((d) => ({ ...d }));
}

/**
 * Assignment side of the split, run once at startup: anything filed
 * under a combined label ends up under BOTH new labels (rename
 * cascades to the first; the second is then added path by path), so
 * nothing loses its filing. Idempotent: skips labels that no longer
 * exist in the defs.
 */
export async function migrateLabelAssignments(
  defs: LabelDef[],
  api: {
    renamePathLabel(oldLabel: string, newLabel: string): Promise<void>;
    listItemsByLabel(label: string): Promise<Array<{ provider_key?: string | null }>>;
    setPathLabel(path: string, label: string, on: boolean): Promise<unknown>;
  },
): Promise<boolean> {
  let migrated = false;
  for (const split of LABEL_SPLITS) {
    if (!defs.some((d) => d.name === split.first) || !defs.some((d) => d.name === split.second)) continue;
    try {
      const before = await api.listItemsByLabel(split.old);
      if (before.length === 0) continue;
      await api.renamePathLabel(split.old, split.first);
      const items = await api.listItemsByLabel(split.first);
      for (const it of items) {
        if (it.provider_key) await api.setPathLabel(it.provider_key, split.second, true);
      }
      migrated = true;
    } catch {
      // best effort — a failed split leaves the old label usable
    }
  }
  return migrated;
}

export function saveLabelDefs(defs: LabelDef[]): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(defs));
  } catch {
    // storage unavailable — definitions just won't persist
  }
}

export function labelColor(defs: LabelDef[], name: string): string | null {
  const d = defs.find((x) => x.name === name);
  return d ? d.color : "#9ca3af"; // unknown labels render gray
}

/** Manage Labels dialog: rename/recolor/add/delete definitions.
 * Renames and deletes cascade to assignments via the backend. */
export class LabelManager {
  private el: HTMLDivElement;
  private listEl: HTMLDivElement;
  private defs: LabelDef[] = [];
  private onChanged: (() => void) | null = null;

  constructor() {
    this.el = document.createElement("div");
    this.el.className = "picker-overlay";
    this.el.hidden = true;
    this.el.innerHTML = `
      <div class="picker-card labels-card" role="dialog" aria-label="Manage labels">
        <h3 class="picker-title">Labels</h3>
        <div class="labels-list"></div>
        <div class="labels-add">
          <input class="labels-new-color" type="color" value="#8b5cf6" />
          <input class="labels-new-name" type="text" placeholder="New label name" />
          <button class="labels-add-btn">Add</button>
        </div>
        <div class="picker-actions">
          <button class="labels-done primary">Done</button>
        </div>
      </div>`;
    this.listEl = this.el.querySelector(".labels-list")!;
    this.el
      .querySelector(".labels-done")!
      .addEventListener("click", () => this.close());
    this.el
      .querySelector(".labels-add-btn")!
      .addEventListener("click", () => void this.addFromInputs());
    this.el.addEventListener("pointerdown", (e) => {
      if (e.target === this.el) this.close();
    });
  }

  get element(): HTMLElement {
    return this.el;
  }

  open(onChanged: () => void): void {
    this.defs = loadLabelDefs();
    this.onChanged = onChanged;
    this.render();
    this.el.hidden = false;
  }

  private render(): void {
    this.listEl.innerHTML = "";
    for (const def of this.defs) {
      const row = document.createElement("div");
      row.className = "labels-row";
      const color = document.createElement("input");
      color.type = "color";
      color.value = def.color;
      color.addEventListener("change", () => {
        def.color = color.value;
        this.persist();
      });
      const name = document.createElement("input");
      name.type = "text";
      name.value = def.name;
      name.className = "labels-name";
      name.addEventListener("change", () => {
        const next = name.value.trim();
        if (!next || next === def.name) {
          name.value = def.name;
          return;
        }
        const old = def.name;
        def.name = next;
        void api.renamePathLabel(old, next).catch(() => undefined);
        this.persist();
      });
      const del = document.createElement("button");
      del.textContent = "✕";
      del.className = "labels-del";
      del.title = `Delete label "${def.name}" (removes it from all files)`;
      del.addEventListener("click", () => {
        void api.deletePathLabel(def.name).catch(() => undefined);
        this.defs = this.defs.filter((d) => d !== def);
        this.persist();
        this.render();
      });
      row.append(color, name, del);
      this.listEl.appendChild(row);
    }
  }

  private async addFromInputs(): Promise<void> {
    const nameEl = this.el.querySelector(
      ".labels-new-name",
    ) as HTMLInputElement;
    const colorEl = this.el.querySelector(
      ".labels-new-color",
    ) as HTMLInputElement;
    const name = nameEl.value.trim();
    if (!name) return;
    if (this.defs.some((d) => d.name.toLowerCase() === name.toLowerCase())) {
      nameEl.value = "";
      nameEl.placeholder = `"${name}" already exists`;
      return;
    }
    this.defs.push({ name, color: colorEl.value });
    nameEl.value = "";
    this.persist();
    this.render();
  }

  private persist(): void {
    saveLabelDefs(this.defs);
    if (this.onChanged) this.onChanged();
  }

  private close(): void {
    this.el.hidden = true;
    this.onChanged = null;
  }
}
