/**
 * OpenWithDialog: choose an application to open a file.
 * Lists apps that handle the file's MIME type, with an
 * "Always use for this file type" option.
 */
import { api } from "./api";
import type { Item } from "./api";

export class OpenWithDialog {
  private el: HTMLElement | null = null;

  async open(item: Item): Promise<void> {
    this.close();

    // Fetch apps
    let info;
    try {
      info = await api.openWithApps(item.id);
    } catch (err) {
      console.error("Failed to get open-with apps:", err);
      return;
    }

    const overlay = document.createElement("div");
    overlay.className = "ow-overlay";

    const dialog = document.createElement("div");
    dialog.className = "ow-dialog";
    dialog.setAttribute("role", "dialog");
    dialog.setAttribute("aria-label", `Open ${item.name} with`);

    const title = document.createElement("h3");
    title.className = "ow-title";
    title.textContent = `Open "${item.name}" with`;
    dialog.appendChild(title);

    const mime = document.createElement("p");
    mime.className = "ow-mime";
    mime.textContent = info.mime;
    dialog.appendChild(mime);

    const list = document.createElement("div");
    list.className = "ow-list";

    if (info.apps.length === 0) {
      const empty = document.createElement("p");
      empty.className = "ow-empty";
      empty.textContent = "No applications found for this file type.";
      list.appendChild(empty);
    }

    // "Always use" checkbox
    const alwaysWrap = document.createElement("label");
    alwaysWrap.className = "ow-always";
    const alwaysCheck = document.createElement("input");
    alwaysCheck.type = "checkbox";
    alwaysWrap.appendChild(alwaysCheck);
    alwaysWrap.appendChild(document.createTextNode(" Always use for this file type"));

    // Phase 6: EPUBs can open in the built-in reader. It is not a
    // system app, so the "Always use" checkbox doesn't apply to it.
    // PDF Reader build: PDFs get the same entry, routed to the
    // paginated reader.
    const lowerItemName = item.name.toLowerCase();
    if (lowerItemName.endsWith(".epub") || lowerItemName.endsWith(".pdf")) {
      const b = document.createElement("button");
      b.className = "ow-app";
      const name = document.createElement("span");
      name.className = "ow-app-name";
      name.textContent = "Fable Reader";
      b.appendChild(name);
      const badge = document.createElement("span");
      badge.className = "ow-badge";
      badge.textContent = "Built-in";
      b.appendChild(badge);
      b.addEventListener("click", () => {
        void (async () => {
          const path =
            item.provider_key ??
            (await api.getItemPath(item.id).catch(() => null));
          this.close();
          if (!path) return;
          if (lowerItemName.endsWith(".pdf")) {
            const m = await import("./pdfreader");
            m.openPdfReader(path);
          } else {
            const m = await import("./reader");
            m.openReader(path);
          }
        })();
      });
      list.appendChild(b);
    }

    for (const app of info.apps) {
      const b = document.createElement("button");
      b.className = "ow-app";
      if (app.id === info.default) b.classList.add("ow-default");

      const name = document.createElement("span");
      name.className = "ow-app-name";
      name.textContent = app.name;
      b.appendChild(name);

      if (app.id === info.default) {
        const badge = document.createElement("span");
        badge.className = "ow-badge";
        badge.textContent = "Default";
        b.appendChild(badge);
      }

      b.addEventListener("click", async () => {
        try {
          if (alwaysCheck.checked) {
            await api.setDefaultApp(info.mime, app.id);
          }
          await api.openWithApp(item.id, app.id);
        } catch (err) {
          console.error("Open with failed:", err);
        }
        this.close();
      });
      list.appendChild(b);
    }

    dialog.appendChild(list);
    dialog.appendChild(alwaysWrap);

    const closeBtn = document.createElement("button");
    closeBtn.className = "ow-close";
    closeBtn.textContent = "Cancel";
    closeBtn.addEventListener("click", () => this.close());
    dialog.appendChild(closeBtn);

    overlay.appendChild(dialog);
    overlay.addEventListener("click", (e) => {
      if (e.target === overlay) this.close();
    });
    document.addEventListener("keydown", this.escHandler);

    document.body.appendChild(overlay);
    this.el = overlay;
  }

  private escHandler = (e: KeyboardEvent) => {
    if (e.key === "Escape") this.close();
  };

  close(): void {
    if (this.el) {
      this.el.remove();
      this.el = null;
      document.removeEventListener("keydown", this.escHandler);
    }
  }
}
