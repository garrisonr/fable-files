// Folder Sync pairs — the two-way-with-memory section of the
// Synchronize Folders dialog (DESIGN: ~/workspace/fable-sync/DESIGN.md).
// Pairs are remembered folder couples kept equal by the ufm-sync
// engine: a plan is previewed first, the run is a journaled job, and
// the Conflict Law (never silently overwrite, never silently delete)
// is the engine's, not this panel's — this is only its face.

import { api } from "./api";
import type { SyncPairView, SyncPlanView } from "./api";
import { Toast, errText } from "./toast";

export class SyncPairsPanel {
  readonly element: HTMLDivElement;
  private listEl: HTMLDivElement;
  private planEl: HTMLDivElement;
  private statusEl: HTMLDivElement;
  private nameInput: HTMLInputElement;
  private pairs: SyncPairView[] = [];
  private planFor: string | null = null;
  onSynced: (() => void) | null = null;

  constructor(private getRoots: () => { a: string; b: string }) {
    this.element = document.createElement("div");
    this.element.className = "tools-sched";
    const title = document.createElement("h3");
    title.textContent = "🔁 Sync pairs — two-way, with memory";
    const hint = document.createElement("p");
    hint.className = "tools-hint";
    hint.textContent =
      "A pair keeps two folders equal and remembers the last sync, so it can tell “deleted here” from “new here”. One-sided changes travel. A deletion travels only when the other side never touched the file — delete-vs-edit keeps the file. If both sides changed the same file, the newest wins and the other copy is kept in a .fable-conflicts folder on its own side. Every run is a job: preview first, then watch it in the Jobs panel.";
    const nameRow = document.createElement("div");
    nameRow.className = "tools-row";
    this.nameInput = document.createElement("input");
    this.nameInput.className = "tools-path";
    this.nameInput.placeholder = "Pair name (e.g. Phone photos)";
    const save = document.createElement("button");
    save.className = "pill-btn";
    save.textContent = "Save A ↔ B as a pair";
    save.addEventListener("click", () => void this.savePair());
    nameRow.append(this.nameInput, save);
    this.listEl = document.createElement("div");
    this.listEl.className = "tools-list";
    this.statusEl = document.createElement("div");
    this.statusEl.className = "tools-hint";
    this.planEl = document.createElement("div");
    this.planEl.className = "tools-list";
    this.element.append(title, hint, nameRow, this.listEl, this.statusEl, this.planEl);
  }

  async refresh(): Promise<void> {
    try {
      this.pairs = await api.syncListPairs();
      this.render();
    } catch {
      // pair list is best-effort UI, like the schedule list
    }
  }

  private render(): void {
    this.listEl.replaceChildren();
    if (this.pairs.length === 0) {
      const p = document.createElement("p");
      p.className = "tools-hint";
      p.textContent = "No pairs yet — set A and B above, name the pair, save it.";
      this.listEl.appendChild(p);
      return;
    }
    for (const view of this.pairs) {
      const { pair } = view;
      const row = document.createElement("div");
      row.className = "tools-sync-row";
      const text = document.createElement("span");
      const avail =
        view.aAvailable && view.bAvailable
          ? ""
          : ` · ⚠ ${!view.aAvailable && !view.bAvailable ? "both sides" : !view.aAvailable ? "A" : "B"} not available`;
      text.textContent =
        `${pair.name} · A: ${pair.root_a} ↔ B: ${pair.root_b}` +
        (view.lastSummary ? ` · last: ${view.lastSummary}` : " · never synced") +
        avail;
      const preview = document.createElement("button");
      preview.className = "pill-btn";
      preview.textContent = "Preview";
      preview.addEventListener("click", () => void this.preview(pair.id));
      const run = document.createElement("button");
      run.className = "pill-btn primary";
      run.textContent = "Sync now";
      run.disabled = !view.aAvailable || !view.bAvailable;
      run.addEventListener("click", () => void this.run(pair.id));
      const del = document.createElement("button");
      del.className = "pill-btn";
      del.textContent = "Forget pair";
      del.addEventListener("click", () => void this.deletePair(pair.id, pair.name));
      row.append(text, preview, run, del);
      this.listEl.appendChild(row);
    }
  }

  private async savePair(): Promise<void> {
    const { a, b } = this.getRoots();
    if (!a || !b) {
      Toast.error("Set both folders first (the A and B fields above).");
      return;
    }
    try {
      this.pairs = await api.syncSavePair({
        id: "",
        name: this.nameInput.value.trim(),
        root_a: a,
        root_b: b,
      });
      this.nameInput.value = "";
      Toast.success("Pair saved.");
      this.render();
    } catch (err) {
      Toast.error(`Could not save pair: ${errText(err)}`);
    }
  }

  private async deletePair(id: string, name: string): Promise<void> {
    try {
      this.pairs = await api.syncDeletePair(id);
      if (this.planFor === id) {
        this.planFor = null;
        this.planEl.replaceChildren();
      }
      Toast.success(`Forgot pair “${name}” (its sync memory went with it).`);
      this.render();
    } catch (err) {
      Toast.error(`Could not forget pair: ${errText(err)}`);
    }
  }

  private async preview(pairId: string): Promise<void> {
    this.statusEl.textContent = "Scanning both sides…";
    this.planEl.replaceChildren();
    try {
      const view: SyncPlanView = await api.syncPlan(pairId);
      this.planFor = pairId;
      this.statusEl.textContent = view.summary;
      for (const action of view.actions) {
        const row = document.createElement("div");
        row.className = "tools-sync-row";
        row.textContent = action.line;
        this.planEl.appendChild(row);
      }
      for (const c of view.conflicts) {
        const row = document.createElement("div");
        row.className = "tools-sync-row";
        row.textContent = `⚠ conflict · ${c.rel} — ${c.reason}${c.loser_kept ? ` (kept at ${c.loser_kept})` : ""}`;
        this.planEl.appendChild(row);
      }
      for (const s of view.skipped) {
        const row = document.createElement("div");
        row.className = "tools-sync-row";
        row.textContent = `skipped · ${s.rel} — ${s.reason}`;
        this.planEl.appendChild(row);
      }
      if (view.actions.length === 0) {
        const row = document.createElement("div");
        row.className = "tools-sync-row";
        row.textContent = "Nothing to move — the pair is in sync. 🎉";
        this.planEl.appendChild(row);
      }
    } catch (err) {
      this.statusEl.textContent = `Preview failed: ${errText(err)}`;
    }
  }

  private async run(pairId: string): Promise<void> {
    try {
      const started = await api.syncStart(pairId);
      Toast.success(
        started.alreadyRunning
          ? "That sync is already running — watch it in the Jobs panel."
          : "Sync started — watch it in the Jobs panel. The pair's last-run summary updates when it lands.",
      );
      this.onSynced?.();
    } catch (err) {
      Toast.error(`Sync did not start: ${errText(err)}`);
    }
  }
}
