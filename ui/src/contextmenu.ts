/**
 * ContextMenu: right-click menu for file items.
 * Minimal, no dependencies — just a positioned div with buttons.
 */

export interface MenuItem {
  label: string;
  shortcut?: string;
  action: () => void;
  disabled?: boolean;
}

export class ContextMenu {
  private el: HTMLElement | null = null;

  constructor() {
    document.addEventListener("click", () => this.close(), true);
    document.addEventListener("keydown", (e) => {
      if (e.key === "Escape") this.close();
    });
    window.addEventListener("blur", () => this.close());
  }

  open(x: number, y: number, items: MenuItem[]): void {
    this.close();
    const menu = document.createElement("div");
    menu.className = "ctx-menu";
    menu.setAttribute("role", "menu");

    for (const item of items) {
      const b = document.createElement("button");
      b.className = "ctx-item";
      b.setAttribute("role", "menuitem");
      b.disabled = !!item.disabled;
      const label = document.createElement("span");
      label.textContent = item.label;
      b.appendChild(label);
      if (item.shortcut) {
        const sc = document.createElement("span");
        sc.className = "ctx-shortcut";
        sc.textContent = item.shortcut;
        b.appendChild(sc);
      }
      b.addEventListener("click", (e) => {
        e.stopPropagation();
        this.close();
        item.action();
      });
      menu.appendChild(b);
    }

    document.body.appendChild(menu);
    this.el = menu;

    // Position, keeping on-screen
    const rect = menu.getBoundingClientRect();
    const px = Math.min(x, window.innerWidth - rect.width - 8);
    const py = Math.min(y, window.innerHeight - rect.height - 8);
    menu.style.left = `${Math.max(8, px)}px`;
    menu.style.top = `${Math.max(8, py)}px`;
  }

  close(): void {
    if (this.el) {
      this.el.remove();
      this.el = null;
    }
  }

  get isOpen(): boolean {
    return this.el !== null;
  }
}
