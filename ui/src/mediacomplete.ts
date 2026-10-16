/**
 * Media Completion — posters, subtitles, and the overnight
 * scheduler (Dickles, 2026-10-15). The completion trio: Media
 * Resolution normalizes the title; this section fetches the poster
 * and the subtitles to go with it, and the background agent runs
 * the whole pipeline over a drive while the house sleeps.
 *
 * GUIDANCE LAW governs the OpenSubtitles copy: what it is, that the
 * account is free, how to get the key, and the honest truth about
 * the download allowance — searching is unlimited, downloading is
 * not, and Fable only ever reports the numbers OpenSubtitles
 * itself reports back. Secrets are one-way: sent to the keyring,
 * never read back.
 */
import {
  api,
  type McPendingItem,
  type McRun,
  type McSchedule,
  type McSettings,
  type McStatus,
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

const WEEKDAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

function quotaText(run: McRun): string | null {
  const q = run.quota;
  if (!q) return null;
  const bits: string[] = [];
  if (q.remaining !== null)
    bits.push(`OpenSubtitles reported ${q.remaining} download(s) remaining`);
  if (q.resetTime) bits.push(`window resets ${q.resetTime}`);
  if (q.message) bits.push(q.message);
  return bits.length ? bits.join(" · ") : null;
}

export class MediaCompleteSection {
  private body: HTMLElement | null = null;
  private settings: McSettings | null = null;
  private status: McStatus | null = null;
  private runs: McRun[] = [];

  async mount(host: HTMLElement): Promise<void> {
    host.appendChild(el("h3", "prefs-section", "Media Completion"));
    host.appendChild(
      el(
        "p",
        "prefs-note",
        "The rest of the trio: once a film or episode is identified, Fable can fetch its poster (and fanart) from TMDB and its subtitles from OpenSubtitles, filed beside the video exactly the way Plex, Emby and Jellyfin read them. Existing posters and subtitles are never overwritten. And with the background agent on, a scheduled run can complete a whole drive overnight — confident matches are handled end-to-end, anything less waits for you in the morning checklist. It never guesses at 3 AM.",
      ),
    );
    this.body = el("div", "mc-prefs");
    this.body.appendChild(el("p", "prefs-note", "Loading…"));
    host.appendChild(this.body);
    await this.reload();
  }

  private async reload(): Promise<void> {
    try {
      this.settings = await api.getMcSettings();
      this.status = await api.getMcStatus();
      this.runs = await api.mcListRuns();
      this.render();
    } catch (err) {
      if (this.body) {
        this.body.innerHTML = "";
        this.body.appendChild(
          el("p", "prefs-note", `Media Completion could not load: ${errText(err)}`),
        );
      }
    }
  }

  private async save(): Promise<void> {
    if (!this.settings) return;
    try {
      this.settings = await api.saveMcSettings(this.settings);
      this.status = await api.getMcStatus();
      this.render();
    } catch (err) {
      Toast.error(`Could not save: ${errText(err)}`);
    }
  }

  private render(): void {
    const body = this.body;
    const s = this.settings;
    const st = this.status;
    if (!body || !s || !st) return;
    body.innerHTML = "";
    body.appendChild(this.renderArtwork(s));
    body.appendChild(this.renderOpensubsPointer(st));
    body.appendChild(this.renderScheduler(s, st));
    body.appendChild(this.renderRuns());
  }

  // ---------------------------------------------------------------
  // Artwork + subtitle defaults
  // ---------------------------------------------------------------

  private renderArtwork(s: McSettings): HTMLElement {
    const block = el("div", "mr-tmdb");
    const head = el("h4", "mc-h", "Posters & subtitles on Organize");
    block.appendChild(head);
    block.appendChild(
      el(
        "p",
        "prefs-note",
        "When an Organize job files a film or episode, these ride along: the poster (TMDB w500) and backdrop become poster.jpg and fanart.jpg in the title's folder; season posters land in their Season folder. Subtitles are matched by the file's OpenSubtitles hash first — the hash is what makes a subtitle actually sync — and saved as “Title (Year).en.srt” (with .sdh / .forced labels when they apply). You can also fetch either one on its own from the Resolve dialog.",
      ),
    );
    const mk = (
      label: string,
      get: () => boolean,
      set: (v: boolean) => void,
    ): HTMLElement => {
      const row = el("label", "mc-check");
      const box = document.createElement("input");
      box.type = "checkbox";
      box.checked = get();
      box.addEventListener("change", () => {
        set(box.checked);
        void this.save();
      });
      row.append(box, el("span", "", label));
      return row;
    };
    block.appendChild(
      mk("Fetch artwork with every Organize", () => s.artworkOnOrganize, (v) => (s.artworkOnOrganize = v)),
    );
    block.appendChild(
      mk("Include fanart (backdrop) too", () => s.fanart, (v) => (s.fanart = v)),
    );
    block.appendChild(
      mk(
        "Fetch subtitles with every Organize (uses your OpenSubtitles allowance)",
        () => s.subtitlesOnOrganize,
        (v) => (s.subtitlesOnOrganize = v),
      ),
    );
    const langRow = el("div", "mr-keyrow");
    langRow.appendChild(el("span", "mr-status", "Subtitle language"));
    const lang = document.createElement("input");
    lang.className = "mr-in";
    lang.style.maxWidth = "90px";
    lang.value = s.subtitleLanguage;
    lang.placeholder = "en";
    const langSave = el("button", "pill-btn", "Save language");
    langSave.addEventListener("click", () => {
      s.subtitleLanguage = lang.value.trim().toLowerCase() || "en";
      void this.save();
    });
    langRow.append(lang, langSave);
    block.appendChild(langRow);
    return block;
  }

  // ---------------------------------------------------------------
  // OpenSubtitles account — moved to Preferences → Metadata
  // Accounts (2026-10-16 cleanup); the state truth stays visible
  // here, the credentials live with the other accounts.
  // ---------------------------------------------------------------

  private renderOpensubsPointer(st: McStatus): HTMLElement {
    const block = el("div", "mr-tmdb");
    block.appendChild(el("h4", "mc-h", "OpenSubtitles (subtitles)"));
    const creds = st.opensubs;
    block.appendChild(
      el(
        "p",
        "prefs-note",
        !creds.keyringOk
          ? "The system keyring is unavailable on this machine, so no OpenSubtitles account can be saved."
          : creds.hasApiKey
            ? `Account saved${creds.hasLogin ? " (API key + account login)" : " (API key only — subtitle downloads use the small anonymous allowance)"}. Manage it under Preferences → Metadata Accounts.`
            : "No account saved yet. Add one under Preferences → Metadata Accounts.",
      ),
    );
    return block;
  }

  // ---------------------------------------------------------------
  // Scheduler + agent
  // ---------------------------------------------------------------

  private renderScheduler(s: McSettings, st: McStatus): HTMLElement {
    const block = el("div", "mr-tmdb");
    block.appendChild(el("h4", "mc-h", "Scheduled completion (overnight)"));
    block.appendChild(
      el(
        "p",
        "prefs-note",
        "The background agent is a small headless copy of Fable that starts at login and runs with the window closed — it pops up nothing, ever. It watches the schedules below; when the machine was off at a scheduled time, the run catches up at the next start and the report admits it ran late. If a run is told to wait for idle time, it measures real keyboard/mouse idle when the xprintidle tool is present, and otherwise treats a quiet system (load average under 1) as idle. On-AC only matters for laptops — a desktop always reads as plugged in.",
      ),
    );
    const agentRow = el("div", "mr-keyrow");
    const toggle = el(
      "button",
      "pill-btn",
      s.agentEnabled ? "Turn the background agent off" : "Turn the background agent on",
    );
    toggle.addEventListener("click", async () => {
      try {
        const msg = await api.setMcAgent(!s.agentEnabled);
        Toast.success(msg, 8000);
        await this.reload();
      } catch (err) {
        Toast.error(`Agent change failed: ${errText(err)}`);
      }
    });
    const state = el(
      "span",
      "mr-status",
      st.agentInstall === "none"
        ? "Not installed."
        : `Installed (${st.agentInstall === "systemd" ? "systemd user service" : "login autostart entry"}) · ${st.agentRunning ? `running (pid ${st.agentPid ?? "?"})` : "not running right now"}${st.agentLastTick ? ` · last check ${st.agentLastTick}` : ""}`,
    );
    agentRow.append(toggle, state);
    block.appendChild(agentRow);
    if (st.nextRuns.length) {
      block.appendChild(
        el(
          "p",
          "prefs-note",
          `Next: ${st.nextRuns.map(([n, t]) => `${n} at ${t}`).join(" · ")}`,
        ),
      );
    }

    for (const sched of s.schedules) {
      block.appendChild(this.renderScheduleRow(s, sched));
    }
    block.appendChild(this.renderAddSchedule(s));
    return block;
  }

  private renderScheduleRow(s: McSettings, sched: McSchedule): HTMLElement {
    const row = el("div", "mc-sched");
    const head = el("div", "mc-sched-head");
    const enabled = document.createElement("input");
    enabled.type = "checkbox";
    enabled.checked = sched.enabled;
    enabled.title = "Enabled";
    enabled.addEventListener("change", () => {
      sched.enabled = enabled.checked;
      void this.save();
    });
    head.appendChild(enabled);
    head.appendChild(el("strong", "", sched.name));
    const steps = [
      sched.steps.organize ? "organize" : null,
      sched.steps.artwork ? "artwork" : null,
      sched.steps.subtitles ? "subtitles" : null,
    ]
      .filter(Boolean)
      .join(" + ");
    head.appendChild(
      el(
        "span",
        "mr-status",
        `${sched.target} · daily ${sched.time}${sched.idleMinutes ? ` · after ${sched.idleMinutes} idle min` : ""}${sched.requireOnAc ? " · on AC only" : ""} · ${steps || "no steps"}`,
      ),
    );
    row.appendChild(head);
    if (sched.lastStatus) {
      row.appendChild(
        el(
          "p",
          "prefs-note",
          `Last run: ${sched.lastStarted ?? "?"} — ${sched.lastStatus}`,
        ),
      );
    }
    const actions = el("div", "mr-keyrow");
    const runNow = el("button", "pill-btn", "Run now");
    runNow.addEventListener("click", async () => {
      runNow.textContent = "Running… (this can take a while)";
      runNow.toggleAttribute("disabled", true);
      try {
        const run = await api.mcRunNow(sched.id);
        Toast.success(
          `Run finished: ${run.summary.organized ?? 0} organized, ${run.summary.artworkPlaced ?? 0} artwork, ${run.summary.subtitlesPlaced ?? 0} subtitles, ${run.pending.length} waiting for you.`,
          9000,
        );
        await this.reload();
      } catch (err) {
        Toast.error(`Run failed: ${errText(err)}`);
        await this.reload();
      }
    });
    const del = el("button", "pill-btn", "Delete schedule");
    del.addEventListener("click", () => {
      s.schedules = s.schedules.filter((x) => x.id !== sched.id);
      void this.save();
    });
    actions.append(runNow, del);
    row.appendChild(actions);
    return row;
  }

  private renderAddSchedule(s: McSettings): HTMLElement {
    const form = el("div", "mc-add");
    form.appendChild(el("div", "mc-h", "Add a schedule"));
    const grid = el("div", "mrx-grid");
    const name = document.createElement("input");
    name.className = "mr-in";
    name.placeholder = "Name (e.g. Overnight films)";
    const target = document.createElement("input");
    target.className = "mr-in";
    target.placeholder = "Drive or folder to complete";
    const time = document.createElement("input");
    time.className = "mr-in";
    time.type = "time";
    time.value = "02:30";
    const idle = document.createElement("input");
    idle.className = "mr-in";
    idle.placeholder = "Idle minutes (0 = none)";
    idle.inputMode = "numeric";
    grid.append(name, target, time, idle);
    form.appendChild(grid);

    const checks = el("div", "mr-keyrow");
    const mkCheck = (label: string, initial: boolean): HTMLInputElement => {
      const wrap = el("label", "mc-check");
      const box = document.createElement("input");
      box.type = "checkbox";
      box.checked = initial;
      wrap.append(box, el("span", "", label));
      checks.appendChild(wrap);
      return box;
    };
    const cOrg = mkCheck("Organize", true);
    const cArt = mkCheck("Artwork", true);
    const cSub = mkCheck("Subtitles", true);
    const cAc = mkCheck("On AC only", false);
    form.appendChild(checks);

    const days = el("div", "mr-keyrow");
    days.appendChild(el("span", "mr-status", "Days (none ticked = every day)"));
    const dayBoxes: HTMLInputElement[] = [];
    for (const d of WEEKDAYS) {
      const wrap = el("label", "mc-check");
      const box = document.createElement("input");
      box.type = "checkbox";
      wrap.append(box, el("span", "", d));
      days.appendChild(wrap);
      dayBoxes.push(box);
    }
    form.appendChild(days);

    const add = el("button", "pill-btn primary", "Add schedule");
    add.addEventListener("click", () => {
      const n = name.value.trim();
      const t = target.value.trim();
      if (!n || !t) {
        Toast.error("A schedule needs a name and a target folder.");
        return;
      }
      s.schedules.push({
        id: crypto.randomUUID(),
        name: n,
        target: t,
        steps: {
          organize: cOrg.checked,
          artwork: cArt.checked,
          subtitles: cSub.checked,
        },
        time: time.value || "02:30",
        weekdays: dayBoxes.map((b) => b.checked),
        idleMinutes: parseInt(idle.value, 10) || 0,
        requireOnAc: cAc.checked,
        enabled: true,
        lastStarted: null,
        lastStatus: null,
      });
      void this.save();
    });
    form.appendChild(add);
    return form;
  }

  // ---------------------------------------------------------------
  // Runs: the morning report + confirmation checklist
  // ---------------------------------------------------------------

  private renderRuns(): HTMLElement {
    const block = el("div", "mr-tmdb");
    block.appendChild(el("h4", "mc-h", "Run history & morning checklist"));
    if (!this.runs.length) {
      block.appendChild(
        el("p", "prefs-note", "No runs yet — a scheduled run (or “Run now”) will leave its report here."),
      );
      return block;
    }
    for (const run of this.runs) {
      block.appendChild(this.renderRun(run));
    }
    return block;
  }

  private renderRun(run: McRun): HTMLElement {
    const wrap = el("div", "mc-run");
    const head = el("div", "mc-run-head");
    head.appendChild(el("strong", "", run.scheduleName));
    head.appendChild(
      el(
        "span",
        "mr-status",
        `${run.started} · ${run.status.replace(/_/g, " ")}${run.wasLate ? " · RAN LATE (machine was off)" : ""}`,
      ),
    );
    wrap.appendChild(head);
    const sm = run.summary;
    if (sm.error) {
      wrap.appendChild(el("p", "mrx-note", `Failed: ${sm.error}`));
    } else if (sm.scanned !== undefined) {
      wrap.appendChild(
        el(
          "p",
          "prefs-note",
          `Scanned ${sm.scanned} · organized ${sm.organized ?? 0} · artwork placed ${sm.artworkPlaced ?? 0} · subtitles placed ${sm.subtitlesPlaced ?? 0} · waiting for you ${run.pending.length}${sm.truncated ? " · (scan capped at 500 files)" : ""}`,
        ),
      );
    }
    const q = quotaText(run);
    if (q) wrap.appendChild(el("p", "prefs-note", q));
    for (const f of sm.failures ?? []) {
      wrap.appendChild(el("p", "mrx-note", `✗ ${f}`));
    }
    for (const item of run.pending) {
      wrap.appendChild(this.renderPending(run, item));
    }
    if (run.canUndo) {
      const undo = el("button", "pill-btn", "Undo this run");
      let armed = false;
      undo.addEventListener("click", async () => {
        if (!armed) {
          armed = true;
          undo.textContent = "Click again to really undo it";
          return;
        }
        try {
          const msg = await api.mcUndoRun(run.id);
          Toast.success(msg, 8000);
          await this.reload();
        } catch (err) {
          Toast.error(`Undo failed: ${errText(err)}`);
        }
      });
      wrap.appendChild(undo);
    }
    return wrap;
  }

  private renderPending(run: McRun, item: McPendingItem): HTMLElement {
    const row = el("div", "mc-pending");
    const guess =
      item.identity ??
      (item.candidates.length
        ? {
            title: item.candidates[0].candidate.title,
            year: item.candidates[0].candidate.year,
            tmdbId: item.candidates[0].candidate.id,
            mediaType:
              item.candidates[0].candidate.media_type === "tv"
                ? "episode"
                : "movie",
          }
        : null);
    row.appendChild(el("div", "mrx-name", item.name));
    row.appendChild(
      el(
        "div",
        "mrx-parsed",
        guess
          ? `Best guess (not applied): ${guess.title}${guess.year ? ` (${guess.year})` : ""} — confirm it and the file completes on the next run or a manual Organize.`
          : "No confident match — resolve it from the file's right-click “Resolve for Media Servers” dialog.",
      ),
    );
    if (guess && item.identity?.confirmed !== true) {
      const btn = el("button", "pill-btn", "Confirm this match");
      btn.addEventListener("click", async () => {
        try {
          await api.mcConfirmPending(run.id, item.path, {
            mediaType: guess.mediaType,
            tmdbId: guess.tmdbId ?? null,
            title: guess.title,
            year: guess.year ?? null,
            season: item.identity?.season ?? item.parsed.season,
            episode: item.identity?.episode ?? item.parsed.episode_first,
            episodeEnd: item.identity?.episodeEnd ?? item.parsed.episode_last,
            episodeTitles: item.identity?.episodeTitles ?? [],
          });
          Toast.success(`Confirmed ${item.name}.`);
          await this.reload();
        } catch (err) {
          Toast.error(`Could not confirm: ${errText(err)}`);
        }
      });
      row.appendChild(btn);
    }
    return row;
  }
}

// ---------------------------------------------------------------------
// The OpenSubtitles account (Preferences → Metadata Accounts, Movies
// & TV group). Split out of MediaCompleteSection in the 2026-10-16
// Preferences cleanup — same controls, same commands; the account's
// home is with the other accounts.
// ---------------------------------------------------------------------

// ---------------------------------------------------------------
// OpenSubtitles account (guidance-law block)
// ---------------------------------------------------------------

function buildOpenSubtitlesBlock(st: McStatus, reload: () => Promise<void>): HTMLElement {
  const block = el("div", "mr-tmdb");
  block.appendChild(el("h4", "mc-h", "OpenSubtitles (subtitles)"));
  const p1 = document.createElement("p");
  p1.innerHTML =
    "<strong>Free account, real limits.</strong> OpenSubtitles is the big community subtitle library. Searching it is unlimited, but <strong>downloading is limited per day</strong> — with just an API key it's 5 downloads per day per address; your free account's own allowance is larger but isn't a fixed published number, so Fable never prints one: after every download it shows you the exact remaining count OpenSubtitles reports back. That's why subtitles on Organize are opt-in, and why a scheduled run stops fetching subtitles when the allowance runs out and says so in the morning report.";
  block.appendChild(p1);
  const p2 = document.createElement("p");
  p2.innerHTML =
    "To set it up: 1. Create a free account at <a class=\"guide-link mr-mono\" href=\"https://www.opensubtitles.com/en/users/sign_up\">opensubtitles.com</a>. 2. Open <a class=\"guide-link mr-mono\" href=\"https://www.opensubtitles.com/en/consumers\">Profile → API</a> (the API consumers page) and create a consumer — any name works, e.g. “Fable Files” — and copy its API key. 3. Paste the API key below, plus the username and password you log in with (the login is what spends your account's allowance instead of the tiny anonymous one). All three live only in your system keyring — never in a settings file, a job, or a log.";
  block.appendChild(p2);
  wireGuideLinks(block);
  const creds = st.opensubs;
  const statusLine = el(
    "p",
    "mr-status",
    !creds.keyringOk
      ? "The system keyring is unavailable on this machine."
      : creds.hasApiKey
        ? `Saved: API key${creds.hasLogin ? " + account login" : " (no account login yet — downloads use the anonymous allowance)"}.`
        : "Nothing saved yet.",
  );
  block.appendChild(statusLine);

  const mkSecret = (
    kind: string,
    label: string,
    placeholder: string,
    password: boolean,
  ): HTMLElement => {
    const row = el("div", "mr-keyrow");
    const input = document.createElement("input");
    input.className = "mr-in mr-key";
    input.type = password ? "password" : "text";
    input.placeholder = placeholder;
    input.setAttribute("aria-label", label);
    const save = el("button", "pill-btn", `Save ${label}`);
    save.addEventListener("click", async () => {
      const v = input.value.trim();
      if (!v) return;
      try {
        await api.opensubsSetSecret(kind, v);
        input.value = "";
        Toast.success(`${label} saved to the keyring.`);
        await reload();
      } catch (err) {
        Toast.error(`Could not save: ${errText(err)}`);
      }
    });
    const clear = el("button", "pill-btn", "Remove");
    clear.addEventListener("click", async () => {
      try {
        await api.opensubsSetSecret(kind, null);
        await reload();
      } catch (err) {
        Toast.error(`Could not remove: ${errText(err)}`);
      }
    });
    row.append(input, save, clear);
    return row;
  };
  block.appendChild(mkSecret("opensubs-apikey", "API key", "OpenSubtitles API key", true));
  block.appendChild(mkSecret("opensubs-username", "username", "OpenSubtitles username", false));
  block.appendChild(mkSecret("opensubs-password", "password", "OpenSubtitles password", true));

  const validateRow = el("div", "mr-keyrow");
  const validate = el("button", "pill-btn", "Check account");
  const vOut = el("span", "mr-status");
  validate.addEventListener("click", async () => {
    vOut.textContent = "Checking…";
    try {
      const r = await api.opensubsValidate();
      vOut.textContent = r.message;
    } catch (err) {
      vOut.textContent = `Check failed: ${errText(err)}`;
    }
  });
  validateRow.append(validate, vOut);
  block.appendChild(validateRow);
  return block;
}


export class OpenSubtitlesAccountSection {
  private body: HTMLElement | null = null;
  private status: McStatus | null = null;

  async mount(host: HTMLElement): Promise<void> {
    this.body = el("div", "mc-prefs");
    this.body.appendChild(el("p", "prefs-note", "Loading…"));
    host.appendChild(this.body);
    await this.reload();
  }

  private async reload(): Promise<void> {
    try {
      this.status = await api.getMcStatus();
      this.render();
    } catch (err) {
      if (this.body) {
        this.body.innerHTML = "";
        this.body.appendChild(
          el("p", "prefs-note", `OpenSubtitles account could not load: ${errText(err)}`),
        );
      }
    }
  }

  private render(): void {
    if (!this.body || !this.status) return;
    this.body.innerHTML = "";
    this.body.appendChild(
      buildOpenSubtitlesBlock(this.status, () => this.reload()),
    );
  }
}
