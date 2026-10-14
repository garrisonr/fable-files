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
import { Toast, confirmDialog, errText} from "./toast";

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
  { name: "Work", color: "#3b82f6" },
  { name: "Resumes", color: "#06b6d4" },
  { name: "Office & Admin", color: "#64748b" },
  { name: "Payroll", color: "#10b981" },
  { name: "Money", color: "#eab308" },
  { name: "Receipts", color: "#f59e0b" },
  { name: "IT & Homelab", color: "#f97316" },
  { name: "Personal", color: "#f43f5e" },
  { name: "Urgent", color: "#ef4444" },
  { name: "Taxes", color: "#b45309" },
  { name: "School", color: "#0ea5e9" },
  { name: "Health & Fitness", color: "#22c55e" },
  { name: "Audiobooks", color: "#7c3aed" },
  { name: "Auto", color: "#6b7280" },
  { name: "Household", color: "#d97706" },
  { name: "Legal", color: "#57534e" },
  { name: "Food", color: "#e11d48" },
  { name: "Screenshots", color: "#0891b2" },
];

/** Renames Dickles asked for (2026-10-13): defs rename in place;
 * assignments cascade through the backend. */
export const LABEL_RENAMES: Array<{ old: string; next: string }> = [
  { old: "HR & Hiring", next: "Work" },
];

/** Starter labels added in Round 5.3 — appended ONCE to existing
 * collections (a flag guards against resurrecting deleted ones). */
const V53_NEW = ["Taxes", "School", "Health & Fitness"];
const V53_FLAG = "ff-labels-v53";
const V55_FLAG = "ff-labels-v55";

/** Starter labels added in Round 5.5 (Dickles' list). */
const V55_NEW = ["Audiobooks", "Auto", "Household", "Legal", "Food", "Screenshots"];

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
        // Round 5.3 renames (def side; assignments cascade separately).
        for (const r of LABEL_RENAMES) {
          const at = out.findIndex((d) => d.name === r.old);
          if (at >= 0) {
            changed = true;
            if (out.some((d) => d.name === r.next)) out.splice(at, 1);
            else out[at] = { ...out[at], name: r.next };
          }
        }
        // Round 5.3 starter labels, appended once only.
        let v53 = false;
        try { v53 = !!localStorage.getItem(V53_FLAG); } catch { v53 = true; }
        if (!v53) {
          for (const name of V53_NEW) {
            if (!out.some((d) => d.name === name)) {
              const def = DEFAULT_LABELS.find((d) => d.name === name);
              if (def) { out.push({ ...def }); changed = true; }
            }
          }
          try { localStorage.setItem(V53_FLAG, "1"); } catch { /* ignore */ }
        }
        // Round 5.5 starter labels, appended once only.
        let v55 = false;
        try { v55 = !!localStorage.getItem(V55_FLAG); } catch { v55 = true; }
        if (!v55) {
          for (const name of V55_NEW) {
            if (!out.some((d) => d.name === name)) {
              const def = DEFAULT_LABELS.find((d) => d.name === name);
              if (def) { out.push({ ...def }); changed = true; }
            }
          }
          try { localStorage.setItem(V55_FLAG, "1"); } catch { /* ignore */ }
        }
        // Alphabetical, always (Dickles, 2026-10-13).
        out.sort((a, b) => a.name.localeCompare(b.name));
        if (changed) saveLabelDefs(out);
        return out;
      }
    }
  } catch {
    // fall through to defaults
  }
  return DEFAULT_LABELS.map((d) => ({ ...d })).sort((a, b) => a.name.localeCompare(b.name));
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
  for (const r of LABEL_RENAMES) {
    try {
      const before = await api.listItemsByLabel(r.old);
      if (before.length === 0) continue;
      await api.renamePathLabel(r.old, r.next);
      migrated = true;
    } catch {
      // best effort
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
// ---------------------------------------------------------------------------
// Auto-label rules: extension (+ optional minimum duration) → label.
// Stored locally; applied by the backend (apply_label_rules), which
// probes and caches durations. Seeded once with Dickles' examples.
// ---------------------------------------------------------------------------

import type { LabelRule } from "./api";

const RULES_KEY = "ff-label-rules";
const RULES_SEED_FLAG = "ff-label-rules-seeded";

export function loadLabelRules(): LabelRule[] {
  try {
    const raw = localStorage.getItem(RULES_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as LabelRule[];
      if (Array.isArray(parsed)) return parsed;
    }
  } catch {
    // fall through to seed
  }
  let seeded = false;
  try { seeded = !!localStorage.getItem(RULES_SEED_FLAG); } catch { seeded = true; }
  if (!seeded) {
    const seed: LabelRule[] = [
      { label: "Photos", extensions: ["jpg", "jpeg"], min_duration_secs: null },
      {
        label: "Film & Video",
        extensions: ["mkv", "avi", "mp4"],
        min_duration_secs: 70 * 60,
      },
    ];
    saveLabelRules(seed);
    try { localStorage.setItem(RULES_SEED_FLAG, "1"); } catch { /* ignore */ }
    return seed;
  }
  return [];
}

export function saveLabelRules(rules: LabelRule[]): void {
  try {
    localStorage.setItem(RULES_KEY, JSON.stringify(rules));
  } catch {
    // storage unavailable
  }
}

export class LabelManager {
  private el: HTMLDivElement;
  private listEl: HTMLDivElement;
  private rulesListEl!: HTMLDivElement;
  private rulesStatus!: HTMLSpanElement;
  private applyBtn!: HTMLButtonElement;
  private ruleLabelSel!: HTMLSelectElement;
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
        <div class="labels-rules">
          <h4 class="labels-rules-title">Auto-label rules</h4>
          <p class="labels-rules-blurb">When you press \u201cApply rules now\u201d, every indexed file whose name ends in one of a rule\u2019s extensions gets that rule\u2019s label. The minutes box is optional: fill it in and only files LONGER than that match \u2014 that\u2019s how Film &amp; Video catches movies but skips short clips. Leave it blank to match files of any length. Lengths are read once and remembered.</p>
          <div class="labels-rules-list"></div>
          <div class="labels-rule-add">
            <select class="labels-rule-label" aria-label="Label"></select>
            <input class="labels-rule-exts" type="text" placeholder="File extensions \u2014 e.g. jpg, png" />
            <span class="labels-rule-mins-wrap"><span class="labels-rule-mins-lab">Only if longer than</span><input class="labels-rule-mins" type="number" min="0" placeholder="min" /><span class="labels-rule-mins-lab">minutes (optional)</span></span>
            <button class="labels-rule-add-btn">Add rule</button>
          </div>
          <div class="labels-rules-actions">
            <button class="labels-apply-rules primary">Apply rules now</button>
            <span class="labels-rules-status" role="status"></span>
          </div>
        </div>
        <div class="labels-danger-row">
          <button class="labels-delete-all">Delete All Labels\u2026</button>
          <span class="labels-danger-hint">Removes every label and every file\u2019s assignments. Irreversible.</span>
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
    this.rulesListEl = this.el.querySelector(".labels-rules-list")!;
    this.rulesStatus = this.el.querySelector(".labels-rules-status")!;
    this.applyBtn = this.el.querySelector(".labels-apply-rules")!;
    this.ruleLabelSel = this.el.querySelector(".labels-rule-label")!;
    this.applyBtn.addEventListener("click", () => void this.applyRules());
    this.el
      .querySelector(".labels-rule-add-btn")!
      .addEventListener("click", () => this.addRuleFromInputs());
    this.el
      .querySelector(".labels-delete-all")!
      .addEventListener("click", () => void this.deleteAllLabels());
    this.el.addEventListener("pointerdown", (e) => {
      if (e.target === this.el) this.close();
    });
  }

  /** Delete every label + its file assignments (irreversible). */
  private async deleteAllLabels(): Promise<void> {
    if (!this.defs.length) {
      Toast.info("No labels to delete.");
      return;
    }
    const ok = await confirmDialog(
      "Delete ALL labels?",
      `${this.defs.length} labels will be removed, and every file loses its label assignments. This is irreversible.`,
      "Delete All Labels",
      true,
    );
    if (!ok) return;
    for (const d of [...this.defs]) {
      try {
        await api.deletePathLabel(d.name);
      } catch {
        /* keep going */
      }
    }
    this.defs = [];
    saveLabelDefs([]);
    saveLabelRules([]);
    this.render();
    this.renderRules();
    Toast.success("All labels deleted.", 6000);
    this.onChanged?.();
  }

  get element(): HTMLElement {
    return this.el;
  }

  open(onChanged: () => void): void {
    this.defs = loadLabelDefs();
    this.onChanged = onChanged;
    this.render();
    this.renderRules();
    this.el.hidden = false;
  }

  /** Open straight at the auto-label rules (hamburger menu entry). */
  openRules(onChanged: () => void): void {
    this.open(onChanged);
    requestAnimationFrame(() => {
      this.el
        .querySelector(".labels-rules")
        ?.scrollIntoView({ block: "nearest" });
    });
  }

  private renderRules(): void {
    const rules = loadLabelRules();
    this.ruleLabelSel.innerHTML = "";
    for (const d of this.defs) {
      const opt = document.createElement("option");
      opt.value = d.name;
      opt.textContent = d.name;
      this.ruleLabelSel.appendChild(opt);
    }
    this.rulesListEl.innerHTML = "";
    if (rules.length === 0) {
      const p = document.createElement("p");
      p.className = "labels-rules-empty";
      p.textContent = "No rules yet.";
      this.rulesListEl.appendChild(p);
      return;
    }
    rules.forEach((rule, idx) => {
      const row = document.createElement("div");
      row.className = "labels-rule-row";
      const text = document.createElement("span");
      text.className = "labels-rule-text";
      const exts = rule.extensions.map((e) => `.${e}`).join(" ");
      text.textContent = rule.min_duration_secs
        ? `${rule.label} ← ${exts} · longer than ${Math.round(rule.min_duration_secs / 60)} min`
        : `${rule.label} ← ${exts}`;
      const del = document.createElement("button");
      del.textContent = "✕";
      del.className = "labels-del";
      del.title = "Delete this rule";
      del.addEventListener("click", () => {
        const next = loadLabelRules();
        next.splice(idx, 1);
        saveLabelRules(next);
        this.renderRules();
      });
      row.append(text, del);
      this.rulesListEl.appendChild(row);
    });
  }

  private addRuleFromInputs(): void {
    const extsEl = this.el.querySelector(
      ".labels-rule-exts",
    ) as HTMLInputElement;
    const minsEl = this.el.querySelector(
      ".labels-rule-mins",
    ) as HTMLInputElement;
    const extensions = extsEl.value
      .split(/[\s,]+/)
      .map((e) => e.trim().replace(/^\./, "").toLowerCase())
      .filter(Boolean);
    const label = this.ruleLabelSel.value;
    if (!label || extensions.length === 0) {
      this.rulesStatus.textContent = "Pick a label and at least one extension.";
      return;
    }
    const mins = Number(minsEl.value);
    const rules = loadLabelRules();
    rules.push({
      label,
      extensions,
      min_duration_secs: mins > 0 ? mins * 60 : null,
    });
    saveLabelRules(rules);
    extsEl.value = "";
    minsEl.value = "";
    this.rulesStatus.textContent = "";
    this.renderRules();
  }

  private async applyRules(): Promise<void> {
    const rules = loadLabelRules();
    if (rules.length === 0) {
      this.rulesStatus.textContent = "No rules yet — add one above.";
      return;
    }
    this.applyBtn.disabled = true;
    try {
      let totalAdded = 0;
      for (let pass = 0; pass < 40; pass++) {
        const outcomes = await api.applyLabelRules(rules);
        let remaining = 0;
        for (const o of outcomes) {
          remaining += o.remaining;
          totalAdded += o.added;
        }
        this.rulesStatus.textContent =
          remaining > 0
            ? `Reading media durations… (${totalAdded} labeled so far)`
            : `Done — ${totalAdded} file${totalAdded === 1 ? "" : "s"} labeled.`;
        if (remaining === 0) break;
      }
      if (this.onChanged) this.onChanged();
    } catch (err) {
      this.rulesStatus.textContent = `Apply failed: ${errText(err)}`;
    } finally {
      this.applyBtn.disabled = false;
    }
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
    this.defs.sort((a, b) => a.name.localeCompare(b.name));
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
