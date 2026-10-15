/**
 * Music Completion — tags, cover art, and lyrics for a music
 * library (Dickles, 2026-10-15). The music sibling of Media
 * Completion: where films got name + poster + subtitles, tracks
 * get their missing tags (MusicBrainz), their cover (Cover Art
 * Archive), and their lyrics (LRCLIB) — all free, no keys.
 *
 * THE LAWS this surface obeys: a match is proposed, never
 * applied on a guess — the review shows every field it would
 * fill before anything is written; fields that are already
 * filled are only ever CHANGED when the user ticks that exact
 * overwrite; and every apply is a journaled run whose Undo (in
 * Preferences → Music Completion) restores the prior tags.
 */
import {
  api,
  type Item,
  type MuApplyField,
  type MuApplyItem,
  type MuLyricsView,
  type MuRun,
  type MuSchedule,
  type MuSettings,
  type MuStatus,
  type MuTrackProposal,
} from "./api";
import { Toast, errText } from "./toast";
import { wireGuideLinks } from "./guidelinks";

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

export const MU_FIELD_LABELS: Record<string, string> = {
  title: "Title",
  artist: "Artist",
  album: "Album",
  albumArtist: "Album artist",
  year: "Year",
  track: "Track",
  trackTotal: "Total tracks",
  disc: "Disc",
  discTotal: "Total discs",
  genre: "Genre",
  composer: "Composer",
};

// ---------------------------------------------------------------------
// LRC parsing for the Quick Look / player highlight (the backend
// parser is ufm_media::lrc; this is its small presentation twin).
// ---------------------------------------------------------------------

export interface LrcLine {
  timeMs: number;
  text: string;
}

export function parseLrcLines(text: string): LrcLine[] {
  const out: LrcLine[] = [];
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    let rest = line;
    const stamps: number[] = [];
    while (rest.startsWith("[")) {
      const close = rest.indexOf("]");
      if (close < 0) break;
      const inner = rest.slice(1, close);
      const m = /^(\d+):(\d{1,2})(?:[.,](\d{1,3}))?$/.exec(inner);
      if (!m) break;
      const frac = m[3] ? parseInt(m[3].padEnd(3, "0"), 10) : 0;
      stamps.push(
        parseInt(m[1], 10) * 60000 + parseInt(m[2], 10) * 1000 + frac,
      );
      rest = rest.slice(close + 1);
    }
    for (const t of stamps) out.push({ timeMs: t, text: rest.trim() });
  }
  out.sort((a, b) => a.timeMs - b.timeMs);
  return out;
}

export function looksSyncedText(text: string): boolean {
  return /\[\d+:\d{1,2}[.,]\d+\]/.test(text);
}

// ---------------------------------------------------------------------
// The review dialog: per-track match + field-by-field diff.
// ---------------------------------------------------------------------

interface RowState {
  row: MuTrackProposal;
  include: HTMLInputElement;
  overwriteBoxes: Map<string, HTMLInputElement>;
  artBox: HTMLInputElement | null;
  lyricsBox: HTMLInputElement | null;
}

export class MusicCompleteDialog {
  private el: HTMLDivElement;
  private bodyEl!: HTMLDivElement;
  private footEl!: HTMLDivElement;
  private titleEl!: HTMLElement;
  private root = "/";
  private rows: RowState[] = [];
  private onDone: (() => void) | null = null;
  private lyricsDialog = new LyricsDialog();

  constructor() {
    this.el = document.createElement("div");
    this.el.className = "picker-overlay";
    this.el.hidden = true;
    this.el.innerHTML = `
      <div class="picker-card labels-card mrx-card mu-card" role="dialog" aria-label="Complete music tags and lyrics">
        <h3 class="picker-title mrx-title">Complete Music</h3>
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
      await this.start(root, paths);
    } catch (err) {
      this.el.hidden = false;
      this.bodyEl.innerHTML = "";
      this.bodyEl.appendChild(
        el("p", "prefs-note", `Could not start: ${errText(err)}`),
      );
      this.renderFoot(0);
    }
  }

  /** The hamburger-menu door: complete the folder being browsed. */
  async openFolder(path: string, onDone: () => void): Promise<void> {
    this.onDone = onDone;
    await this.start(path, [path]);
  }

  private async start(root: string, paths: string[]): Promise<void> {
    this.el.hidden = false;
    this.titleEl.textContent = "Complete Music — Tags, Artwork & Lyrics";
    this.bodyEl.innerHTML = "";
    this.bodyEl.appendChild(
      el(
        "p",
        "prefs-note",
        "Reading tags and asking MusicBrainz… (it answers about once a second, by its own etiquette — a big folder takes a while.)",
      ),
    );
    this.footEl.innerHTML = "";
    this.root = root;
    try {
      const view = await api.muScan({ root, paths });
      this.renderScan(view.rows, view.truncated, view.fingerprintingReady);
    } catch (err) {
      this.bodyEl.innerHTML = "";
      this.bodyEl.appendChild(
        el("p", "prefs-note", `Could not scan: ${errText(err)}`),
      );
      this.renderFoot(0);
    }
  }

  private close(): void {
    this.el.hidden = true;
  }

  private renderScan(
    rows: MuTrackProposal[],
    truncated: boolean,
    fingerprintingReady: boolean,
  ): void {
    const body = this.bodyEl;
    body.innerHTML = "";
    this.rows = [];
    const work = rows.filter((r) => r.needsWork).length;
    body.appendChild(
      el(
        "p",
        "mrx-summary",
        `${rows.length} track(s) read · ${work} could be completed · ${rows.length - work} already complete or unmatched`,
      ),
    );
    body.appendChild(
      el(
        "p",
        "mrx-intro",
        "MusicBrainz proposes; you dispose. Empty tags get filled. A tag that already says something different is NEVER changed unless you tick that exact line — and everything you apply lands as one journaled run you can undo in Preferences → Music Completion.",
      ),
    );
    if (!fingerprintingReady) {
      body.appendChild(
        el(
          "p",
          "mrx-note",
          "Fingerprint matching (AcoustID) is off — it needs the free fpcalc tool plus your own AcoustID key (Preferences → Music Completion). Tag and filename matching are unaffected.",
        ),
      );
    }
    if (truncated) {
      body.appendChild(
        el(
          "p",
          "mrx-note",
          "This folder is big — only the first 500 audio files were scanned.",
        ),
      );
    }
    for (const row of rows) {
      body.appendChild(this.renderRow(row));
    }
    this.renderFoot(work);
  }

  private renderRow(row: MuTrackProposal): HTMLElement {
    const wrap = el("div", "mc-run mu-row");
    const head = el("div", "mc-run-head");
    const include = document.createElement("input");
    include.type = "checkbox";
    include.checked = row.needsWork;
    include.disabled = !row.needsWork;
    include.title = "Complete this track";
    head.appendChild(include);
    head.appendChild(el("strong", "", row.name));
    const chip = el(
      "span",
      "mr-status",
      row.candidate
        ? `${row.state} · ${Math.round(row.candidate.score * 100)}%`
        : row.state,
    );
    head.appendChild(chip);
    wrap.appendChild(head);

    const state: RowState = {
      row,
      include,
      overwriteBoxes: new Map(),
      artBox: null,
      lyricsBox: null,
    };
    this.rows.push(state);

    if (row.candidate) {
      const c = row.candidate.candidate;
      const line = el(
        "div",
        "mrx-parsed",
        `${c.artist} — ${c.title}${c.album ? ` · ${c.album}` : ""}${c.year ? ` (${c.year})` : ""}`,
      );
      wrap.appendChild(line);
      if (row.candidate.evidence.length) {
        wrap.appendChild(
          el("div", "mu-evidence", row.candidate.evidence.join(" · ")),
        );
      }
      if (c.releaseId && row.artAvailable) {
        const img = document.createElement("img");
        img.className = "mu-art-preview";
        img.alt = "Cover art preview";
        img.src = `https://coverartarchive.org/release/${c.releaseId}/front-250`;
        img.addEventListener("error", () => img.remove());
        wrap.appendChild(img);
      }
    } else if (row.note) {
      wrap.appendChild(el("p", "mrx-note", row.note));
    }

    // The field-by-field diff.
    for (const change of row.changes) {
      const line = el("div", "mu-change");
      line.appendChild(
        el("span", "mu-field", MU_FIELD_LABELS[change.field] ?? change.field),
      );
      line.appendChild(
        el(
          "span",
          "mu-from",
          change.current ?? "— empty —",
        ),
      );
      line.appendChild(el("span", "mu-arrow", "→"));
      line.appendChild(el("span", "mu-to", change.proposed));
      if (change.overwrite) {
        const box = document.createElement("input");
        box.type = "checkbox";
        box.checked = false;
        box.title = "Replace the existing value";
        const lab = el("label", "mu-overwrite", "");
        lab.append(box, el("span", "", "overwrite"));
        line.appendChild(lab);
        state.overwriteBoxes.set(change.field, box);
      }
      wrap.appendChild(line);
    }

    // Artwork line.
    const art = el("div", "mu-change");
    art.appendChild(el("span", "mu-field", "Cover art"));
    if (row.hasArt) {
      art.appendChild(el("span", "mu-from", "already in the file — yours wins"));
    } else if (row.artAvailable) {
      art.appendChild(el("span", "mu-to", "will fetch from the Cover Art Archive"));
      const box = document.createElement("input");
      box.type = "checkbox";
      box.checked = true;
      const lab = el("label", "mu-overwrite", "");
      lab.append(box, el("span", "", "include"));
      art.appendChild(lab);
      state.artBox = box;
    } else {
      art.appendChild(el("span", "mu-from", "none found"));
    }
    wrap.appendChild(art);

    // Lyrics line + the Lyrics Manager door.
    const lyr = el("div", "mu-change");
    lyr.appendChild(el("span", "mu-field", "Lyrics"));
    if (row.hasLyrics) {
      lyr.appendChild(el("span", "mu-from", "already saved"));
    } else if (row.lyricsAvailable) {
      lyr.appendChild(
        el(
          "span",
          "mu-to",
          row.fetchedSynced
            ? "LRCLIB has them — with synced (timed) lines"
            : "LRCLIB has them",
        ),
      );
      const box = document.createElement("input");
      box.type = "checkbox";
      box.checked = true;
      const lab = el("label", "mu-overwrite", "");
      lab.append(box, el("span", "", "include"));
      lyr.appendChild(lab);
      state.lyricsBox = box;
    } else {
      lyr.appendChild(el("span", "mu-from", "none found"));
    }
    const lyrBtn = el("button", "pill-btn", "Lyrics…");
    lyrBtn.addEventListener("click", () => {
      void this.lyricsDialog.open(row.path, row.name);
    });
    lyr.appendChild(lyrBtn);
    wrap.appendChild(lyr);

    if (row.note && row.candidate) {
      wrap.appendChild(el("p", "mrx-note", row.note));
    }
    return wrap;
  }

  private renderFoot(workCount: number): void {
    const foot = this.footEl;
    foot.innerHTML = "";
    const apply = el(
      "button",
      "pill-btn primary",
      workCount ? `Complete the ticked tracks` : "Nothing to complete",
    );
    apply.toggleAttribute("disabled", workCount === 0);
    apply.addEventListener("click", () => void this.apply());
    const close = el("button", "pill-btn", "Close");
    close.addEventListener("click", () => this.close());
    foot.append(apply, close);
  }

  private async apply(): Promise<void> {
    const items: MuApplyItem[] = [];
    for (const st of this.rows) {
      if (!st.include.checked) continue;
      const row = st.row;
      const fields: MuApplyField[] = [];
      for (const change of row.changes) {
        if (change.overwrite) {
          if (!st.overwriteBoxes.get(change.field)?.checked) continue;
        }
        fields.push({ field: change.field, value: change.proposed });
      }
      const cand = row.candidate?.candidate;
      items.push({
        path: row.path,
        fields,
        artReleaseId:
          st.artBox?.checked && cand ? cand.releaseId : null,
        artReleaseGroupId:
          st.artBox?.checked && cand ? cand.releaseGroupId : null,
        lyricsPlain: st.lyricsBox?.checked ? row.fetchedPlain : null,
        lyricsSynced: st.lyricsBox?.checked ? row.fetchedSynced : null,
      });
    }
    if (!items.length) {
      Toast.error("Nothing is ticked — tick a track (and any overwrites) first.");
      return;
    }
    try {
      const run = await api.muApply({ target: this.root, items });
      const s = run.summary;
      const bits = [
        `${s.filesCompleted ?? 0} track(s) completed`,
        `${s.fieldsWritten ?? 0} tag(s) written`,
      ];
      if (s.artworkEmbedded) bits.push(`${s.artworkEmbedded} cover(s)`);
      if (s.lyricsSaved) bits.push(`${s.lyricsSaved} lyric save(s)`);
      Toast.success(bits.join(" · ") + ". Undo lives in Preferences → Music Completion.", 9000);
      for (const f of s.failures ?? []) Toast.error(f);
      this.close();
      this.onDone?.();
    } catch (err) {
      Toast.error(`Apply failed: ${errText(err)}`);
    }
  }
}

// ---------------------------------------------------------------------
// The Lyrics Manager: view / fetch / edit / save one file's lyrics.
// ---------------------------------------------------------------------

export class LyricsDialog {
  private el: HTMLDivElement;
  private bodyEl!: HTMLDivElement;
  private footEl!: HTMLDivElement;
  private path = "";
  private view: MuLyricsView | null = null;
  private editor!: HTMLTextAreaElement;

  constructor() {
    this.el = document.createElement("div");
    this.el.className = "picker-overlay";
    this.el.hidden = true;
    this.el.innerHTML = `
      <div class="picker-card labels-card mrx-card mu-card" role="dialog" aria-label="Lyrics manager">
        <h3 class="picker-title mrx-title">Lyrics</h3>
        <div class="mrx-body"></div>
        <div class="picker-actions mrx-foot"></div>
      </div>`;
    document.body.appendChild(this.el);
    this.bodyEl = this.el.querySelector(".mrx-body")!;
    this.footEl = this.el.querySelector(".mrx-foot")!;
    this.el.addEventListener("pointerdown", (e) => {
      if (e.target === this.el) this.close();
    });
  }

  async open(path: string, name: string): Promise<void> {
    this.path = path;
    this.el.hidden = false;
    this.el.querySelector(".mrx-title")!.textContent = `Lyrics — ${name}`;
    this.bodyEl.innerHTML = "";
    this.bodyEl.appendChild(el("p", "prefs-note", "Loading…"));
    this.footEl.innerHTML = "";
    try {
      this.view = await api.muLyricsGet(path, false);
      this.render();
    } catch (err) {
      this.bodyEl.innerHTML = "";
      this.bodyEl.appendChild(
        el("p", "prefs-note", `Could not read lyrics: ${errText(err)}`),
      );
    }
  }

  private close(): void {
    this.el.hidden = true;
  }

  private currentText(): string {
    const v = this.view;
    if (!v) return "";
    return (
      v.embeddedSynced ?? v.embeddedPlain ?? v.sidecarText ?? ""
    );
  }

  private render(): void {
    const v = this.view;
    if (!v) return;
    const body = this.bodyEl;
    body.innerHTML = "";
    const where: string[] = [];
    if (v.embeddedSynced) where.push("synced lyrics are embedded in the file");
    else if (v.embeddedPlain) where.push("plain lyrics are embedded in the file");
    if (v.sidecarText) where.push("a .lrc sidecar sits beside the file");
    body.appendChild(
      el(
        "p",
        "mrx-intro",
        where.length
          ? `Right now: ${where.join(" · ")}.`
          : "No lyrics anywhere yet — fetch them from LRCLIB (free, no account), or type them in below.",
      ),
    );
    if (v.note) body.appendChild(el("p", "mrx-note", v.note));

    this.editor = document.createElement("textarea");
    this.editor.className = "mu-lyrics-editor";
    this.editor.rows = 14;
    this.editor.value = this.currentText();
    this.editor.placeholder =
      "Lyrics go here. Lines that start with a time stamp — [00:12.00] like this — are synced lyrics: players (and Fable's Quick Look) highlight each line as it's sung.";
    body.appendChild(this.editor);

    // A read-only synced preview so the stamps read as a song,
    // not as markup soup.
    const preview = el("div", "mu-lrc-preview");
    const drawPreview = () => {
      preview.innerHTML = "";
      const lines = parseLrcLines(this.editor.value);
      if (!lines.length) {
        preview.appendChild(
          el("p", "prefs-note", "No timed lines yet — this will save as plain lyrics."),
        );
        return;
      }
      for (const l of lines.slice(0, 60)) {
        const mm = String(Math.floor(l.timeMs / 60000));
        const ss = String(Math.floor((l.timeMs % 60000) / 1000)).padStart(2, "0");
        preview.appendChild(
          el("div", "mu-lrc-line", `${mm}:${ss}  ${l.text}`),
        );
      }
    };
    this.editor.addEventListener("input", drawPreview);
    drawPreview();
    body.appendChild(preview);

    const foot = this.footEl;
    foot.innerHTML = "";
    const fetchBtn = el("button", "pill-btn", "Fetch from LRCLIB");
    fetchBtn.addEventListener("click", async () => {
      fetchBtn.textContent = "Asking LRCLIB…";
      try {
        const got = await api.muLyricsGet(this.path, true);
        if (got.fetchedSynced || got.fetchedPlain) {
          this.editor.value = got.fetchedSynced ?? got.fetchedPlain ?? "";
          this.editor.dispatchEvent(new Event("input"));
          Toast.success("Lyrics fetched — review them, then Save.");
        } else if (got.fetchedInstrumental) {
          Toast.success("LRCLIB says this track is an instrumental.");
        } else {
          Toast.error(got.note ?? "LRCLIB has no lyrics for this track yet.");
        }
      } catch (err) {
        Toast.error(`Fetch failed: ${errText(err)}`);
      } finally {
        fetchBtn.textContent = "Fetch from LRCLIB";
      }
    });
    const save = el("button", "pill-btn primary", "Save lyrics");
    save.addEventListener("click", async () => {
      const text = this.editor.value.trim();
      try {
        const msg = await api.muLyricsSave(this.path, text ? this.editor.value : null);
        Toast.success(msg, 8000);
        this.close();
      } catch (err) {
        Toast.error(`Save failed: ${errText(err)}`);
      }
    });
    const remove = el("button", "pill-btn", "Remove embedded");
    remove.addEventListener("click", async () => {
      try {
        const msg = await api.muLyricsSave(this.path, null);
        Toast.success(msg, 8000);
        this.close();
      } catch (err) {
        Toast.error(`Remove failed: ${errText(err)}`);
      }
    });
    const close = el("button", "pill-btn", "Close");
    close.addEventListener("click", () => this.close());
    foot.append(fetchBtn, save, remove, close);
  }
}

// ---------------------------------------------------------------------
// Preferences → Music Completion
// ---------------------------------------------------------------------

const DAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

export class MusicCompleteSection {
  private settings: MuSettings | null = null;
  private status: MuStatus | null = null;
  private host!: HTMLElement;
  private note!: HTMLElement;
  private savedTimer: number | undefined;
  private pendingArm = new Map<string, ReturnType<typeof setTimeout>>();
  private runsHost: HTMLElement | null = null;

  async mount(host: HTMLElement): Promise<void> {
    this.host = host;
    host.innerHTML = "";
    const section = el("section", "prefs-section");
    section.appendChild(el("h3", "", "Music Completion"));
    section.appendChild(
      el(
        "p",
        "prefs-note",
        "Fable can finish a music library the way a good record-store clerk would: it reads what's already in each file, looks the track up, and fills in ONLY what's missing — title, artist, album, year, track numbers, genre, composer — plus the cover art and the lyrics (timed, when timed lyrics exist, so players can highlight each line as it's sung). Everything is a proposal first: you always see the before/after, nothing that already says something is ever changed without you ticking that exact line, and every batch is journaled so it can be undone right here.",
      ),
    );
    const guide = el("div", "prefs-form");
    guide.innerHTML = `
      <p class="prefs-note">Where the answers come from — all free, no accounts, no keys:</p>
      <ul class="prefs-note">
        <li><a href="https://musicbrainz.org/" target="_blank" rel="noreferrer">MusicBrainz</a> — the open encyclopedia of music (the Wikipedia of albums, run by a nonprofit). Fable asks it politely, about one question a second, which is why a big folder takes a while.</li>
        <li><a href="https://coverartarchive.org/" target="_blank" rel="noreferrer">Cover Art Archive</a> — album artwork kept by MusicBrainz and the Internet Archive. If a match has a cover, Fable can embed it in the file. Art you already have always wins.</li>
        <li><a href="https://lrclib.net/" target="_blank" rel="noreferrer">LRCLIB</a> — a community lyrics library. It has plain lyrics and, for many songs, synced lyrics timed to the music.</li>
        <li><a href="https://acoustid.org/" target="_blank" rel="noreferrer">AcoustID</a> — the optional last resort: it identifies a track by HOW IT SOUNDS when names and tags are hopeless. It needs the free fpcalc helper tool and your own free AcoustID key — see "When names aren't enough" below. Without them, fingerprinting simply stays off and everything else still works.</li>
      </ul>
      <p class="prefs-note">Start it from the browser: right-click music files or a folder → Complete Music Tags & Lyrics…, or Organize ▸ Complete Music Tags & Lyrics…. To save asking file by file, schedules below can do it overnight — but only matches Fable is CONFIDENT about are completed unattended; the rest wait here as a morning checklist, never guessed.</p>`;
    wireGuideLinks(guide);
    section.appendChild(guide);
    const host2 = el("div", "");
    section.appendChild(host2);
    this.note = el("p", "prefs-note prefs-saved");
    section.appendChild(this.note);
    host.appendChild(section);
    try {
      this.settings = await api.getMuSettings();
      this.status = await api.getMuStatus();
      this.render(host2);
      await this.refreshRuns();
    } catch (err) {
      host2.appendChild(
        el("p", "prefs-note", `Could not load music settings: ${errText(err)}`),
      );
    }
  }

  private render(host: HTMLElement): void {
    const s = this.settings;
    if (!s) return;
    host.innerHTML = "";

    // --- Lyrics saving -------------------------------------------
    const lyr = el("div", "prefs-form");
    lyr.appendChild(el("h4", "", "Lyrics"));
    lyr.appendChild(
      el(
        "p",
        "prefs-note",
        "Where should saved lyrics live? Embedded travels with the file (best for players that read tags). A sidecar is a plain .lrc text file sitting next to the song — best for synced lyrics, because that's the file Navidrome, Jellyfin, and most karaoke-style players look for; synced lines make no sense embedded if your player can't read them there. Both = belt and suspenders.",
      ),
    );
    const targets: [string, string][] = [
      ["embedded", "Embedded in the file's tags"],
      ["sidecar", "Sidecar .lrc file"],
      ["both", "Both"],
    ];
    for (const [value, label] of targets) {
      const row = el("label", "mc-check");
      const radio = document.createElement("input");
      radio.type = "radio";
      radio.name = "mu-lyrics-target";
      radio.value = value;
      radio.checked = s.lyricsTarget === value;
      radio.addEventListener("change", () => {
        if (radio.checked) {
          s.lyricsTarget = value;
          void this.save();
        }
      });
      row.append(radio, el("span", "", label));
      lyr.appendChild(row);
    }
    const fetchLyr = el("label", "mc-check");
    const fl = document.createElement("input");
    fl.type = "checkbox";
    fl.checked = s.fetchLyrics;
    fl.addEventListener("change", () => {
      s.fetchLyrics = fl.checked;
      void this.save();
    });
    fetchLyr.append(fl, el("span", "", "Fetch lyrics (LRCLIB) when completing music"));
    lyr.appendChild(fetchLyr);
    const fetchArt = el("label", "mc-check");
    const fa = document.createElement("input");
    fa.type = "checkbox";
    fa.checked = s.fetchArtwork;
    fa.addEventListener("change", () => {
      s.fetchArtwork = fa.checked;
      void this.save();
    });
    fetchArt.append(
      fa,
      el("span", "", "Fetch missing cover art (Cover Art Archive) when completing music"),
    );
    lyr.appendChild(fetchArt);
    host.appendChild(lyr);

    this.renderAcoustid(host);
    this.renderSchedules(host);
    const runs = el("div", "prefs-form");
    runs.appendChild(el("h4", "", "Runs — the journal"));
    runs.appendChild(
      el(
        "p",
        "prefs-note",
        "Every completion batch is recorded with the exact tag values it replaced. Undo puts those values back, removes artwork and lyrics the run added, and deletes sidecars it created — sidecars and lyrics that were already yours are never touched.",
      ),
    );
    this.runsHost = el("div", "");
    runs.appendChild(this.runsHost);
    host.appendChild(runs);
    void this.refreshRuns();
  }

  private renderAcoustid(host: HTMLElement): void {
    const st = this.status;
    const box = el("div", "prefs-form");
    box.appendChild(el("h4", "", "When names aren't enough (AcoustID, optional)"));
    if (!st) {
      host.appendChild(box);
      return;
    }
    if (!st.fpcalcAvailable) {
      box.appendChild(
        el(
          "p",
          "prefs-note",
          "The fpcalc fingerprint tool isn't installed on this computer, so sound-based matching is off. That's fine — tags and filenames identify most libraries. fpcalc ships in the “libchromaprint-tools” package on Ubuntu/Zorin; install it and this section lights up.",
        ),
      );
    } else if (!st.acoustidKeySaved) {
      box.appendChild(
        el(
          "p",
          "prefs-note",
          "fpcalc is installed. It still needs a free AcoustID application key: sign in at acoustid.org, choose “Register your application” (a name is all it asks for), and paste the API key it gives you here. It lives in your system keyring, is used only to identify tracks by sound, and Fable shows you only whether one is saved.",
        ),
      );
    } else {
      box.appendChild(
        el(
          "p",
          "prefs-note",
          "Fingerprinting is ready: unidentified tracks can be matched by how they sound. The key is stored in your system keyring.",
        ),
      );
    }
    if (st.fpcalcAvailable) {
      const row = el("div", "prefs-form-row");
      const input = document.createElement("input");
      input.type = "password";
      input.placeholder = st.acoustidKeySaved
        ? "AcoustID key saved (keyring) — paste to replace"
        : "Paste your AcoustID API key";
      input.autocomplete = "off";
      row.appendChild(input);
      const saveKey = el("button", "pill-btn", "Save key");
      saveKey.addEventListener("click", async () => {
        const v = input.value.trim();
        if (!v) {
          Toast.error("Paste the key first, then Save.");
          return;
        }
        try {
          this.status = await api.setMuAcoustidKey(v);
          input.value = "";
          Toast.success(
            this.status.acoustidKeySaved
              ? "AcoustID key saved to your keyring and read back to prove it."
              : "The keyring would not confirm that save — the key was NOT kept.",
          );
          this.render(host);
        } catch (err) {
          Toast.error(`Could not save the key: ${errText(err)}`);
        }
      });
      row.appendChild(saveKey);
      if (st.acoustidKeySaved) {
        const remove = el("button", "pill-btn", "Remove key");
        remove.addEventListener("click", async () => {
          try {
            this.status = await api.setMuAcoustidKey(null);
            Toast.success("AcoustID key removed.");
            this.render(host);
          } catch (err) {
            Toast.error(`Could not remove the key: ${errText(err)}`);
          }
        });
        row.appendChild(remove);
      }
      box.appendChild(row);
    }
    if (!st.keyringOk) {
      box.appendChild(
        el(
          "p",
          "prefs-note",
          "Heads up: your system keyring isn't answering right now, so a key can't be saved until it's unlocked.",
        ),
      );
    }
    host.appendChild(box);
  }

  private renderSchedules(host: HTMLElement): void {
    const s = this.settings;
    if (!s) return;
    const wrap = el("div", "prefs-form");
    wrap.appendChild(el("h4", "", "Schedules — overnight completion"));
    wrap.appendChild(
      el(
        "p",
        "prefs-note",
        "Runs on the same background agent as Media Completion (install or enable it there; the agent must be on for any schedule to fire). A run scans the folder, completes only CONFIDENT matches end-to-end, and queues everything less certain in the run's checklist below for your morning yes/no. A missed run catches up once, flagged “ran late”.",
      ),
    );
    for (const sched of s.schedules) {
      const card = el("div", "mc-sched");
      const steps = [
        sched.steps.tags ? "tags" : null,
        sched.steps.lyrics ? "lyrics" : null,
        sched.steps.artwork ? "artwork" : null,
      ]
        .filter(Boolean)
        .join(" + ");
      const days = sched.weekdays.every(Boolean)
        ? "every day"
        : DAYS.filter((_, i) => sched.weekdays[i]).join(" ") || "every day";
      card.appendChild(
        el(
          "div",
          "mc-sched-title",
          `${sched.name} — ${steps} in ${sched.target}, ${days} at ${sched.time}${sched.enabled ? "" : " (paused)"}`,
        ),
      );
      card.appendChild(
        el(
          "p",
          "prefs-note",
          sched.lastStarted
            ? `Last ran ${sched.lastStarted} — ${sched.lastStatus ?? ""}`
            : "Hasn't run yet.",
        ),
      );
      const row = el("div", "prefs-form-row");
      const runNow = el("button", "pill-btn", "Run now");
      runNow.addEventListener("click", async () => {
        runNow.toggleAttribute("disabled", true);
        try {
          const run = await api.muRunNow(sched.id);
          Toast.success(
            `“${sched.name}” finished: ${run.summary.filesCompleted ?? 0} completed, ${run.summary.pending ?? 0} waiting for you below.`,
            9000,
          );
          this.settings = await api.getMuSettings();
          this.render(host);
        } catch (err) {
          Toast.error(`Run failed: ${errText(err)}`);
        } finally {
          runNow.toggleAttribute("disabled", false);
        }
      });
      const pause = el(
        "button",
        "pill-btn",
        sched.enabled ? "Pause" : "Resume",
      );
      pause.addEventListener("click", async () => {
        sched.enabled = !sched.enabled;
        await this.save();
        this.render(host);
      });
      const del = el("button", "pill-btn", "Delete");
      del.addEventListener("click", async () => {
        s.schedules = s.schedules.filter((x) => x.id !== sched.id);
        await this.save();
        this.render(host);
      });
      row.append(runNow, pause, del);
      card.appendChild(row);
      wrap.appendChild(card);
    }

    // The add-schedule form.
    const form = el("div", "mc-sched");
    form.appendChild(el("div", "mc-sched-title", "New schedule"));
    const name = document.createElement("input");
    name.type = "text";
    name.placeholder = "Name — e.g. Music folder, nightly";
    const target = document.createElement("input");
    target.type = "text";
    target.placeholder = "Folder to complete — e.g. /home/you/Music";
    const time = document.createElement("input");
    time.type = "time";
    time.value = "02:30";
    const idle = document.createElement("input");
    idle.type = "number";
    idle.min = "0";
    idle.value = "10";
    idle.title = "Only run after the computer has been idle this many minutes (0 = run on time regardless)";
    const acRow = el("label", "mc-check");
    const ac = document.createElement("input");
    ac.type = "checkbox";
    acRow.append(ac, el("span", "", "Only when plugged in (safer for big libraries)"));
    const stepsHost = el("div", "prefs-form-row");
    const stepBoxes: Record<string, HTMLInputElement> = {};
    for (const [key, label] of [
      ["tags", "Tags"],
      ["lyrics", "Lyrics"],
      ["artwork", "Artwork"],
    ] as const) {
      const lab = el("label", "mc-check");
      const box = document.createElement("input");
      box.type = "checkbox";
      box.checked = true;
      stepBoxes[key] = box;
      lab.append(box, el("span", "", label));
      stepsHost.appendChild(lab);
    }
    const dayHost = el("div", "prefs-form-row");
    const dayBoxes: HTMLInputElement[] = [];
    DAYS.forEach((d) => {
      const lab = el("label", "mc-check");
      const box = document.createElement("input");
      box.type = "checkbox";
      box.checked = true;
      dayBoxes.push(box);
      lab.append(box, el("span", "", d));
      dayHost.appendChild(lab);
    });
    const addRow = el("div", "prefs-form-row");
    const add = el("button", "pill-btn primary", "Add schedule");
    add.addEventListener("click", async () => {
      if (!name.value.trim() || !target.value.trim()) {
        Toast.error("A schedule needs a name and a folder.");
        return;
      }
      const sched: MuSchedule = {
        id: `mu-${Date.now().toString(36)}`,
        name: name.value.trim(),
        target: target.value.trim(),
        steps: {
          tags: stepBoxes.tags.checked,
          lyrics: stepBoxes.lyrics.checked,
          artwork: stepBoxes.artwork.checked,
        },
        time: time.value || "02:30",
        weekdays: dayBoxes.map((b) => b.checked),
        idleMinutes: Number(idle.value) || 0,
        requireOnAc: ac.checked,
        enabled: true,
        lastStarted: null,
        lastStatus: null,
      };
      s.schedules.push(sched);
      await this.save();
      this.render(host);
      Toast.success("Schedule added — it rides the background agent (see Media Completion for the agent switch).");
    });
    addRow.appendChild(add);
    const idleLab = el("label", "prefs-note", "Idle minutes first: ");
    idleLab.appendChild(idle);
    form.append(name, target, time, stepsHost, dayHost, acRow, idleLab, addRow);
    wrap.appendChild(form);
    host.appendChild(wrap);
  }

  private async refreshRuns(): Promise<void> {
    if (!this.runsHost) return;
    const host = this.runsHost;
    host.innerHTML = "";
    let runs: MuRun[];
    try {
      runs = await api.muListRuns();
    } catch {
      host.appendChild(el("p", "prefs-note", "Runs could not be loaded."));
      return;
    }
    if (!runs.length) {
      host.appendChild(
        el(
          "p",
          "prefs-note",
          "No completions yet — finish some music and the journal entries land here.",
        ),
      );
      return;
    }
    for (const run of runs) host.appendChild(this.renderRun(run));
  }

  private renderRun(run: MuRun): HTMLElement {
    const card = el("div", "mc-run");
    const s = run.summary;
    const bits: string[] = [];
    if (s.error) bits.push(`error: ${s.error}`);
    if (s.filesCompleted !== undefined)
      bits.push(
        `${s.filesCompleted} completed · ${s.fieldsWritten ?? 0} tags · ${s.artworkEmbedded ?? 0} covers · ${s.lyricsSaved ?? 0} lyrics`,
      );
    if (run.wasLate) bits.push("ran late (caught up)");
    if (run.status) bits.push(run.status);
    card.appendChild(
      el(
        "div",
        "mc-run-head",
        `${run.scheduleName} — ${run.target} · ${run.started.replace("T", " ")}${bits.length ? " · " + bits.join(" · ") : ""}`,
      ),
    );
    for (const f of s.failures ?? []) {
      card.appendChild(el("p", "mc-pending-note", f));
    }
    // The morning checklist: less-than-confident matches wait here.
    for (const p of run.pending) {
      const line = el("div", "mc-pending");
      const title = p.candidate
        ? `${p.name} — looks like ${p.candidate.artist} “${p.candidate.title}”${p.candidate.album ? ` (${p.candidate.album})` : ""}${p.score !== null ? ` · ${Math.round(p.score * 100)}%` : ""}`
        : `${p.name} — ${p.state}`;
      line.appendChild(el("span", "mc-pending-name", title));
      const go = el("button", "pill-btn", "Complete it now");
      go.addEventListener("click", async () => {
        try {
          const msg = await api.muConfirmPending(run.id, p.path);
          Toast.success(msg, 8000);
          void this.refreshRuns();
        } catch (err) {
          Toast.error(`Could not complete: ${errText(err)}`);
        }
      });
      line.appendChild(go);
      card.appendChild(line);
    }
    const actions = el("div", "prefs-form-row");
    if (run.canUndo) {
      const undo = el("button", "pill-btn destructive", "Undo this run");
      undo.addEventListener("click", async () => {
        if (!this.pendingArm.has(run.id)) {
          undo.textContent = "Really undo? Tags, art and lyrics from this run are put back";
          undo.classList.add("armed");
          this.pendingArm.set(
            run.id,
            setTimeout(() => {
              this.pendingArm.delete(run.id);
              undo.textContent = "Undo this run";
              undo.classList.remove("armed");
            }, 4000),
          );
          return;
        }
        clearTimeout(this.pendingArm.get(run.id));
        this.pendingArm.delete(run.id);
        try {
          Toast.success(await api.muUndoRun(run.id), 8000);
          void this.refreshRuns();
        } catch (err) {
          Toast.error(`Undo failed: ${errText(err)}`);
        }
      });
      actions.appendChild(undo);
    }
    if (actions.childNodes.length) card.appendChild(actions);
    return card;
  }

  private async save(): Promise<void> {
    if (!this.settings) return;
    try {
      this.settings = await api.saveMuSettings(this.settings);
      window.clearTimeout(this.savedTimer);
      this.note.textContent = "Saved.";
      this.savedTimer = window.setTimeout(() => {
        this.note.textContent = "";
      }, 2500);
    } catch (err) {
      Toast.error(`Could not save music settings: ${errText(err)}`);
    }
  }
}
