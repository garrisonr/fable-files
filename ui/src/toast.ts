/**
 * Toast notifications and confirmation dialogs.
 * Replaces ugly native alert()/confirm() with styled UI.
 */

/** Render any thrown value as human text for a toast. Tauri command
 * errors arrive as CommandError objects ({code, message}) — a bare
 * `${err}` stringifies those as "[object Object]" (Dickles hit this
 * on Trash and Clipboard, 2026-10-14). Every user-visible error
 * interpolation goes through here. */
export function errText(err: unknown): string {
  if (typeof err === "string") return err;
  if (err instanceof Error) return err.message;
  if (err && typeof err === "object") {
    const o = err as { message?: unknown; code?: unknown };
    if (typeof o.message === "string" && o.message) return o.message;
    if (typeof o.code === "string" && o.code) return o.code;
    try {
      return JSON.stringify(err);
    } catch {
      return "Unknown error";
    }
  }
  if (err === null || err === undefined) return "Unknown error";
  return String(err);
}

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

  static success(msg: string, duration?: number): void { this.show(msg, 'success', duration); }
  static error(msg: string, duration?: number): void { this.show(msg, 'error', duration); }
  static info(msg: string, duration?: number): void { this.show(msg, 'info', duration); }
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

/** One-field text prompt in the confirmDialog style. Resolves to the
 * trimmed value, or null on cancel. */
export function promptDialog(
  title: string,
  placeholder: string,
  initial = "",
): Promise<string | null> {
  return new Promise((resolve) => {
    const overlay = document.createElement("div");
    overlay.className = "confirm-overlay";
    overlay.innerHTML = `
      <div class="confirm-card" role="dialog" aria-label="${title}">
        <h3></h3>
        <input class="prompt-input" type="text" spellcheck="false" />
        <div class="confirm-actions">
          <button class="confirm-cancel">Cancel</button>
          <button class="confirm-ok primary">OK</button>
        </div>
      </div>`;
    overlay.querySelector("h3")!.textContent = title;
    const input = overlay.querySelector(".prompt-input") as HTMLInputElement;
    input.placeholder = placeholder;
    input.value = initial;
    document.body.appendChild(overlay);
    input.focus();
    input.select();
    const done = (v: string | null) => {
      overlay.remove();
      resolve(v);
    };
    overlay
      .querySelector(".confirm-cancel")!
      .addEventListener("click", () => done(null));
    overlay
      .querySelector(".confirm-ok")!
      .addEventListener("click", () => done(input.value.trim() || null));
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter") done(input.value.trim() || null);
      if (e.key === "Escape") done(null);
      e.stopPropagation();
    });
    overlay.addEventListener("pointerdown", (e) => {
      if (e.target === overlay) done(null);
    });
  });
}
