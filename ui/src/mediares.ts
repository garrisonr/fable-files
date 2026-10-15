/**
 * Media Resolution — film + TV made ready for Plex / Emby /
 * Jellyfin (Dickles, 2026-10-15). The video twin of ROM
 * identification: Fable parses the release-soup name, matches it
 * against TMDB with the confidence shown honestly, judges the file
 * against the one layout all three servers accept, and — only after
 * a full preview, only for confirmed or confident matches — moves
 * files into that layout as a journaled job.
 *
 * The GUIDANCE LAW governs the copy: the Preferences section teaches
 * what TMDB is, that the key is free, how to get one, and how to name
 * files by hand so no key is ever needed. This file never sees a
 * saved key again after sending it — only whether one exists.
 */
import {
  api,
  type Item,
  type MediaresCandidate,
  type MediaresIdentity,
  type MediaresParsed,
  type MediaresScanRequest,
  type MediaresScanResult,
  type MediaresScanRow,
} from "./api";
import { Toast, errText } from "./toast";

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

function describeParsed(p: MediaresParsed): string {
  if (p.flags.includes("date_based") && p.air_date) {
    return `${p.title || "Untitled"} — dated episode (${p.air_date})`;
  }
  if (p.absolute_episode !== null) {
    return `${p.title || "Untitled"} — absolute episode ${p.absolute_episode}`;
  }
  if (p.kind === "episode" && p.season !== null && p.episode_first !== null) {
    const range =
      p.episode_last !== null && p.episode_last !== p.episode_first
        ? `E${String(p.episode_first).padStart(2, "0")}–E${String(p.episode_last).padStart(2, "0")}`
        : `E${String(p.episode_first).padStart(2, "0")}`;
    return `${p.title} — S${String(p.season).padStart(2, "0")}${range}`;
  }
  if (p.title) {
    return p.year ? `${p.title} (${p.year})` : p.title;
  }
  return "Untitled";
}

function describeIdentity(idn: MediaresIdentity): string {
  const bits: string[] = [idn.title];
  if (idn.mediaType === "episode" && idn.season !== null && idn.episode !== null) {
    bits[0] += ` — S${String(idn.season).padStart(2, "0")}E${String(idn.episode).padStart(2, "0")}`;
    if (idn.episodeEnd !== null && idn.episodeEnd !== idn.episode) {
      bits[0] += `–E${String(idn.episodeEnd).padStart(2, "0")}`;
    }
  } else if (idn.year) {
    bits[0] += ` (${idn.year})`;
  }
  if (idn.confirmed) bits.push("confirmed by you");
  else if (idn.source === "scan" && idn.confidence !== null)
    bits.push(`TMDB match · ${Math.round(idn.confidence * 100)}%`);
  else if (idn.source === "stored" && idn.confidence !== null)
    bits.push(`matched earlier · ${Math.round(idn.confidence * 100)}%`);
  else if (idn.source === "name") bits.push("from the file name — unverified");
  return bits.join(" · ");
}

const STATE_LABEL: Record<MediaresScanRow["state"], string> = {
  ready: "Server-ready",
  fixable: "Can organize",
  attention: "Needs attention",
};

// ---------------------------------------------------------------------
// The resolution dialog
// ---------------------------------------------------------------------

export class MediaResDialog {
  private el: HTMLDivElement;
  private bodyEl!: HTMLDivElement;
  private footEl!: HTMLDivElement;
  private titleEl!: HTMLElement;
  private req: MediaresScanRequest | null = null;
  private result: MediaresScanResult | null = null;
  private onDone: (() => void) | null = null;

  constructor() {
    this.el = document.createElement("div");
    this.el.className = "picker-overlay";
    this.el.hidden = true;
    this.el.innerHTML = `
      <div class="picker-card labels-card mrx-card" role="dialog" aria-label="Resolve media for servers">
        <h3 class="picker-title mrx-title">Resolve for Media Servers</h3>
        <div class="mrx-body"></div>
        <div class="picker-actions mrx-foot"></div>
      </div>`;
    document.body.appendChild(this.el);
    this.bodyEl = this.el.querySelector(".mrx-body")!;
    this.footEl = this.el.querySelector(".mrx-foot")!;
    this.titleEl = this.el.querySelector(".mrx-title")!;
    this.el.addEventListener("pointerdown", (e) => {
      if (e.target === this.el) this.close();
    });
  }

  async open(items: Item[], onDone: () => void): Promise<void> {
    this.onDone = onDone;
    this.el.hidden = false;
    this.titleEl.textContent = "Resolve for Media Servers";
    this.bodyEl.innerHTML = "";
    this.bodyEl.appendChild(el("p", "prefs-note", "Reading names…"));
    this.footEl.innerHTML = "";
    try {
      const paths: string[] = [];
      let folderRoot: string | null = null;
      for (const item of items) {
        const path = await api.getItemPath(item.id);
        paths.push(path);
        if (item.kind === "folder") folderRoot = path;
      }
      const root =
        folderRoot ??
        (paths.length
          ? paths[0].slice(0, paths[0].lastIndexOf("/")) || "/"
          : "/");
      this.req = { root, paths };
      await this.rescan();
    } catch (err) {
      this.bodyEl.innerHTML = "";
      this.bodyEl.appendChild(
        el("p", "prefs-note", `Could not start: ${errText(err)}`),
      );
      this.renderFoot(false);
    }
  }

  private close(): void {
    this.el.hidden = true;
  }

  private async rescan(): Promise<void> {
    if (!this.req) return;
    this.bodyEl.innerHTML = "";
    this.bodyEl.appendChild(el("p", "prefs-note", "Resolving…"));
    try {
      this.result = await api.mediaresScan(this.req);
      this.render();
    } catch (err) {
      this.bodyEl.innerHTML = "";
      this.bodyEl.appendChild(
        el("p", "prefs-note", `Scan failed: ${errText(err)}`),
      );
      this.renderFoot(false);
    }
  }

  private render(): void {
    const res = this.result;
    if (!res) return;
    const body = this.bodyEl;
    body.innerHTML = "";
    const summary = el(
      "p",
      "mrx-summary",
      `${res.ready} server-ready · ${res.fixable} can organize · ${res.attention} need attention`,
    );
    body.appendChild(summary);
    body.appendChild(
      el(
        "p",
        "mrx-intro",
        res.hasKey
          ? "Titles are checked against TMDB. A match is never renamed on a guess: confident matches still appear here first, and anything less than confident waits for your confirmation."
          : "No TMDB key is set, so titles are judged from the file names alone. Add a free key in Preferences → Media Resolution to verify them against TMDB — or confirm each one yourself below.",
      ),
    );
    if (res.truncated) {
      body.appendChild(
        el("p", "mrx-note", "This folder is big — only the first 500 video files were resolved."),
      );
    }
    const list = el("div", "mrx-list");
    for (const row of res.rows) {
      list.appendChild(this.renderRow(row));
    }
    body.appendChild(list);
    this.renderFoot(res.fixable > 0);
  }

  private renderRow(row: MediaresScanRow): HTMLElement {
    const wrap = el("div", `mrx-row mrx-${row.state}`);
    const head = el("div", "mrx-row-head");
    head.appendChild(el("span", "mrx-chip", STATE_LABEL[row.state]));
    const main = el("div", "mrx-row-main");
    main.appendChild(el("div", "mrx-name", row.name));
    main.appendChild(el("div", "mrx-parsed", describeParsed(row.parsed)));
    if (row.identity) {
      main.appendChild(
        el("div", "mrx-identity", `→ ${describeIdentity(row.identity)}`),
      );
    }
    if (row.targetRel && row.state !== "ready") {
      main.appendChild(el("div", "mrx-target", `files as ${row.targetRel}`));
    }
    if (row.note) {
      main.appendChild(el("div", "mrx-note", row.note));
    }
    head.appendChild(main);
    wrap.appendChild(head);

    const needsHuman =
      row.state === "attention" ||
      (row.identity !== null &&
        !row.identity.confirmed &&
        row.identity.source !== "name" &&
        row.identity.confidence !== null &&
        row.identity.confidence < 0.85);
    const canQuickConfirm =
      row.identity !== null &&
      !row.identity.confirmed &&
      row.identity.source !== "name";
    if (needsHuman || canQuickConfirm || row.identity === null) {
      const actions = el("div", "mrx-row-actions");
      if (canQuickConfirm) {
        const btn = el("button", "pill-btn mrx-confirm", "Confirm this match");
        btn.addEventListener("click", () => void this.confirmIdentity(row));
        actions.appendChild(btn);
      }
      const detail = el("div", "mrx-detail");
      detail.hidden = true;
      const toggle = el(
        "button",
        "pill-btn",
        row.candidates.length ? "Choose a match / correct…" : "Correct…",
      );
      toggle.addEventListener("click", () => {
        detail.hidden = !detail.hidden;
      });
      actions.appendChild(toggle);
      wrap.appendChild(actions);
      this.renderDetail(detail, row);
      wrap.appendChild(detail);
    }
    return wrap;
  }

  private renderDetail(host: HTMLElement, row: MediaresScanRow): void {
    host.innerHTML = "";
    for (const cand of row.candidates) {
      host.appendChild(this.renderCandidate(row, cand));
    }
    // Manual correct: the guidance path — say what it is, by hand.
    const form = el("div", "mrx-correct");
    form.appendChild(el("div", "mrx-correct-title", "Or say what it is:"));
    const grid = el("div", "mrx-grid");
    const typeSel = document.createElement("select");
    typeSel.className = "mr-in";
    for (const [v, label] of [
      ["movie", "Movie"],
      ["episode", "TV episode"],
    ] as const) {
      const opt = document.createElement("option");
      opt.value = v;
      opt.textContent = label;
      typeSel.appendChild(opt);
    }
    typeSel.value =
      row.identity?.mediaType ?? (row.parsed.kind === "episode" ? "episode" : "movie");
    const titleIn = document.createElement("input");
    titleIn.className = "mr-in";
    titleIn.placeholder = "Title";
    titleIn.value = row.identity?.title ?? row.parsed.title;
    const yearIn = document.createElement("input");
    yearIn.className = "mr-in";
    yearIn.placeholder = "Year";
    yearIn.inputMode = "numeric";
    yearIn.value = String(row.identity?.year ?? row.parsed.year ?? "");
    const seasonIn = document.createElement("input");
    seasonIn.className = "mr-in";
    seasonIn.placeholder = "Season";
    seasonIn.inputMode = "numeric";
    seasonIn.value = String(row.identity?.season ?? row.parsed.season ?? "");
    const epIn = document.createElement("input");
    epIn.className = "mr-in";
    epIn.placeholder = "Episode";
    epIn.inputMode = "numeric";
    epIn.value = String(row.identity?.episode ?? row.parsed.episode_first ?? "");
    const epEndIn = document.createElement("input");
    epEndIn.className = "mr-in";
    epEndIn.placeholder = "Last ep. (multi)";
    epEndIn.inputMode = "numeric";
    epEndIn.value = String(
      row.identity?.episodeEnd ?? row.parsed.episode_last ?? "",
    );
    grid.append(typeSel, titleIn, yearIn, seasonIn, epIn, epEndIn);
    form.appendChild(grid);
    const save = el("button", "pill-btn primary mrx-save", "Save this identity");
    save.addEventListener("click", () => {
      const num = (s: string): number | null => {
        const v = parseInt(s, 10);
        return Number.isFinite(v) ? v : null;
      };
      void this.confirmChoice(row, {
        mediaType: typeSel.value,
        tmdbId: null,
        title: titleIn.value.trim(),
        year: num(yearIn.value),
        season: num(seasonIn.value),
        episode: num(epIn.value),
        episodeEnd: num(epEndIn.value),
        episodeTitles: [],
      });
    });
    form.appendChild(save);
    host.appendChild(form);
  }

  private renderCandidate(
    row: MediaresScanRow,
    cand: MediaresCandidate,
  ): HTMLElement {
    const wrap = el("div", "mrx-cand");
    const c = cand.candidate;
    const label = `${c.title}${c.year ? ` (${c.year})` : ""} — ${Math.round(cand.score * 100)}% · ${cand.tier}`;
    wrap.appendChild(el("span", "mrx-cand-label", label));
    if (cand.reasons.length) {
      wrap.appendChild(
        el("span", "mrx-cand-why", cand.reasons.join("; ")),
      );
    }
    const btn = el("button", "pill-btn", "Use this");
    btn.addEventListener("click", () => {
      void this.confirmChoice(row, {
        mediaType: c.media_type === "tv" ? "episode" : "movie",
        tmdbId: c.id,
        title: c.title,
        year: c.year,
        season: row.parsed.season,
        episode: row.parsed.episode_first,
        episodeEnd: row.parsed.episode_last,
        episodeTitles: row.identity?.episodeTitles ?? [],
      });
    });
    wrap.appendChild(btn);
    return wrap;
  }

  private async confirmIdentity(row: MediaresScanRow): Promise<void> {
    const idn = row.identity;
    if (!idn) return;
    await this.confirmChoice(row, {
      mediaType: idn.mediaType,
      tmdbId: idn.tmdbId,
      title: idn.title,
      year: idn.year,
      season: idn.season,
      episode: idn.episode,
      episodeEnd: idn.episodeEnd,
      episodeTitles: idn.episodeTitles,
    });
  }

  private async confirmChoice(
    row: MediaresScanRow,
    choice: MediaresConfirmRequestChoice,
  ): Promise<void> {
    try {
      await api.mediaresConfirm({ path: row.path, choice });
      Toast.success(`Identified ${row.name}.`);
      await this.rescan();
    } catch (err) {
      Toast.error(`Could not save that identity: ${errText(err)}`);
    }
  }

  private renderFoot(canOrganize: boolean): void {
    const foot = this.footEl;
    foot.innerHTML = "";
    const cancel = el("button", "pill-btn", "Close");
    cancel.addEventListener("click", () => this.close());
    foot.appendChild(cancel);
    if (canOrganize && this.req) {
      const org = el("button", "pill-btn primary", "Preview Organize…");
      org.addEventListener("click", () => void this.previewOrganize());
      foot.appendChild(org);
    }
  }

  private async previewOrganize(): Promise<void> {
    if (!this.req) return;
    try {
      const plan = await api.mediaresPlanOrganize(this.req);
      const body = this.bodyEl;
      body.innerHTML = "";
      body.appendChild(
        el("h4", "mrx-preview-title", "Organize — check every line"),
      );
      body.appendChild(
        el(
          "p",
          "mrx-intro",
          "These files move into the layout Plex, Emby and Jellyfin all read. Nothing is overwritten — a file whose destination already exists is skipped and listed below. Empty folders left behind are not deleted. The move runs as a normal Fable job: pause or cancel it in Transfers, and Ctrl+Z after it finishes undoes it.",
        ),
      );
      const list = el("div", "mrx-list");
      for (const a of plan.actions) {
        const rowEl = el("div", "mrx-plan-row");
        rowEl.appendChild(el("div", "mrx-old", a.src));
        rowEl.appendChild(el("div", "mrx-new", `→ ${a.rel}`));
        list.appendChild(rowEl);
      }
      for (const ex of plan.excluded) {
        const rowEl = el("div", "mrx-plan-row mrx-excluded");
        rowEl.appendChild(el("div", "mrx-old", ex.path));
        rowEl.appendChild(el("div", "mrx-note", `skipped — ${ex.reason}`));
        list.appendChild(rowEl);
      }
      body.appendChild(list);
      const foot = this.footEl;
      foot.innerHTML = "";
      const back = el("button", "pill-btn", "Back");
      back.addEventListener("click", () => this.render());
      const go = el(
        "button",
        "pill-btn primary",
        plan.actions.length
          ? `Organize ${plan.actions.length} file${plan.actions.length === 1 ? "" : "s"}`
          : "Nothing to organize",
      );
      go.toggleAttribute("disabled", plan.actions.length === 0);
      go.addEventListener("click", () => void this.startOrganize());
      foot.append(back, go);
    } catch (err) {
      Toast.error(`Could not build the organize preview: ${errText(err)}`);
    }
  }

  private async startOrganize(): Promise<void> {
    if (!this.req) return;
    try {
      await api.mediaresStartOrganize(this.req);
      Toast.success(
        "Organize job started — watch it in Transfers. Ctrl+Z undoes a finished organize.",
        7000,
      );
      this.close();
      this.onDone?.();
    } catch (err) {
      Toast.error(`Organize did not start: ${errText(err)}`);
    }
  }
}

type MediaresConfirmRequestChoice =
  import("./api").MediaresConfirmRequest["choice"];

// ---------------------------------------------------------------------
// Preferences section: the TMDB key, taught properly
// ---------------------------------------------------------------------

export class MediaResSection {
  private body: HTMLElement | null = null;
  private hasKey = false;

  async mount(host: HTMLElement): Promise<void> {
    host.appendChild(el("h3", "prefs-section", "Media Resolution"));
    host.appendChild(
      el(
        "p",
        "prefs-note",
        "Make your film and TV libraries server-ready: right-click video files (or a folder of them) and choose “Resolve for Media Servers”. Fable reads the messy file name, works out what the film or episode actually is, and can file it in the one layout Plex, Emby and Jellyfin all understand — Movies as “Title (Year)”, shows as “Show (Year) / Season 01 / Show (Year) - S01E02”. Nothing is ever renamed on a guess, and organizing always shows you the full plan first.",
      ),
    );
    this.body = el("div", "mrx-prefs");
    this.body.appendChild(el("p", "prefs-note", "Loading…"));
    host.appendChild(this.body);
    try {
      const view = await api.getMediaresSettings();
      this.hasKey = view.hasKey;
      this.render(view.keyringOk);
    } catch (err) {
      this.body.innerHTML = "";
      this.body.appendChild(
        el("p", "prefs-note", `Media resolution settings could not load: ${errText(err)}`),
      );
    }
  }

  private render(keyringOk: boolean): void {
    const body = this.body;
    if (!body) return;
    body.innerHTML = "";
    const block = el("div", "mr-tmdb");
    const p1 = document.createElement("p");
    p1.innerHTML =
      "<strong>TMDB verification (optional, free).</strong> The Movie Database is a free, community-run catalogue of films and shows. With a free TMDB API key, Fable checks each file's title and year against it and shows you how sure the match is, instead of trusting a messy file name. Without a key, Fable still parses names, judges the layout, and organizes files you confirm yourself — the key only adds verification.";
    block.appendChild(p1);
    const p2 = document.createElement("p");
    p2.innerHTML =
      "To get a key: 1. Create a free account at <span class=\"mr-mono\">themoviedb.org</span>. 2. Open <span class=\"mr-mono\">Settings → API</span> and copy the <span class=\"mr-mono\">API Key (v3 auth)</span>. 3. Paste it here — it is stored in your system keyring, never in a settings file, and is only used when you run a resolution scan.";
    block.appendChild(p2);
    const row = el("div", "mr-keyrow");
    const input = document.createElement("input");
    input.className = "mr-in mr-key";
    input.type = "password";
    input.placeholder = this.hasKey
      ? "A key is saved — paste to replace it"
      : "TMDB API key (v3 auth)";
    const status = el("span", "mr-status");
    status.textContent = this.hasKey
      ? "Key saved in the system keyring."
      : keyringOk
        ? "No key saved."
        : "The system keyring is unavailable on this machine.";
    const save = el("button", "pill-btn", "Save key");
    save.addEventListener("click", async () => {
      const v = input.value.trim();
      if (!v) {
        status.textContent = "Paste a key first.";
        return;
      }
      try {
        status.textContent = await api.setMediaresKey(v);
        input.value = "";
        this.hasKey = true;
        this.render(keyringOk);
      } catch (err) {
        status.textContent = `Could not save: ${errText(err)}`;
      }
    });
    const check = el("button", "pill-btn", "Check key");
    check.addEventListener("click", async () => {
      status.textContent = "Checking…";
      try {
        const r = await api.validateMediaresKey();
        status.textContent = r.message;
      } catch (err) {
        status.textContent = `Check failed: ${errText(err)}`;
      }
    });
    const clear = el("button", "pill-btn", "Remove");
    clear.addEventListener("click", async () => {
      try {
        status.textContent = await api.setMediaresKey(null);
        this.hasKey = false;
        this.render(keyringOk);
      } catch (err) {
        status.textContent = `Could not remove: ${errText(err)}`;
      }
    });
    row.append(input, save, check, clear, status);
    block.appendChild(row);
    const p3 = document.createElement("p");
    p3.innerHTML =
      "Prefer to do it yourself? Name films <span class=\"mr-mono\">Title (Year).mkv</span> inside a folder of the same name, and episodes <span class=\"mr-mono\">Show (Year) - S01E02.mkv</span> inside <span class=\"mr-mono\">Show (Year)/Season 01</span> — every server will match them, and Fable's checker will call them server-ready with no key at all.";
    block.appendChild(p3);
    body.appendChild(block);
  }
}
