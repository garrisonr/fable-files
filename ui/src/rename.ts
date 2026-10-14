import { errText } from "./toast";
/**
 * RenameDialog — small modal for renaming one item (F2 / context menu,
 * Finder's Rename). Prefills the current name with the stem selected.
 */
import { api, type Item } from "./api";

export class RenameDialog {
  private el: HTMLDivElement;
  private input: HTMLInputElement;
  private errorEl: HTMLDivElement;
  private item: Item | null = null;
  private onDone: (() => void) | null = null;

  constructor() {
    this.el = document.createElement("div");
    this.el.className = "picker-overlay";
    this.el.hidden = true;
    this.el.innerHTML = `
      <div class="picker-card rename-card" role="dialog" aria-label="Rename">
        <h3 class="picker-title">Rename</h3>
        <input class="rename-input" type="text" spellcheck="false" />
        <div class="rename-error"></div>
        <div class="picker-actions">
          <button class="rename-cancel">Cancel</button>
          <button class="rename-ok primary">Rename</button>
        </div>
      </div>`;
    this.input = this.el.querySelector(".rename-input")!;
    this.errorEl = this.el.querySelector(".rename-error")!;
    this.el
      .querySelector(".rename-cancel")!
      .addEventListener("click", () => this.close());
    this.el
      .querySelector(".rename-ok")!
      .addEventListener("click", () => void this.submit());
    this.input.addEventListener("keydown", (e) => {
      if (e.key === "Enter") void this.submit();
      if (e.key === "Escape") this.close();
      e.stopPropagation();
    });
    this.el.addEventListener("pointerdown", (e) => {
      if (e.target === this.el) this.close();
    });
  }

  get element(): HTMLElement {
    return this.el;
  }

  open(item: Item, onDone: () => void): void {
    this.item = item;
    this.onDone = onDone;
    this.errorEl.textContent = "";
    this.input.value = item.name;
    this.el.hidden = false;
    // Select the stem, leaving the extension unselected (Finder-style).
    const dot = item.name.lastIndexOf(".");
    this.input.focus();
    if (item.kind !== "folder" && dot > 0) this.input.setSelectionRange(0, dot);
    else this.input.select();
  }

  private async submit(): Promise<void> {
    if (!this.item) return;
    const name = this.input.value.trim();
    if (!name || name === this.item.name) {
      this.close();
      return;
    }
    try {
      await api.renameItem(this.item.id, name);
      const done = this.onDone;
      this.close();
      if (done) done();
    } catch (err) {
      this.errorEl.textContent = `${errText(err)}`;
    }
  }

  private close(): void {
    this.el.hidden = true;
    this.item = null;
    this.onDone = null;
  }
}
