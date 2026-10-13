/**
 * App-styled dropdown: a shared-geometry button whose list is the
 * app's own context menu, so it always wears the active theme.
 * Native <select> popups leak the OS theme (wrong colors, wrong
 * highlight) — Dickles called the Burn Studio one "skank" and he
 * was right (2026-10-13).
 */
import { ContextMenu } from "./contextmenu";

export interface DropdownOption {
  value: string;
  label: string;
}

export class Dropdown {
  readonly el: HTMLButtonElement;
  private menu = new ContextMenu();
  private options: DropdownOption[];
  private current: string;

  constructor(
    options: DropdownOption[],
    value: string,
    private onChange: (value: string) => void,
  ) {
    this.options = options;
    this.current = value;
    this.el = document.createElement("button");
    this.el.className = "dropdown-btn";
    this.el.type = "button";
    this.syncLabel();
    this.el.addEventListener("click", () => {
      const rect = this.el.getBoundingClientRect();
      this.menu.open(
        rect.left,
        rect.bottom + 6,
        this.options.map((o) => ({
          label: (o.value === this.current ? "✓ " : "   ") + o.label,
          action: () => {
            if (o.value !== this.current) {
              this.current = o.value;
              this.syncLabel();
              this.onChange(o.value);
            }
          },
        })),
      );
    });
  }

  get value(): string {
    return this.current;
  }

  setValue(v: string): void {
    this.current = v;
    this.syncLabel();
  }

  private syncLabel(): void {
    const o = this.options.find((x) => x.value === this.current);
    this.el.textContent = (o?.label ?? this.current) + " ▾";
  }
}
