/**
 * LibraryMemorySection — Preferences ▸ Library Memory (Indexer v2).
 *
 * Dickles' ruling: "Fable should remember past drives until you
 * tell it to forget." This section is where he tells it: how long
 * disconnected drives are remembered (global default + per-drive
 * override), which drives are remembered, and the two deliberate
 * acts — Forget this drive, and Rebuild (from scratch) for a drive
 * that's here. The section owns no storage: settings and volume
 * records live in the backend (library-settings.json + the
 * catalog's volumes table); every control writes through and
 * re-reads, so what's on screen is what's in force.
 */
import { api, type LibrarySettings, type VolumeSummary } from "./api";
import { Toast, errText } from "./toast";
import { fmtDay } from "./search";

export interface LibraryHooks {
  /** Purge + re-index one connected volume (the shell owns the
   *  location registry and the index tracker). */
  rebuildVolume(fsUuid: string): Promise<void>;
}

const WINDOWS: Array<{ days: number | null; label: string }> = [
  { days: null, label: "Forever" },
  { days: 365, label: "1 year" },
  { days: 180, label: "6 months" },
  { days: 90, label: "90 days" },
  { days: 60, label: "60 days" },
  { days: 30, label: "30 days" },
];

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  e.className = className;
  if (text !== undefined) e.textContent = text;
  return e;
}

function windowLabel(days: number | null): string {
  return WINDOWS.find((w) => w.days === days)?.label ?? `${days} days`;
}

export class LibraryMemorySection {
  private hooks: LibraryHooks | null = null;
  private body: HTMLElement | null = null;
  private settings: LibrarySettings | null = null;
  private volumes: VolumeSummary[] = [];
  /** fs_uuid whose Forget button is armed (two-step confirm). */
  private forgetArmed: string | null = null;
  private forgetTimer = 0;

  async mount(host: HTMLElement, hooks: LibraryHooks): Promise<void> {
    this.hooks = hooks;
    host.appendChild(el("h3", "prefs-section", "Library Memory"));
    host.appendChild(
      el(
        "p",
        "prefs-note",
        "Fable remembers drives you've unplugged — their files stay searchable, marked with the drive they live on, until you forget them or their memory expires. A connected drive is never forgotten.",
      ),
    );
    this.body = el("div", "lib-body");
    this.body.appendChild(el("p", "prefs-note", "Loading…"));
    host.appendChild(this.body);
    await this.refresh();
  }

  private async refresh(): Promise<void> {
    try {
      const [settings, volumes] = await Promise.all([
        api.getLibrarySettings(),
        api.listVolumes(),
      ]);
      this.settings = settings;
      this.volumes = volumes;
      this.render();
    } catch (err) {
      if (this.body) {
        this.body.innerHTML = "";
        this.body.appendChild(
          el("p", "prefs-note", `Library memory could not load: ${errText(err)}`),
        );
      }
    }
  }

  private async saveSettings(next: LibrarySettings): Promise<void> {
    try {
      this.settings = await api.setLibrarySettings(next);
      // Retention drives what's prunable — re-read the volumes too.
      this.volumes = await api.listVolumes();
      this.render();
    } catch (err) {
      Toast.error(`Could not save library settings: ${errText(err)}`);
    }
  }

  private render(): void {
    const body = this.body;
    const settings = this.settings;
    if (!body || !settings) return;
    body.innerHTML = "";

    // --- Global retention ------------------------------------------
    const globalRow = el("div", "prefs-row lib-global-row");
    const lab = el("span", "prefs-row-label", "Remember disconnected drives");
    const select = document.createElement("select");
    select.className = "lib-select";
    select.setAttribute("aria-label", "Remember disconnected drives");
    for (const w of WINDOWS) {
      const opt = document.createElement("option");
      opt.value = w.days === null ? "" : String(w.days);
      opt.textContent = w.label;
      opt.selected = settings.retention_days === w.days;
      select.appendChild(opt);
    }
    select.addEventListener("change", () => {
      const days = select.value === "" ? null : Number(select.value);
      void this.saveSettings({ ...settings, retention_days: days });
    });
    globalRow.append(lab, select);
    body.appendChild(globalRow);

    // --- Remembered volumes ----------------------------------------
    if (this.volumes.length === 0) {
      body.appendChild(
        el(
          "p",
          "prefs-note",
          "No drives remembered yet — index a drive and Fable will remember it here.",
        ),
      );
      return;
    }
    for (const v of this.volumes) {
      body.appendChild(this.volumeRow(v, settings));
    }
  }

  private volumeRow(v: VolumeSummary, settings: LibrarySettings): HTMLElement {
    const wrap = el("div", "lib-vol");

    const head = el("div", "lib-vol-head");
    head.appendChild(el("span", "lib-vol-name", v.label));
    const state = el(
      "span",
      `lib-state ${v.online ? "on" : "off"}`,
      v.online ? "Connected" : "Not connected",
    );
    head.appendChild(state);
    if (v.is_system) head.appendChild(el("span", "lib-state sys", "Main drive"));
    wrap.appendChild(head);

    const count = v.item_count.toLocaleString();
    const seen = fmtDay(v.last_seen);
    const meta = v.online
      ? `${count} file${v.item_count === 1 ? "" : "s"} remembered · at ${v.mount_point ?? ""}`
      : `${count} file${v.item_count === 1 ? "" : "s"} remembered · last seen ${seen}`;
    wrap.appendChild(el("p", "prefs-note lib-vol-meta", meta));

    const controls = el("div", "lib-vol-controls");

    // Per-volume retention override.
    const override = document.createElement("select");
    override.className = "lib-select";
    override.setAttribute("aria-label", `Memory for ${v.label}`);
    const inherit = document.createElement("option");
    inherit.value = "inherit";
    inherit.textContent = `Use default (${windowLabel(settings.retention_days)})`;
    inherit.selected = !v.has_override;
    override.appendChild(inherit);
    for (const w of WINDOWS) {
      const opt = document.createElement("option");
      opt.value = w.days === null ? "forever" : String(w.days);
      opt.textContent =
        w.days === null ? "Remember forever" : `Forget after ${w.label}`;
      opt.selected = v.has_override && v.override_days === w.days;
      override.appendChild(opt);
    }
    override.addEventListener("change", () => {
      const overrides = { ...settings.overrides };
      if (override.value === "inherit") delete overrides[v.fs_uuid];
      else if (override.value === "forever") overrides[v.fs_uuid] = null;
      else overrides[v.fs_uuid] = Number(override.value);
      void this.saveSettings({ ...settings, overrides });
    });
    controls.appendChild(override);

    if (v.online) {
      const rebuild = el("button", "pill-btn", "Rebuild index…") as HTMLButtonElement;
      rebuild.title = "Forget this drive's index and rebuild it from scratch";
      rebuild.addEventListener("click", () => {
        rebuild.disabled = true;
        rebuild.textContent = "Rebuilding…";
        void this.hooks
          ?.rebuildVolume(v.fs_uuid)
          .then(() => {
            Toast.success(`Rebuilding the index for “${v.label}”.`);
          })
          .catch((err) => {
            Toast.error(`Could not rebuild: ${errText(err)}`);
          })
          .finally(() => void this.refresh());
      });
      controls.appendChild(rebuild);
    }

    if (!v.online && !v.is_system) {
      const armed = this.forgetArmed === v.fs_uuid;
      const forget = el(
        "button",
        "pill-btn lib-forget" + (armed ? " armed" : ""),
        armed
          ? `Forget ${v.item_count.toLocaleString()} files? Click again to confirm`
          : "Forget this drive…",
      ) as HTMLButtonElement;
      forget.addEventListener("click", () => {
        if (this.forgetArmed !== v.fs_uuid) {
          this.forgetArmed = v.fs_uuid;
          window.clearTimeout(this.forgetTimer);
          this.forgetTimer = window.setTimeout(() => {
            this.forgetArmed = null;
            this.render();
          }, 5000);
          this.render();
          return;
        }
        window.clearTimeout(this.forgetTimer);
        this.forgetArmed = null;
        forget.disabled = true;
        void api
          .forgetVolume(v.fs_uuid)
          .then((purged) => {
            Toast.success(
              `Forgot “${v.label}” — ${purged.toLocaleString()} files removed from memory.`,
            );
          })
          .catch((err) => Toast.error(`Could not forget: ${errText(err)}`))
          .finally(() => void this.refresh());
      });
      controls.appendChild(forget);
    }

    wrap.appendChild(controls);
    return wrap;
  }
}
