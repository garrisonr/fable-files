import { Toast, errText } from "./toast";
/**
 * FolderPicker — in-app destination chooser for Move to… / Copy to… /
 * Extract to… / save dialogs. Resolves to an absolute path.
 *
 * The rail mirrors the main sidebar (Dickles, 2026-10-15):
 *  - FAVORITES: the built-ins (Home, Documents, Downloads, Pictures,
 *    Music) resolved the same way the shell resolves them, plus the
 *    user's own favorites.
 *  - PLACES: the locations/shortcuts the caller passes (or the
 *    location list when the caller passes none).
 *  - SMART FOLDERS: the rule folders (Audiobooks, Music, Movies,
 *    Documents, Spreadsheets, …). The ROMs collection is NOT here —
 *    it's a separate sidebar section, not a smart folder, and its
 *    60 systems would swamp a picker. A smart folder is a view, not
 *    a place, so picking one lists the real folders that CONTAIN
 *    matching files; drilling into one browses it for real and
 *    Choose resolves its real path.
 * A New Folder button creates a folder in the current directory
 * (inline name prompt; the existing create_folder command), renames
 * it to the typed name, and lands inside it — standard dialog
 * behavior.
 */
import { api, type LocationSummary } from "./api";

export interface PickerShortcut {
  label: string;
  locationId: string;
  dirId: string;
}

interface RailPlace extends PickerShortcut {
  path?: string;
}

interface SmartFolderDef {
  name: string;
  rule?: { extensions: string[]; minDurationSecs?: number | null } | null;
}

const BUILTIN_FAVORITES = ["Home", "Documents", "Downloads", "Pictures", "Music"];
const FUZZY_FAVORITES: Record<string, string[]> = {
  Pictures: ["Pictures", "Photos", "Images", "Pics"],
  Music: ["Music", "Audio", "Songs"],
};

/** Smart-folder storage mirror (the shell owns the originals in
 * localStorage "ff-smart-folders", seeded with these defaults). The
 * picker only reads — it never writes the list. Rule folders only:
 * saved free-text searches are searches, not places. */
function loadPickerSmartFolders(): SmartFolderDef[] {
  try {
    const raw = localStorage.getItem("ff-smart-folders");
    if (raw) {
      const parsed = JSON.parse(raw) as SmartFolderDef[];
      if (Array.isArray(parsed)) {
        return parsed.filter((f) => f && typeof f.name === "string" && !!f.rule);
      }
    }
  } catch {
    // fall through to the seeded defaults
  }
  return [
    { name: "Audiobooks", rule: { extensions: ["m4b"], minDurationSecs: null } },
    {
      name: "Music",
      rule: {
        extensions: ["mp3", "aac", "flac", "ogg", "oga", "opus", "wav", "m4a", "aif", "aiff"],
        minDurationSecs: 30,
      },
    },
    {
      name: "Movies",
      rule: {
        extensions: ["mkv", "mp4", "avi", "mov", "m4v", "webm", "mpg", "mpeg", "wmv"],
        minDurationSecs: 70 * 60,
      },
    },
    {
      name: "Documents",
      rule: {
        extensions: ["doc", "docx", "odt", "ods", "odp", "pdf", "rtf", "xls", "xlsx", "ppt", "pptx"],
        minDurationSecs: null,
      },
    },
    {
      name: "Spreadsheets",
      rule: { extensions: ["csv", "tsv", "xls", "xlsx", "ods", "numbers"], minDurationSecs: null },
    },
  ];
}

function normPath(p: string): string {
  return p.replace(/\/+$/, "") || "/";
}

export class FolderPicker {
  private el: HTMLDivElement;
  private resolveFn: ((path: string | null) => void) | null = null;
  private currentPath = "";
  private currentLocationId = "";
  private currentDirId = "";
  private history: { locationId: string; dirId: string }[] = [];
  private locations: LocationSummary[] = [];
  /** True while the list shows a smart folder's containing folders —
   * Choose stays disabled until a real folder is browsed. */
  private inSmart = false;

  constructor() {
    this.el = document.createElement("div");
    this.el.className = "picker-overlay";
    this.el.hidden = true;
    this.el.innerHTML = `
      <div class="picker-card" role="dialog" aria-label="Choose a folder">
        <h3 class="picker-title">Choose a folder</h3>
        <div class="picker-main">
          <div class="picker-shortcuts"></div>
          <div class="picker-list"></div>
        </div>
        <div class="picker-path"></div>
        <div class="picker-newrow" hidden>
          <input class="picker-newname" type="text" spellcheck="false" placeholder="Folder name" />
          <button class="picker-newcreate primary">Create</button>
          <button class="picker-newcancel">Cancel</button>
        </div>
        <div class="picker-actions">
          <button class="picker-newfolder">New Folder</button>
          <button class="picker-cancel">Cancel</button>
          <button class="picker-choose primary">Choose this folder</button>
        </div>
      </div>`;
    this.el
      .querySelector(".picker-cancel")!
      .addEventListener("click", () => this.close(null));
    this.el
      .querySelector(".picker-choose")!
      .addEventListener("click", () => {
        if (!this.inSmart) this.close(this.currentPath || null);
      });
    this.el
      .querySelector(".picker-newfolder")!
      .addEventListener("click", () => this.openNewFolder());
    this.el
      .querySelector(".picker-newcancel")!
      .addEventListener("click", () => this.closeNewFolder());
    this.el
      .querySelector(".picker-newcreate")!
      .addEventListener("click", () => void this.createNewFolder());
    this.el
      .querySelector<HTMLInputElement>(".picker-newname")!
      .addEventListener("keydown", (e) => {
        if (e.key === "Enter") void this.createNewFolder();
        if (e.key === "Escape") this.closeNewFolder();
        e.stopPropagation();
      });
    this.el.addEventListener("pointerdown", (e) => {
      if (e.target === this.el) this.close(null);
    });
  }

  get element(): HTMLElement {
    return this.el;
  }

  /** Show the picker; resolves to the chosen absolute path or null.
   * With a null start (and/or no shortcuts) it helps itself to the
   * location list: dialogs don't have to know the shell's state. */
  async pick(
    title: string,
    shortcuts: PickerShortcut[],
    start: { locationId: string; dirId: string } | null,
  ): Promise<string | null> {
    try {
      this.locations = await api.listLocations();
    } catch {
      this.locations = [];
    }
    if (shortcuts.length === 0) {
      shortcuts = this.locations.map((l) => ({
        label: l.display_name,
        locationId: l.id,
        dirId: l.root_item,
      }));
    }
    if (!start) {
      const home =
        this.locations.find((l) =>
          l.display_name.toLowerCase().includes("home"),
        ) ?? this.locations[0];
      if (home) start = { locationId: home.id, dirId: home.root_item };
    }
    if (!start) return null;
    this.el.querySelector(".picker-title")!.textContent = title;
    await this.buildRail(shortcuts);
    this.history = [];
    this.inSmart = false;
    this.closeNewFolder();
    this.el.hidden = false;
    await this.browse(start.locationId, start.dirId);
    return new Promise((resolve) => {
      this.resolveFn = resolve;
    });
  }

  /** Build the places rail: Favorites / Places / Smart Folders. */
  private async buildRail(shortcuts: PickerShortcut[]): Promise<void> {
    const sc = this.el.querySelector(".picker-shortcuts")!;
    sc.innerHTML = "";
    const key = (s: PickerShortcut) => `${s.locationId}|${s.dirId}`;
    const seen = new Set<string>();
    const section = (label: string) => {
      const h = document.createElement("div");
      h.className = "picker-rail-head";
      h.textContent = label;
      sc.appendChild(h);
    };
    const place = (label: string, target: PickerShortcut) => {
      if (seen.has(key(target))) return;
      seen.add(key(target));
      const b = document.createElement("button");
      b.className = "picker-shortcut";
      b.textContent = `☆ ${label}`;
      b.addEventListener("click", () => {
        this.history = [];
        void this.browse(target.locationId, target.dirId);
      });
      sc.appendChild(b);
    };

    // Favorites first — built-ins resolved like the shell resolves
    // them, then the user's own favorites by path.
    const favorites = await this.resolveFavorites();
    if (favorites.length > 0) {
      section("Favorites");
      for (const f of favorites) place(f.label, f);
    }
    if (shortcuts.length > 0) {
      section("Places");
      for (const s of shortcuts) place(s.label, s);
    }
    const smart = loadPickerSmartFolders();
    if (smart.length > 0) {
      section("Smart Folders");
      for (const sf of smart) {
        const b = document.createElement("button");
        b.className = "picker-shortcut";
        b.textContent = `☆ ${sf.name}`;
        b.addEventListener("click", () => void this.browseSmart(sf));
        sc.appendChild(b);
      }
    }
  }

  /** Built-in favorites (the shell's resolution, mirrored): a
   * location named like the favorite wins; otherwise a same-named
   * folder inside Home. Plus user favorites (ui prefs, localStorage
   * mirror fallback) resolved by path. */
  private async resolveFavorites(): Promise<RailPlace[]> {
    const out: RailPlace[] = [];
    const locs = this.locations;
    if (locs.length === 0) return out;
    const homeLoc =
      locs.find((l) => l.display_name.toLowerCase().includes("home")) ??
      locs.find((l) => l.provider === "local") ??
      locs[0];
    for (const name of BUILTIN_FAVORITES) {
      const want = name.toLowerCase();
      const direct =
        locs.find((l) => l.display_name.toLowerCase().includes(want)) ??
        locs.find((l) => l.provider.toLowerCase().includes(want));
      if (direct) {
        out.push({
          label: name,
          locationId: direct.id,
          dirId: direct.root_item,
        });
        continue;
      }
      if (homeLoc && name !== "Home") {
        try {
          const kids = await api.listDir(homeLoc.id, homeLoc.root_item);
          const candidates = FUZZY_FAVORITES[name] ?? [name];
          const kid = kids.find(
            (k) =>
              k.kind === "folder" &&
              candidates.some((c) => k.name.toLowerCase() === c.toLowerCase()),
          );
          if (kid) {
            out.push({ label: name, locationId: homeLoc.id, dirId: kid.id });
          }
        } catch {
          // favorite simply doesn't resolve on this machine
        }
      }
    }
    // User favorites: durable ui prefs first, localStorage mirror
    // second — the shell's own read order.
    let userFavs: { name: string; path: string }[] = [];
    try {
      const prefs = await api.getUiPrefs();
      const raw = prefs["ff-user-favorites"];
      if (Array.isArray(raw)) {
        userFavs = raw.filter(
          (f): f is { name: string; path: string } =>
            !!f &&
            typeof f === "object" &&
            typeof (f as { name?: unknown }).name === "string" &&
            typeof (f as { path?: unknown }).path === "string",
        );
      }
    } catch {
      // prefs unreachable — try the mirror
    }
    if (userFavs.length === 0) {
      try {
        const ls = localStorage.getItem("ff-user-favorites");
        if (ls) userFavs = JSON.parse(ls) as { name: string; path: string }[];
      } catch {
        userFavs = [];
      }
    }
    for (const f of userFavs) {
      const target = await this.resolvePathTarget(f.path);
      if (target) out.push({ ...target, label: f.name });
    }
    return out;
  }

  /** Resolve an absolute path to a browsable (location, dir) by
   * walking location roots segment by segment — the shell's
   * resolvePathTarget, mirrored for the picker. */
  private async resolvePathTarget(
    path: string,
  ): Promise<PickerShortcut | null> {
    const n = normPath(path);
    for (const loc of this.locations) {
      const root = await api.getItemPath(loc.root_item).catch(() => "");
      if (!root) continue;
      const rn = normPath(root);
      if (n === rn) {
        return { label: loc.display_name, locationId: loc.id, dirId: loc.root_item };
      }
      const prefix = rn === "/" ? "/" : `${rn}/`;
      if (!n.startsWith(prefix)) continue;
      const rel = n.slice(prefix.length).split("/").filter(Boolean);
      let dirId = loc.root_item;
      let ok = true;
      for (const seg of rel) {
        const kids = await api.listDir(loc.id, dirId).catch(() => null);
        const kid = kids?.find((k) => k.kind === "folder" && k.name === seg);
        if (!kid) {
          ok = false;
          break;
        }
        dirId = kid.id;
      }
      if (ok) return { label: rel[rel.length - 1] ?? loc.display_name, locationId: loc.id, dirId };
    }
    return null;
  }

  /** Smart folder view: the real folders containing matching files.
   * Choose stays disabled here — a smart folder is a view; the
   * destination must be one of the real folders inside it. */
  private async browseSmart(sf: SmartFolderDef): Promise<void> {
    if (!sf.rule) return;
    this.inSmart = true;
    this.syncActions();
    const list = this.el.querySelector(".picker-list")!;
    list.innerHTML = "";
    this.el.querySelector(".picker-path")!.textContent =
      `Smart Folder: ${sf.name} — folders containing matching files`;
    const status = document.createElement("div");
    status.className = "picker-empty";
    status.textContent = `Gathering “${sf.name}”…`;
    list.appendChild(status);
    try {
      const parents = new Map<string, { locationId: string; path: string }>();
      for (let pass = 0; pass < 40; pass++) {
        const res = await api.ruleItems(
          sf.rule.extensions,
          sf.rule.minDurationSecs ?? null,
          1000,
          250,
        );
        for (const item of res.items) {
          const p = item.provider_key;
          if (!p) continue;
          const parent = p.slice(0, p.lastIndexOf("/")) || "/";
          const k = `${item.location}|${parent}`;
          if (!parents.has(k)) {
            parents.set(k, { locationId: item.location, path: parent });
          }
        }
        if (res.remaining === 0) break;
      }
      list.innerHTML = "";
      if (parents.size === 0) {
        const empty = document.createElement("div");
        empty.className = "picker-empty";
        empty.textContent = `No folders contain “${sf.name}” files yet — pick a place from the rail instead.`;
        list.appendChild(empty);
        return;
      }
      const rows = [...parents.values()].sort((a, b) =>
        a.path.localeCompare(b.path),
      );
      for (const row of rows) {
        const b = document.createElement("button");
        b.className = "picker-row";
        const name = row.path.slice(row.path.lastIndexOf("/") + 1) || row.path;
        b.innerHTML = "";
        const title = document.createElement("span");
        title.textContent = `📁 ${name}`;
        const sub = document.createElement("span");
        sub.className = "picker-row-sub";
        sub.textContent = row.path;
        b.append(title, sub);
        b.addEventListener("click", () => {
          void (async () => {
            const target = await this.resolvePathTarget(row.path);
            if (target) {
              this.history = [];
              await this.browse(target.locationId, target.dirId);
            }
          })();
        });
        list.appendChild(b);
      }
    } catch (err) {
      this.el.querySelector(".picker-path")!.textContent =
        `Error: ${errText(err)}`;
    }
  }

  private async browse(locationId: string, dirId: string): Promise<void> {
    this.inSmart = false;
    this.currentLocationId = locationId;
    this.currentDirId = dirId;
    this.syncActions();
    const list = this.el.querySelector(".picker-list")!;
    list.innerHTML = "";
    try {
      const items = await api.listDir(locationId, dirId);
      this.currentPath = await api.getItemPath(dirId).catch(() => "");
      this.el.querySelector(".picker-path")!.textContent =
        this.currentPath || "(choose a folder)";
      if (this.history.length > 0) {
        const up = document.createElement("button");
        up.className = "picker-row picker-up";
        up.textContent = "⬆ Up one level";
        up.addEventListener("click", () => {
          const prev = this.history.pop()!;
          void this.browse(prev.locationId, prev.dirId);
        });
        list.appendChild(up);
      }
      const folders = items
        .filter((i) => i.kind === "folder")
        .sort((a, b) => a.name.localeCompare(b.name));
      if (folders.length === 0) {
        const empty = document.createElement("div");
        empty.className = "picker-empty";
        empty.textContent = "No subfolders — you can still choose this folder";
        list.appendChild(empty);
      }
      for (const f of folders) {
        const row = document.createElement("button");
        row.className = "picker-row";
        row.textContent = `📁 ${f.name}`;
        row.addEventListener("click", () => {
          this.history.push({ locationId, dirId });
          void this.browse(locationId, f.id);
        });
        list.appendChild(row);
      }
    } catch (err) {
      this.el.querySelector(".picker-path")!.textContent = `Error: ${errText(err)}`;
    }
  }

  /** Enable/disable Choose + New Folder for the current view. */
  private syncActions(): void {
    const choose = this.el.querySelector<HTMLButtonElement>(".picker-choose")!;
    choose.disabled = this.inSmart;
    choose.title = this.inSmart
      ? "Pick one of the folders in this smart folder first"
      : "";
    const nf = this.el.querySelector<HTMLButtonElement>(".picker-newfolder")!;
    nf.disabled = this.inSmart || !this.currentDirId;
  }

  // ---------------------------------------------------------------
  // New Folder — inline name prompt in the picker itself.
  // ---------------------------------------------------------------

  private openNewFolder(): void {
    if (this.inSmart || !this.currentDirId) return;
    const row = this.el.querySelector<HTMLElement>(".picker-newrow")!;
    row.hidden = false;
    const input = this.el.querySelector<HTMLInputElement>(".picker-newname")!;
    input.value = "New Folder";
    input.focus();
    input.select();
  }

  private closeNewFolder(): void {
    const row = this.el.querySelector<HTMLElement>(".picker-newrow");
    if (row) row.hidden = true;
  }

  private async createNewFolder(): Promise<void> {
    const input = this.el.querySelector<HTMLInputElement>(".picker-newname")!;
    const name = input.value.trim();
    if (!name || name.includes("/") || name === "." || name === "..") {
      Toast.error("Give the folder a name without slashes.");
      input.focus();
      return;
    }
    const parentLoc = this.currentLocationId;
    const parentDir = this.currentDirId;
    try {
      // The existing create_folder command makes "Untitled Folder"
      // (auto-numbered); rename it to the typed name, then land in it.
      const createdPath = await api.createFolderIn(parentDir);
      const createdName = createdPath.slice(createdPath.lastIndexOf("/") + 1);
      let finalPath = createdPath;
      if (name !== createdName) {
        const kids = await api.listDir(parentLoc, parentDir);
        const created = kids.find((k) => k.name === createdName);
        if (created) {
          finalPath = await api.renameItem(created.id, name);
        }
      }
      this.closeNewFolder();
      Toast.success(`Created “${name}”.`);
      const target = await this.resolvePathTarget(finalPath);
      if (target) {
        this.history.push({ locationId: parentLoc, dirId: parentDir });
        await this.browse(target.locationId, target.dirId);
      } else {
        await this.browse(parentLoc, parentDir);
      }
    } catch (err) {
      Toast.error(`New folder failed: ${errText(err)}`);
    }
  }

  private close(path: string | null): void {
    this.el.hidden = true;
    const r = this.resolveFn;
    this.resolveFn = null;
    if (r) r(path);
  }
}
