/**
 * AppDeleter — uninstall an app properly: remove the package, then
 * find its leftovers (config, data, cache, state, Flatpak data)
 * and choose: delete the preferences too, or keep your files.
 * Leftovers go to the Trash, never straight to /dev/null.
 */
import { api } from "./api";
import { Toast } from "./toast";

interface AppEntry {
  name: string;
  id: string;
  source: string;
  desktop_path: string;
}

function fmt(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
  return `${(bytes / 1024 ** 3).toFixed(2)} GB`;
}

const SOURCE_BADGE: Record<string, string> = {
  system: "📦 system",
  user: "👤 local",
  flatpak: "🫙 flatpak",
};

export class AppDeleter {
  readonly element: HTMLDivElement;
  private searchInput: HTMLInputElement;
  private listEl: HTMLDivElement;
  private detailEl: HTMLDivElement;
  private apps: AppEntry[] = [];
  private selected: AppEntry | null = null;
  private checks = new Map<string, HTMLInputElement>();
  private armed = false;
  private onDone: (() => void) | null = null;

  constructor() {
    this.element = document.createElement("div");
    this.element.className = "props-overlay";
    this.element.hidden = true;
    const dialog = document.createElement("div");
    dialog.className = "deleter-dialog";
    dialog.setAttribute("role", "dialog");
    dialog.setAttribute("aria-label", "App Deleter");
    const title = document.createElement("h2");
    title.className = "deleter-title";
    title.textContent = "🗑 App Deleter";
    const sub = document.createElement("p");
    sub.className = "deleter-sub";
    sub.textContent =
      "Uninstalls the app AND hunts down what it leaves behind. Leftovers go to the Trash — recoverable, never vaporized.";
    this.searchInput = document.createElement("input");
    this.searchInput.className = "deleter-search";
    this.searchInput.placeholder = "Search installed apps…";
    this.searchInput.addEventListener("input", () => this.renderList());
    const cols = document.createElement("div");
    cols.className = "deleter-cols";
    this.listEl = document.createElement("div");
    this.listEl.className = "deleter-list";
    this.detailEl = document.createElement("div");
    this.detailEl.className = "deleter-detail";
    this.detailEl.textContent = "Pick an app on the left.";
    cols.append(this.listEl, this.detailEl);
    const foot = document.createElement("div");
    foot.className = "deleter-foot";
    const close = document.createElement("button");
    close.className = "pill-btn";
    close.textContent = "Close";
    close.addEventListener("click", () => this.close());
    foot.appendChild(close);
    dialog.append(title, sub, this.searchInput, cols, foot);
    this.element.appendChild(dialog);
    this.element.addEventListener("mousedown", (e) => {
      if (e.target === this.element) this.close();
    });
  }

  async open(onDone: () => void): Promise<void> {
    this.onDone = onDone;
    this.element.hidden = false;
    this.listEl.textContent = "Loading apps…";
    try {
      this.apps = await api.listInstalledApps();
    } catch (err) {
      Toast.error(`Could not list apps: ${err}`);
      this.apps = [];
    }
    this.renderList();
  }

  close(): void {
    this.element.hidden = true;
    this.armed = false;
  }

  private renderList(): void {
    const q = this.searchInput.value.trim().toLowerCase();
    this.listEl.replaceChildren();
    for (const app of this.apps) {
      if (q && !app.name.toLowerCase().includes(q)) continue;
      const row = document.createElement("button");
      row.className =
        "deleter-app" + (this.selected?.id === app.id ? " active" : "");
      row.textContent = `${app.name}  ${SOURCE_BADGE[app.source] ?? app.source}`;
      row.addEventListener("click", () => void this.select(app));
      this.listEl.appendChild(row);
    }
  }

  private async select(app: AppEntry): Promise<void> {
    this.selected = app;
    this.armed = false;
    this.renderList();
    this.detailEl.replaceChildren();
    const loading = document.createElement("div");
    loading.textContent = `Hunting leftovers of ${app.name}…`;
    this.detailEl.appendChild(loading);
    let leftovers;
    try {
      leftovers = await api.findAppLeftovers(app.id, app.name);
    } catch (err) {
      loading.textContent = `Leftover hunt failed: ${err}`;
      return;
    }
    this.checks.clear();
    this.detailEl.replaceChildren();
    const head = document.createElement("div");
    head.className = "deleter-apphead";
    head.textContent = `${app.name} — ${app.id}`;
    this.detailEl.appendChild(head);

    const modeRow = document.createElement("div");
    modeRow.className = "deleter-modes";
    const keepRadio = document.createElement("input");
    keepRadio.type = "radio";
    keepRadio.name = "deleter-mode";
    keepRadio.checked = true;
    const keepLabel = document.createElement("label");
    keepLabel.append(keepRadio, document.createTextNode(" Keep my files (uninstall the app only)"));
    const wipeRadio = document.createElement("input");
    wipeRadio.type = "radio";
    wipeRadio.name = "deleter-mode";
    const wipeLabel = document.createElement("label");
    wipeLabel.append(wipeRadio, document.createTextNode(" Also delete preferences & related files (checked below → Trash)"));
    modeRow.append(keepLabel, wipeLabel);
    this.detailEl.appendChild(modeRow);

    const list = document.createElement("div");
    list.className = "deleter-leftovers";
    let total = 0;
    if (leftovers.length === 0) {
      const none = document.createElement("div");
      none.textContent = "No leftovers found — a clean app. Suspicious. Admirable.";
      list.appendChild(none);
    }
    for (const l of leftovers) {
      total += l.size;
      const row = document.createElement("label");
      row.className = "deleter-leftover";
      const chk = document.createElement("input");
      chk.type = "checkbox";
      chk.checked = true;
      chk.disabled = true; // enabled only in wipe mode
      this.checks.set(l.path, chk);
      const text = document.createElement("span");
      text.textContent = `${l.path} · ${fmt(l.size)} · ${l.category}`;
      row.append(chk, text);
      list.appendChild(row);
    }
    this.detailEl.appendChild(list);
    const syncChecks = () => {
      for (const chk of this.checks.values()) chk.disabled = !wipeRadio.checked;
    };
    keepRadio.addEventListener("change", syncChecks);
    wipeRadio.addEventListener("change", syncChecks);

    const totalEl = document.createElement("div");
    totalEl.className = "deleter-total";
    totalEl.textContent =
      leftovers.length > 0
        ? `Leftovers: ${fmt(total)} across ${leftovers.length} location(s).`
        : "";
    this.detailEl.appendChild(totalEl);

    const btn = document.createElement("button");
    btn.className = "pill-btn danger";
    btn.textContent = `Uninstall ${app.name}`;
    btn.addEventListener("click", async () => {
      if (!this.armed) {
        this.armed = true;
        btn.textContent = `⚠ Click again to really uninstall ${app.name}`;
        return;
      }
      this.armed = false;
      const chosen = wipeRadio.checked
        ? [...this.checks.entries()]
            .filter(([, c]) => c.checked)
            .map(([p]) => p)
        : [];
      btn.disabled = true;
      btn.textContent = "Uninstalling…";
      try {
        const summary = await api.uninstallApp(app, chosen);
        Toast.success(`Removed ${app.name}: ${summary}`);
        this.apps = this.apps.filter((a) => a.id !== app.id);
        this.selected = null;
        this.detailEl.textContent = "Pick an app on the left.";
        this.renderList();
        this.onDone?.();
      } catch (err) {
        Toast.error(`Uninstall failed: ${err}`);
        btn.disabled = false;
        btn.textContent = `Uninstall ${app.name}`;
      }
    });
    this.detailEl.appendChild(btn);
    const note = document.createElement("div");
    note.className = "deleter-note";
    note.textContent =
      app.source === "system"
        ? "System apps ask for your admin password (pacman)."
        : app.source === "flatpak"
          ? "Flatpak apps remove cleanly; their data dir is in the leftover list."
          : "Local launchers just lose their menu entry.";
    this.detailEl.appendChild(note);
  }
}
