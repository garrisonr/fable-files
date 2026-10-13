/**
 * PropertiesDialog: file/folder details in a clean Apple-style sheet.
 * Shows name, kind, size, full path, dates, permissions, and for files
 * an executable toggle (same backend as Ctrl+E).
 */
import { api, formatBytes, type Item } from "./api";

export class PropertiesDialog {
  private el: HTMLElement | null = null;

  async open(item: Item): Promise<void> {
    this.close();
    let props;
    try {
      props = await api.getItemProperties(item.id);
    } catch {
      // Fall back to what the listing already knows.
      props = null;
    }

    const overlay = document.createElement("div");
    overlay.className = "props-overlay";
    overlay.addEventListener("mousedown", (e) => {
      if (e.target === overlay) this.close();
    });

    const dialog = document.createElement("div");
    dialog.className = "props-dialog";
    dialog.setAttribute("role", "dialog");
    dialog.setAttribute("aria-label", `Properties of ${item.name}`);

    const head = document.createElement("div");
    head.className = "props-head";
    const title = document.createElement("h3");
    title.className = "props-title";
    title.textContent = props?.name ?? item.name;
    const closeBtn = document.createElement("button");
    closeBtn.className = "icon-btn props-close";
    closeBtn.textContent = "✕";
    closeBtn.setAttribute("aria-label", "Close properties");
    closeBtn.addEventListener("click", () => this.close());
    head.append(title, closeBtn);
    dialog.appendChild(head);

    const rows: Array<[string, string]> = props
      ? [
          ["Kind", props.is_dir ? "Folder" : "File"],
          ["Size", props.is_dir ? `${formatBytes(props.size_bytes)} (contents)` : formatBytes(props.size_bytes)],
          ["Where", props.path],
          ["Created", props.created],
          ["Modified", props.modified],
          ["Accessed", props.accessed],
          ["Permissions", `${props.permissions}  (${props.mode_octal})`],
        ]
      : [
          ["Kind", item.kind === "folder" ? "Folder" : "File"],
          ["Size", item.kind === "folder" ? "—" : formatBytes(item.size_bytes)],
          ["Where", item.provider_key ?? "—"],
        ];

    const table = document.createElement("div");
    table.className = "props-rows";
    for (const [k, v] of rows) {
      const row = document.createElement("div");
      row.className = "props-row";
      const key = document.createElement("span");
      key.className = "props-key";
      key.textContent = k;
      const val = document.createElement("span");
      val.className = "props-val";
      val.textContent = v;
      if (k === "Where") val.title = v;
      row.append(key, val);
      table.appendChild(row);
    }
    dialog.appendChild(table);

    // Executable toggle for files (mirrors Make Executable).
    if (props && !props.is_dir) {
      const label = document.createElement("label");
      label.className = "props-exec";
      const check = document.createElement("input");
      check.type = "checkbox";
      check.checked = props.executable;
      check.addEventListener("change", () => {
        api.setExecutable(item.id, check.checked).catch(() => {
          check.checked = !check.checked;
        });
      });
      label.append(check, document.createTextNode(" Allow executing file as program"));
      dialog.appendChild(label);
    }

    overlay.appendChild(dialog);
    document.body.appendChild(overlay);
    this.el = overlay;
    closeBtn.focus();
  }

  close(): void {
    if (this.el) {
      this.el.remove();
      this.el = null;
    }
  }
}
