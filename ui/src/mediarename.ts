/**
 * Rename for Media Player (Plex / Kodi / Emby): turns release-soup
 * filenames into the clean forms media servers match on —
 *   "Maid.Robot.2026.1080p.WEBRip.x264-[YTS].mp4" -> "Maid Robot (2026).mp4"
 *   "Show.Name.S01E02.720p.WEB-DL.mkv"            -> "Show Name - S01E02.mkv"
 * The parser is local and deterministic. TMDB lookup is OPTIONAL:
 * with the user's own free API key, proposals can be verified
 * against The Movie Database. Every proposal is editable before
 * applying, batches included (Dickles, 2026-10-13).
 */
import { api, type Item } from "./api";
import { Toast } from "./toast";

const JUNK = new Set([
  "2160p", "1080p", "720p", "576p", "480p", "4k", "uhd", "web", "webrip",
  "web-dl", "webdl", "bluray", "bdrip", "brrip", "hdrip", "dvdrip", "hdtv",
  "x264", "x265", "h264", "h265", "hevc", "avc", "aac", "ac3", "eac3", "dts",
  "truehd", "atmos", "5", "7", "2", "10bit", "hdr", "hdr10", "dv", "remux",
  "proper", "repack", "extended", "uncut", "internal", "limited", "yts",
  "yify", "rarbg", "galaxyrg", "ettv", "multi", "subs", "dubbed", "complete",
  "season", "pack", "amzn", "nf", "hulu", "dsnp", "atvp", "imax", "oar",
]);

export interface ParsedName {
  title: string;
  year: number | null;
  episode: string | null; // "S01E02"
  ext: string;
  proposal: string;
}

function sanitize(name: string): string {
  return name.replace(/[\\/:*?"<>|]/g, "").replace(/\s+/g, " ").trim();
}

export function parseMediaName(filename: string): ParsedName {
  const dot = filename.lastIndexOf(".");
  const ext = dot > 0 ? filename.slice(dot) : "";
  const stem = dot > 0 ? filename.slice(0, dot) : filename;
  const work = stem.replace(/[._]+/g, " ").replace(/\s+/g, " ").trim();

  // Episode marker first: the title is whatever precedes it.
  let episode: string | null = null;
  let head = work;
  const epMatch = work.match(/\bS(\d{1,2})\s?E(\d{1,3})\b/i) ??
    work.match(/\b(\d{1,2})x(\d{2,3})\b/);
  if (epMatch && epMatch.index !== undefined) {
    episode = `S${epMatch[1].padStart(2, "0")}E${epMatch[2].padStart(2, "0")}`;
    head = work.slice(0, epMatch.index);
  }

  // Year: last plausible year in the head; the title precedes it.
  let year: number | null = null;
  let titlePart = head;
  const years = [...head.matchAll(/\b(19\d{2}|20\d{2})\b/g)];
  if (years.length) {
    const last = years[years.length - 1];
    year = parseInt(last[1], 10);
    titlePart = head.slice(0, last.index);
  }

  // Clean the title: bracket remnants out, junk tokens off the tail.
  let words = titlePart.replace(/[[\](){}]/g, " ").split(/\s+/).filter(Boolean);
  while (words.length > 1) {
    const tail = words[words.length - 1].toLowerCase().replace(/[-–—]$/, "");
    if (JUNK.has(tail) || /^[-–—]+$/.test(words[words.length - 1])) words.pop();
    else break;
  }
  // Leading junk too ("YTS - Title" style).
  while (words.length > 1 && JUNK.has(words[0].toLowerCase())) words.shift();
  let title = words.join(" ").replace(/[-–—:\s]+$/, "").trim();
  if (!title) {
    title = work
      .split(/\s+/)
      .filter((w) => !JUNK.has(w.toLowerCase()))
      .slice(0, 4)
      .join(" ");
  }
  const proposal = sanitize(
    episode
      ? `${title} - ${episode}${ext}`
      : year
        ? `${title} (${year})${ext}`
        : `${title}${ext}`,
  );
  return { title, year, episode, ext, proposal };
}

const TMDB_KEY = "ff-tmdb-key";

export class MediaRenameDialog {
  private el: HTMLDivElement;
  private listEl!: HTMLDivElement;
  private keyInput!: HTMLInputElement;
  private statusEl!: HTMLSpanElement;
  private applyBtn!: HTMLButtonElement;
  private titleEl!: HTMLElement;
  private rows: Array<{ item: Item; input: HTMLInputElement }> = [];
  private onDone: (() => void) | null = null;

  constructor() {
    this.el = document.createElement("div");
    this.el.className = "picker-overlay";
    this.el.hidden = true;
    this.el.innerHTML = `
      <div class="picker-card labels-card mr-card" role="dialog" aria-label="Rename for media player">
        <h3 class="picker-title mr-title">Rename for Media Player</h3>
        <p class="mr-intro">Clean names Plex, Kodi and Emby match correctly. Edit any proposal before applying — the file extension is always kept.</p>
        <div class="mr-warn">⚠ Renaming can’t be easily undone. You can try Ctrl+Z afterwards, but check the result — a wrong name in a big batch is a pain to fix by hand.</div>
        <div class="mr-list"></div>
        <div class="mr-tmdb">
          <p><strong>TMDB lookup (optional).</strong> Fable renames from the filename alone, and that’s usually enough. With a free TMDB key it can verify titles against The Movie Database instead:</p>
          <p>1. Go to <span class="mr-mono">themoviedb.org</span> and create a free account.<br/>
          2. Open <span class="mr-mono">Settings → API</span> and copy the <span class="mr-mono">API Key (v3 auth)</span>.<br/>
          3. Paste it here — it’s saved on this PC only, and used only when you press Look up.</p>
          <div class="mr-keyrow">
            <input class="mr-in mr-key" type="text" placeholder="TMDB API key (v3 auth)" />
            <button class="pill-btn mr-lookup">Look up on TMDB</button>
            <span class="mr-status" role="status"></span>
          </div>
        </div>
        <div class="picker-actions">
          <button class="mr-cancel">Cancel</button>
          <button class="mr-apply primary">Rename</button>
        </div>
      </div>`;
    document.body.appendChild(this.el);
    this.listEl = this.el.querySelector(".mr-list")!;
    this.keyInput = this.el.querySelector(".mr-key")!;
    this.statusEl = this.el.querySelector(".mr-status")!;
    this.applyBtn = this.el.querySelector(".mr-apply")!;
    this.titleEl = this.el.querySelector(".mr-title")!;
    try {
      this.keyInput.value = localStorage.getItem(TMDB_KEY) ?? "";
    } catch {
      /* storage unavailable */
    }
    this.el
      .querySelector(".mr-cancel")!
      .addEventListener("click", () => this.close());
    this.applyBtn.addEventListener("click", () => void this.apply());
    this.el
      .querySelector(".mr-lookup")!
      .addEventListener("click", () => void this.lookupTmdb());
    this.el.addEventListener("pointerdown", (e) => {
      if (e.target === this.el) this.close();
    });
  }

  open(items: Item[], onDone: () => void): void {
    this.onDone = onDone;
    this.rows = [];
    this.listEl.innerHTML = "";
    this.statusEl.textContent = "";
    this.titleEl.textContent =
      items.length === 1
        ? "Rename for Media Player"
        : `Rename ${items.length} files for Media Player`;
    this.applyBtn.textContent =
      items.length === 1 ? "Rename" : `Rename ${items.length} files`;
    for (const item of items) {
      const row = document.createElement("div");
      row.className = "mr-row";
      const old = document.createElement("span");
      old.className = "mr-old";
      old.textContent = item.name;
      old.title = item.name;
      const input = document.createElement("input");
      input.className = "mr-in";
      input.type = "text";
      input.value = parseMediaName(item.name).proposal;
      row.append(old, input);
      this.listEl.appendChild(row);
      this.rows.push({ item, input });
    }
    this.el.hidden = false;
  }

  private close(): void {
    this.el.hidden = true;
  }

  private async lookupTmdb(): Promise<void> {
    const key = this.keyInput.value.trim();
    if (!key) {
      this.statusEl.textContent = "Paste a TMDB key first.";
      return;
    }
    try {
      localStorage.setItem(TMDB_KEY, key);
    } catch {
      /* ignore */
    }
    this.statusEl.textContent = "Looking up…";
    let ok = 0;
    let failed = 0;
    for (const row of this.rows) {
      const parsed = parseMediaName(row.item.name);
      const type = parsed.episode ? "tv" : "movie";
      const yearParam = parsed.year
        ? type === "movie"
          ? `&year=${parsed.year}`
          : `&first_air_date_year=${parsed.year}`
        : "";
      try {
        const res = await fetch(
          `https://api.themoviedb.org/3/search/${type}?api_key=${encodeURIComponent(key)}&query=${encodeURIComponent(parsed.title)}${yearParam}`,
        );
        if (!res.ok) throw new Error(String(res.status));
        const data = (await res.json()) as {
          results?: Array<Record<string, string>>;
        };
        const first = data.results?.[0];
        if (!first) {
          failed++;
          continue;
        }
        const title = (first.title ?? first.name ?? parsed.title).trim();
        const dateStr = first.release_date ?? first.first_air_date ?? "";
        const year = dateStr ? parseInt(dateStr.slice(0, 4), 10) : parsed.year;
        row.input.value = sanitize(
          parsed.episode
            ? `${title} - ${parsed.episode}${parsed.ext}`
            : year
              ? `${title} (${year})${parsed.ext}`
              : `${title}${parsed.ext}`,
        );
        ok++;
      } catch {
        failed++;
      }
    }
    this.statusEl.textContent =
      failed === 0
        ? `TMDB verified ${ok} title${ok === 1 ? "" : "s"}.`
        : `TMDB verified ${ok}; ${failed} not found or the key was rejected — proposals unchanged for those.`;
  }

  private async apply(): Promise<void> {
    let renamed = 0;
    let skipped = 0;
    const errors: string[] = [];
    for (const row of this.rows) {
      const newName = row.input.value.trim();
      if (!newName || newName === row.item.name) {
        skipped++;
        continue;
      }
      try {
        await api.renameItem(row.item.id, newName);
        renamed++;
      } catch (err) {
        errors.push(`${row.item.name}: ${err}`);
      }
    }
    this.close();
    if (renamed > 0) {
      Toast.success(
        `Renamed ${renamed} file${renamed === 1 ? "" : "s"} for your media player.`,
        7000,
      );
    }
    if (errors.length) {
      Toast.error(
        `${errors.length} rename${errors.length === 1 ? "" : "s"} failed — ${errors[0]}`,
        9000,
      );
    }
    if (renamed === 0 && errors.length === 0) {
      Toast.info("Nothing to rename — proposals matched the current names.");
    }
    void skipped;
    this.onDone?.();
  }
}
