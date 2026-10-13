/**
 * CommandPalette: the spec's launcher — an anchored sheet from the top
 * with material blur, type-to-filter, keyboard-first. Ctrl+K / Super+K.
 * Feedback is instant on pointer-down, results track the query live.
 */
export interface PaletteCommand {
  title: string;
  hint?: string;
  run: () => void;
}

export class CommandPalette {
  private overlay: HTMLElement | null = null;
  private listEl: HTMLElement | null = null;
  private inputEl: HTMLInputElement | null = null;
  private filtered: PaletteCommand[] = [];
  private activeIdx = 0;

  get isOpen(): boolean {
    return this.overlay !== null;
  }

  open(commands: PaletteCommand[]): void {
    this.close();
    const overlay = document.createElement("div");
    overlay.className = "palette-overlay";
    overlay.addEventListener("mousedown", (e) => {
      if (e.target === overlay) this.close();
    });

    const sheet = document.createElement("div");
    sheet.className = "palette-sheet";
    sheet.setAttribute("role", "dialog");
    sheet.setAttribute("aria-label", "Command palette");

    const input = document.createElement("input");
    input.className = "palette-input";
    input.type = "text";
    input.placeholder = "Type a command or place…";
    input.setAttribute("aria-label", "Command palette");
    input.addEventListener("input", () => this.applyFilter(commands, input.value));
    input.addEventListener("keydown", (e) => this.onKey(e));

    const list = document.createElement("div");
    list.className = "palette-list";
    list.setAttribute("role", "listbox");

    sheet.append(input, list);
    overlay.appendChild(sheet);
    document.body.appendChild(overlay);
    this.overlay = overlay;
    this.listEl = list;
    this.inputEl = input;
    this.filtered = commands;
    this.activeIdx = 0;
    this.renderList();
    input.focus();
  }

  close(): void {
    if (this.overlay) {
      this.overlay.remove();
      this.overlay = null;
      this.listEl = null;
      this.inputEl = null;
    }
  }

  private applyFilter(commands: PaletteCommand[], q: string): void {
    const needle = q.trim().toLowerCase();
    this.filtered = needle
      ? commands.filter((c) => c.title.toLowerCase().includes(needle))
      : commands;
    this.activeIdx = 0;
    this.renderList();
  }

  private renderList(): void {
    if (!this.listEl) return;
    this.listEl.innerHTML = "";
    if (this.filtered.length === 0) {
      const p = document.createElement("p");
      p.className = "palette-empty";
      p.textContent = "No matching commands.";
      this.listEl.appendChild(p);
      return;
    }
    this.filtered.forEach((cmd, i) => {
      const row = document.createElement("button");
      row.className = "palette-row" + (i === this.activeIdx ? " active" : "");
      row.setAttribute("role", "option");
      const title = document.createElement("span");
      title.className = "palette-title";
      title.textContent = cmd.title;
      row.appendChild(title);
      if (cmd.hint) {
        const hint = document.createElement("span");
        hint.className = "palette-hint";
        hint.textContent = cmd.hint;
        row.appendChild(hint);
      }
      // Pointer-down, not click: feedback (and action) with zero latency.
      row.addEventListener("mousedown", (e) => {
        e.preventDefault();
        this.close();
        cmd.run();
      });
      row.addEventListener("mousemove", () => {
        if (this.activeIdx !== i) {
          this.activeIdx = i;
          this.renderList();
        }
      });
      this.listEl!.appendChild(row);
    });
  }

  private onKey(e: KeyboardEvent): void {
    if (e.key === "Escape") {
      this.close();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      this.activeIdx = Math.min(this.filtered.length - 1, this.activeIdx + 1);
      this.renderList();
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      this.activeIdx = Math.max(0, this.activeIdx - 1);
      this.renderList();
    } else if (e.key === "Enter") {
      e.preventDefault();
      const cmd = this.filtered[this.activeIdx];
      if (cmd) {
        this.close();
        cmd.run();
      }
    }
  }
}
