/**
 * PrintDialog — pick a printer and copies, send the file to CUPS.
 * Office documents are converted to PDF by the backend first.
 */
import { api } from "./api";
import type { Item } from "./api";
import { Toast, errText} from "./toast";

export class PrintDialog {
  readonly element: HTMLDivElement;
  private body: HTMLDivElement;
  private item: Item | null = null;

  constructor() {
    this.element = document.createElement("div");
    this.element.className = "props-overlay";
    this.element.hidden = true;
    const dialog = document.createElement("div");
    dialog.className = "print-dialog";
    dialog.setAttribute("role", "dialog");
    dialog.setAttribute("aria-label", "Print");
    const title = document.createElement("h2");
    title.textContent = "🖨 Print";
    this.body = document.createElement("div");
    this.body.className = "print-body";
    dialog.append(title, this.body);
    this.element.appendChild(dialog);
    this.element.addEventListener("mousedown", (e) => {
      if (e.target === this.element) this.close();
    });
  }

  async open(item: Item): Promise<void> {
    this.item = item;
    this.element.hidden = false;
    this.body.textContent = "Looking for printers…";
    let printers;
    try {
      printers = await api.listPrinters();
    } catch (err) {
      this.body.textContent = `Printer search failed: ${errText(err)}`;
      return;
    }
    this.body.replaceChildren();
    if (printers.length === 0) {
      const none = document.createElement("div");
      none.className = "print-none";
      none.textContent =
        "No printers are set up on this computer yet, so there's nothing to send " +
        `“${item.name}” to. Add one in your system settings (CUPS) — it'll show up here the moment it exists.`;
      this.body.appendChild(none);
      const close = document.createElement("button");
      close.className = "pill-btn";
      close.textContent = "Close";
      close.addEventListener("click", () => this.close());
      this.body.appendChild(close);
      return;
    }
    const fileLine = document.createElement("div");
    fileLine.className = "print-file";
    fileLine.textContent = `Printing “${item.name}”`;
    const printerSel = document.createElement("select");
    printerSel.className = "print-select";
    for (const p of printers) {
      const o = document.createElement("option");
      o.value = p.name;
      o.textContent =
        p.name + (p.is_default ? " (default)" : "") + ` — ${p.state}`;
      if (p.is_default) o.selected = true;
      printerSel.appendChild(o);
    }
    const copiesInput = document.createElement("input");
    copiesInput.className = "print-copies";
    copiesInput.type = "number";
    copiesInput.min = "1";
    copiesInput.max = "99";
    copiesInput.value = "1";
    const copiesLabel = document.createElement("label");
    copiesLabel.className = "print-label";
    copiesLabel.append(document.createTextNode("Copies: "), copiesInput);
    const row = document.createElement("div");
    row.className = "print-row";
    const printBtn = document.createElement("button");
    printBtn.className = "pill-btn primary";
    printBtn.textContent = "Print";
    printBtn.addEventListener("click", async () => {
      if (!this.item) return;
      printBtn.disabled = true;
      printBtn.textContent = "Sending…";
      try {
        const job = await api.printItem(
          this.item.id,
          printerSel.value,
          Math.max(1, Math.floor(Number(copiesInput.value) || 1)),
        );
        Toast.success(job || `Sent to ${printerSel.value}.`);
        this.close();
      } catch (err) {
        Toast.error(`Print failed: ${errText(err)}`);
        printBtn.disabled = false;
        printBtn.textContent = "Print";
      }
    });
    const cancel = document.createElement("button");
    cancel.className = "pill-btn";
    cancel.textContent = "Cancel";
    cancel.addEventListener("click", () => this.close());
    row.append(printBtn, cancel);
    this.body.append(fileLine, printerSel, copiesLabel, row);
  }

  close(): void {
    this.element.hidden = true;
    this.item = null;
  }
}
