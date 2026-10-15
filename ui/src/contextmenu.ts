/**
 * ContextMenu: right-click menu for file items, and the main
 * (hamburger) menu. Minimal, no dependencies — positioned divs with
 * buttons.
 *
 * Submenus (Dickles, 2026-10-15 — the main menu's named groups):
 * an item with `submenu` opens a flyout panel on hover or click.
 * Flyouts flip side near the right edge, drop below the parent item
 * on narrow screens, and support the full keyboard path (↑/↓ move,
 * → opens, ← closes, Esc closes one level at a time). Checks,
 * shortcuts, dots, and danger styling all work inside flyouts —
 * they share the same item rendering.
 */

export interface MenuItem {
  label: string;
  /** Render as a separator line (label/action ignored). */
  separator?: boolean;
  /** Colored dot shown before the label (label/tag menus). */
  dot?: string;
  /** Show a check mark (toggle state). */
  checked?: boolean;
  shortcut?: string;
  action: () => void;
  disabled?: boolean;
  /** Destructive action (Delete, Empty Trash, Forget…): renders
   * like a normal item at rest; hover/focus paints the bright-red
   * box with white text (.ctx-danger in styles.css). */
  danger?: boolean;
  /** Flyout submenu items. When present, `action` is not used —
   * the item is a group header, never a leaf. */
  submenu?: MenuItem[];
}

export class ContextMenu {
  /** Open panels, root first; flyouts stack after their parent. */
  private panels: HTMLElement[] = [];
  /** The parent button each flyout belongs to (index-aligned with
   * panels[1..]). */
  private parents: HTMLElement[] = [];

  constructor() {
    document.addEventListener("click", () => this.close(), true);
    document.addEventListener("keydown", (e) => this.onKey(e));
    window.addEventListener("blur", () => this.close());
  }

  open(x: number, y: number, items: MenuItem[]): void {
    this.close();
    const menu = this.buildPanel(items, 0);
    document.body.appendChild(menu);
    this.panels = [menu];
    this.parents = [];
    const rect = menu.getBoundingClientRect();
    const px = Math.min(x, window.innerWidth - rect.width - 8);
    const py = Math.min(y, window.innerHeight - rect.height - 8);
    menu.style.left = `${Math.max(8, px)}px`;
    menu.style.top = `${Math.max(8, py)}px`;
    menu.querySelector<HTMLElement>(".ctx-item:not(:disabled)")?.focus();
  }

  private buildPanel(items: MenuItem[], depth: number): HTMLElement {
    const menu = document.createElement("div");
    menu.className = "ctx-menu";
    menu.setAttribute("role", "menu");
    menu.dataset.depth = String(depth);

    for (const item of items) {
      if (item.separator) {
        const hr = document.createElement("div");
        hr.className = "ctx-sep";
        menu.appendChild(hr);
        continue;
      }
      const b = document.createElement("button");
      b.className = "ctx-item" + (item.danger ? " ctx-danger" : "");
      b.setAttribute("role", "menuitem");
      b.disabled = !!item.disabled;
      if (item.dot) {
        const dot = document.createElement("span");
        dot.className = "ctx-dot";
        dot.style.background = item.dot;
        b.appendChild(dot);
      }
      const label = document.createElement("span");
      label.textContent = item.label;
      b.appendChild(label);
      if (item.checked) {
        const check = document.createElement("span");
        check.className = "ctx-check";
        check.textContent = "✓";
        b.appendChild(check);
      }
      if (item.submenu && item.submenu.length > 0) {
        b.setAttribute("aria-haspopup", "menu");
        b.setAttribute("aria-expanded", "false");
        const arrow = document.createElement("span");
        arrow.className = "ctx-sub-arrow";
        arrow.textContent = "▸";
        b.appendChild(arrow);
        const openFlyout = () => this.openFlyout(menu, b, item.submenu!);
        b.addEventListener("mouseenter", openFlyout);
        b.addEventListener("click", (e) => {
          e.stopPropagation();
          openFlyout();
          // Focus the flyout's first item so the keyboard path
          // continues seamlessly from a click.
          const fly = this.panels[depth + 1];
          fly?.querySelector<HTMLElement>(".ctx-item:not(:disabled)")?.focus();
        });
      } else {
        if (item.shortcut) {
          const sc = document.createElement("span");
          sc.className = "ctx-shortcut";
          sc.textContent = item.shortcut;
          b.appendChild(sc);
        }
        // Hovering a leaf at this depth closes any flyout a sibling
        // opened — classic menu tracking.
        b.addEventListener("mouseenter", () => this.closeDeeperThan(depth));
        b.addEventListener("click", (e) => {
          e.stopPropagation();
          this.close();
          item.action();
        });
      }
      menu.appendChild(b);
    }
    return menu;
  }

  /** Open (or keep open) the flyout for `parentBtn` in `panel`. */
  private openFlyout(
    panel: HTMLElement,
    parentBtn: HTMLElement,
    items: MenuItem[],
  ): void {
    const depth = Number(panel.dataset.depth ?? 0);
    // Already this item's flyout? Leave it be.
    if (this.parents[depth] === parentBtn && this.panels[depth + 1]) {
      return;
    }
    this.closeDeeperThan(depth);
    const fly = this.buildPanel(items, depth + 1);
    document.body.appendChild(fly);
    this.panels[depth + 1] = fly;
    this.parents[depth] = parentBtn;
    parentBtn.setAttribute("aria-expanded", "true");

    const pRect = panel.getBoundingClientRect();
    const iRect = parentBtn.getBoundingClientRect();
    const fRect = fly.getBoundingClientRect();
    // Default: fly out to the right of the parent panel, aligned
    // with the parent item. Flip left near the right edge; on
    // narrow screens (or when neither side fits) drop below the
    // item, inside the parent's column — multi-format law.
    let left = pRect.right - 4;
    if (left + fRect.width > window.innerWidth - 8) {
      left = pRect.left - fRect.width + 4;
    }
    let top = iRect.top - 4;
    if (window.innerWidth < 560 || left < 8) {
      left = pRect.left;
      top = iRect.bottom + 2;
    }
    if (top + fRect.height > window.innerHeight - 8) {
      top = Math.max(8, window.innerHeight - fRect.height - 8);
    }
    fly.style.left = `${Math.max(8, left)}px`;
    fly.style.top = `${Math.max(8, top)}px`;
  }

  /** Close every panel deeper than `depth`, un-expanding parents. */
  private closeDeeperThan(depth: number): void {
    while (this.panels.length > depth + 1) {
      const panel = this.panels.pop()!;
      panel.remove();
      const parent = this.parents.pop();
      parent?.setAttribute("aria-expanded", "false");
    }
  }

  private onKey(e: KeyboardEvent): void {
    if (this.panels.length === 0) return;
    if (e.key === "Escape") {
      // One level at a time; the root level closes the menu.
      if (this.panels.length > 1) {
        const depth = this.panels.length - 2;
        const parent = this.parents[depth];
        this.closeDeeperThan(depth);
        parent?.focus();
      } else {
        this.close();
      }
      e.stopPropagation();
      return;
    }
    const active = document.activeElement as HTMLElement | null;
    const panel = this.panels.find((p) => active && p.contains(active));
    if (!panel || !active) return;
    const depth = Number(panel.dataset.depth ?? 0);
    const items = [
      ...panel.querySelectorAll<HTMLElement>(".ctx-item:not(:disabled)"),
    ];
    const idx = items.indexOf(active);
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const next =
        e.key === "ArrowDown"
          ? items[(idx + 1) % items.length]
          : items[(idx - 1 + items.length) % items.length];
      next?.focus();
    } else if (e.key === "ArrowRight") {
      if (active.getAttribute("aria-haspopup") === "menu") {
        e.preventDefault();
        active.click();
      }
    } else if (e.key === "ArrowLeft") {
      if (depth > 0) {
        e.preventDefault();
        const parent = this.parents[depth - 1];
        this.closeDeeperThan(depth - 1);
        parent?.focus();
      }
    }
  }

  close(): void {
    for (const p of this.panels) p.remove();
    this.panels = [];
    this.parents = [];
  }

  get isOpen(): boolean {
    return this.panels.length > 0;
  }
}
