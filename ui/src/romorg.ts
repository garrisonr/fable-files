/**
 * ROM Organize — identified games filed by system + DAT title
 * (Organize parity, Dickles 2026-10-16). The games twin of the
 * media-server Organize: Fable reads the identities its hash
 * identification already stored, shows every move BEFORE it happens
 * (old path → new path), lists every skip with its reason, and runs
 * the moves as a journaled job the user can pause, cancel, and undo
 * (Ctrl+Z). Nothing here guesses from a file name — a file
 * identification hasn't claimed is simply not organized.
 */
import {
  api,
  type MediaresScanRequest,
  type RomOrganizePlan,
} from "./api";
import { Toast, errText } from "./toast";

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  e.className = className;
  if (text !== undefined) e.textContent = text;
  return e;
}

export class RomOrganizeDialog {
  private el: HTMLDivElement;
  private bodyEl!: HTMLDivElement;
  private footEl!: HTMLDivElement;
  private req: MediaresScanRequest | null = null;
  private onDone: (() => void) | null = null;

  constructor() {
    this.el = document.createElement("div");
    this.el.className = "picker-overlay";
    this.el.hidden = true;
    this.el.innerHTML = `
      <div class="picker-card labels-card mrx-card" role="dialog" aria-label="Organize video games">
        <h3 class="picker-title mrx-title">Organize Video Games</h3>
        <div class="mrx-body"></div>
        <div class="picker-actions mrx-foot"></div>
      </div>`;
    document.body.appendChild(this.el);
    this.bodyEl = this.el.querySelector(".mrx-body")!;
    this.footEl = this.el.querySelector(".mrx-foot")!;
    this.el.addEventListener("pointerdown", (e) => {
      if (e.target === this.el) this.close();
    });
  }

  /** Open on the folder being browsed: the plan's root and only
   * input are the folder itself; the plan expands it recursively. */
  async openFolder(path: string, onDone: () => void): Promise<void> {
    this.onDone = onDone;
    this.el.hidden = false;
    this.bodyEl.innerHTML = "";
    this.bodyEl.appendChild(
      el("p", "prefs-note", "Checking which games Fable has identified…"),
    );
    this.footEl.innerHTML = "";
    this.req = { root: path, paths: [path] };
    await this.preview();
  }

  private close(): void {
    this.el.hidden = true;
  }

  private async preview(): Promise<void> {
    if (!this.req) return;
    try {
      const plan = await api.romPlanOrganize(this.req);
      this.renderPlan(plan);
    } catch (err) {
      this.bodyEl.innerHTML = "";
      this.bodyEl.appendChild(
        el(
          "p",
          "prefs-note",
          `Could not build the organize preview: ${errText(err)}`,
        ),
      );
      this.renderFoot(null);
    }
  }

  private renderPlan(plan: RomOrganizePlan): void {
    const body = this.bodyEl;
    body.innerHTML = "";
    body.appendChild(
      el("h4", "mrx-preview-title", "Organize games — check every line"),
    );
    body.appendChild(
      el(
        "p",
        "mrx-intro",
        "Identified games move into one folder per system, under the name the game lists (DATs) give them. Only games Fable has identified are filed — nothing is renamed on a guess, and part of a disc set is never moved away from its set. Nothing is overwritten: a file whose destination already exists is skipped and listed below. The move runs as a normal Fable job — pause or cancel it in Transfers, and Ctrl+Z after it finishes undoes it.",
      ),
    );
    if (plan.truncated) {
      body.appendChild(
        el(
          "p",
          "mrx-note",
          "This folder holds more files than one organize pass covers — the rest are left for a second pass.",
        ),
      );
    }
    const list = el("div", "mrx-list");
    for (const a of plan.actions) {
      const rowEl = el("div", "mrx-plan-row");
      rowEl.appendChild(el("div", "mrx-old", a.src));
      rowEl.appendChild(el("div", "mrx-new", `→ ${a.rel}`));
      list.appendChild(rowEl);
    }
    for (const ex of plan.excluded) {
      const rowEl = el("div", "mrx-plan-row mrx-excluded");
      rowEl.appendChild(el("div", "mrx-old", ex.path));
      rowEl.appendChild(el("div", "mrx-note", `skipped — ${ex.reason}`));
      list.appendChild(rowEl);
    }
    body.appendChild(list);
    if (!plan.actions.length && !plan.excluded.length) {
      body.appendChild(
        el(
          "p",
          "prefs-note",
          "No files here yet — organize works on folders Fable has indexed.",
        ),
      );
    }
    this.renderFoot(plan);
  }

  private renderFoot(plan: RomOrganizePlan | null): void {
    const foot = this.footEl;
    foot.innerHTML = "";
    const cancel = el("button", "pill-btn", "Close");
    cancel.addEventListener("click", () => this.close());
    foot.appendChild(cancel);
    if (plan) {
      const go = el(
        "button",
        "pill-btn primary",
        plan.actions.length
          ? `Organize ${plan.actions.length} game${plan.actions.length === 1 ? "" : "s"}`
          : "Nothing to organize",
      );
      go.toggleAttribute("disabled", plan.actions.length === 0);
      go.addEventListener("click", () => void this.startOrganize());
      foot.appendChild(go);
    }
  }

  private async startOrganize(): Promise<void> {
    if (!this.req) return;
    try {
      await api.romStartOrganize(this.req);
      Toast.success(
        "Organize job started — watch it in Transfers. Ctrl+Z undoes a finished organize.",
        7000,
      );
      this.close();
      this.onDone?.();
    } catch (err) {
      Toast.error(`Organize did not start: ${errText(err)}`);
    }
  }
}
