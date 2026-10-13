/**
 * Toast notifications and confirmation dialogs.
 * Replaces ugly native alert()/confirm() with styled UI.
 */

export class Toast {
  private static container: HTMLElement | null = null;

  private static ensureContainer(): HTMLElement {
    if (!this.container) {
      this.container = document.createElement("div");
      this.container.className = "toast-container";
      document.body.appendChild(this.container);
    }
    return this.container;
  }

  /** Show a toast message. Type: 'success' | 'error' | 'info' */
  static show(message: string, type: 'success' | 'error' | 'info' = 'info', duration = 3000): void {
    const container = this.ensureContainer();
    const el = document.createElement("div");
    el.className = `toast toast-${type}`;
    el.textContent = message;
    container.appendChild(el);

    // Animate in
    requestAnimationFrame(() => el.classList.add("toast-show"));

    // Auto-dismiss
    setTimeout(() => {
      el.classList.remove("toast-show");
      el.classList.add("toast-hide");
      setTimeout(() => el.remove(), 300);
    }, duration);
  }

  static success(msg: string): void { this.show(msg, 'success'); }
  static error(msg: string): void { this.show(msg, 'error'); }
  static info(msg: string): void { this.show(msg, 'info'); }
}

/**
 * Styled confirmation dialog. Returns true if confirmed, false if cancelled.
 */
export function confirmDialog(title: string, message: string, confirmLabel = "Confirm", danger = false): Promise<boolean> {
  return new Promise((resolve) => {
    const overlay = document.createElement("div");
    overlay.className = "confirm-overlay";

    const dialog = document.createElement("div");
    dialog.className = "confirm-dialog";
    dialog.setAttribute("role", "alertdialog");

    const h = document.createElement("h3");
    h.className = "confirm-title";
    h.textContent = title;
    dialog.appendChild(h);

    const p = document.createElement("p");
    p.className = "confirm-message";
    p.textContent = message;
    dialog.appendChild(p);

    const btns = document.createElement("div");
    btns.className = "confirm-buttons";

    const cancel = document.createElement("button");
    cancel.className = "confirm-btn confirm-cancel";
    cancel.textContent = "Cancel";
    cancel.addEventListener("click", () => {
      overlay.remove();
      resolve(false);
    });

    const ok = document.createElement("button");
    ok.className = "confirm-btn confirm-ok" + (danger ? " confirm-danger" : "");
    ok.textContent = confirmLabel;
    ok.addEventListener("click", () => {
      overlay.remove();
      resolve(true);
    });

    btns.append(cancel, ok);
    dialog.appendChild(btns);
    overlay.appendChild(dialog);

    overlay.addEventListener("click", (e) => {
      if (e.target === overlay) {
        overlay.remove();
        resolve(false);
      }
    });

    const esc = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        overlay.remove();
        document.removeEventListener("keydown", esc);
        resolve(false);
      }
    };
    document.addEventListener("keydown", esc);

    document.body.appendChild(overlay);
    ok.focus();
  });
}
