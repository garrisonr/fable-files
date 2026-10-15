/**
 * RomMedia — the "Game Media" surface of Preferences + the
 * missing-media report (ROM media enrichment, Dickles 2026-10-14).
 *
 * The GUIDANCE LAW (DESIGN.md) governs this file's copy: the feature
 * TEACHES, it does not just fetch. Every source block says what it
 * is, what it costs, and how to get the account or key, in plain
 * language. The free path is first-class: a user who pays nothing
 * and signs up for nothing still ends up with a pretty library
 * (Libretro) or a clear do-it-yourself path (the report).
 *
 * Storage truth this UI must never hide: settings live in the
 * backend (rom-media.json); credentials live ONLY in the OS
 * keyring — this file never sees a saved secret again after sending
 * it, only whether one exists.
 */
import {
  api,
  type RomMediaSettings,
  type RomMediaSettingsView,
  type RomMediaSystemReport,
} from "./api";
import { Toast, errText } from "./toast";

export interface RomMediaHooks {
  pickFolder(initial: string | null): Promise<string | null>;
}

const TYPE_ROWS: Array<{
  key: "boxart" | "screenshots" | "titles" | "trailers";
  type: string;
  label: string;
  blurb: string;
}> = [
  {
    key: "boxart",
    type: "boxart",
    label: "Box art & posters",
    blurb: "The game's box, leading your collections.",
  },
  {
    key: "screenshots",
    type: "screenshot",
    label: "Screenshots",
    blurb: "A shot of the game being played.",
  },
  {
    key: "titles",
    type: "titleshot",
    label: "Title shots",
    blurb: "The game's title screen.",
  },
  {
    key: "trailers",
    type: "trailer",
    label: "Trailers & video clips",
    blurb:
      "Short gameplay videos. The big one — a full system's worth can be gigabytes, so it starts off and asks about quality first.",
  },
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

export class RomMediaSection {
  private view: RomMediaSettingsView | null = null;
  private hooks: RomMediaHooks | null = null;
  private body: HTMLElement | null = null;

  async mount(host: HTMLElement, hooks: RomMediaHooks): Promise<void> {
    this.hooks = hooks;
    host.appendChild(el("h3", "prefs-section", "Game Media"));
    const intro = el(
      "p",
      "prefs-note",
      "Make your game collections beautiful — box art, screenshots, title screens and trailers, fetched once and filed where your emulators look for them too, so nobody does this work twice.",
    );
    host.appendChild(intro);
    this.body = el("div", "rm-body");
    this.body.appendChild(el("p", "prefs-note", "Loading…"));
    host.appendChild(this.body);
    try {
      this.view = await api.getRomMediaSettings();
      this.render();
    } catch (err) {
      this.body.innerHTML = "";
      this.body.appendChild(
        el("p", "prefs-note", `Game media settings could not load: ${errText(err)}`),
      );
    }
  }

  private settings(): RomMediaSettings {
    return this.view!.settings;
  }

  private async save(): Promise<void> {
    try {
      await api.setRomMediaSettings(this.settings());
    } catch (err) {
      Toast.error(`Could not save media settings: ${errText(err)}`);
    }
  }

  private render(): void {
    const body = this.body;
    const view = this.view;
    if (!body || !view) return;
    body.innerHTML = "";
    const s = this.settings();

    // --- Where it all lives -------------------------------------------
    body.appendChild(
      this.pathRow(
        "Media folder",
        "Everything below lives here, arranged the way ES-DE and RetroArch expect: one folder per system, one per kind of media.",
        s.mediaRoot,
        async (path) => {
          if (path) s.mediaRoot = path;
          await this.save();
          this.render();
        },
      ),
    );
    body.appendChild(
      this.pathRow(
        "RetroArch thumbnails folder",
        "Optional. Point at RetroArch's thumbnails folder and Fable keeps a second copy of the pictures there (linked where the drive allows, so it costs almost no space) — RetroArch then shows the same art without downloading anything.",
        s.retroarchDir,
        async (path) => {
          s.retroarchDir = path;
          await this.save();
          this.render();
        },
        true,
      ),
    );

    // --- The four kinds ------------------------------------------------
    for (const row of TYPE_ROWS) {
      body.appendChild(this.typeRow(row.key, row.type, row.label, row.blurb));
    }

    // --- Sources --------------------------------------------------------
    const srcHead = el("h4", "prefs-subhead", "Where it comes from");
    body.appendChild(srcHead);
    body.appendChild(
      el(
        "p",
        "prefs-note",
        "Fable asks these in order and stops at the first one that has your game. Free and keyless first, always.",
      ),
    );
    body.appendChild(this.libretroBlock());
    body.appendChild(this.screenscraperBlock());
    body.appendChild(this.emumoviesBlock());
    body.appendChild(
      this.keyBlock(
        "thegamesdb",
        "TheGamesDB",
        view.secretPresent["thegamesdb"] ?? false,
        "A second opinion for box art and screenshots. Keys are free but human-issued: ask on the TheGamesDB forums (Developers → API key requests) — it can take a little while — then paste the key here. It comes with a monthly request budget, and Fable shows it as it spends.",
        "Paste API key",
      ),
    );
    body.appendChild(
      this.keyBlock(
        "steamgriddb",
        "SteamGridDB",
        view.secretPresent["steamgriddb"] ?? false,
        "Extra-pretty poster art for games the other sources miss. Free account at steamgriddb.com → your profile → Preferences → API → create a key, then paste it here.",
        "Paste API key",
      ),
    );

    // --- The free path, stated plainly (guidance law) ------------------
    const free = el("div", "rm-freebox");
    free.appendChild(
      el(
        "p",
        "rm-freebox-title",
        "None of these accounts are required.",
      ),
    );
    free.appendChild(
      el(
        "p",
        "prefs-note",
        "Libretro above is free and needs no sign-up, and it covers most games. And you can always file your own art and videos by hand: the media report (below) shows exactly which folder each system uses and how files must be named, and Fable — and your emulators — will just find them. Hunting a particular art or video set? Ask your favorite Grizzly Bear AI where to find it, then drop the files in.",
      ),
    );
    body.appendChild(free);

    // --- Actions ---------------------------------------------------------
    const actions = el("div", "rm-actions");
    const dl = el("button", "pill-btn primary", "Download missing media");
    dl.addEventListener("click", () => {
      void startMediaDownload(null, null);
    });
    const report = el("button", "pill-btn", "Missing-media report…");
    report.addEventListener("click", () => {
      openRomMediaReport(null);
    });
    actions.append(dl, report);
    body.appendChild(actions);

    if (!view.keyringOk) {
      body.appendChild(
        el(
          "p",
          "prefs-note rm-warn",
          "Heads up: the system keyring is not answering right now, so account details cannot be saved on this machine. Downloads from Libretro (no account) still work.",
        ),
      );
    }
  }

  private pathRow(
    label: string,
    note: string,
    value: string | null,
    apply: (path: string | null) => Promise<void>,
    clearable = false,
  ): HTMLElement {
    const wrap = el("div", "rm-pathrow");
    const lab = el("span", "prefs-row-label", label);
    const val = el(
      "span",
      "rm-path-value",
      value && value.trim() !== "" ? value : "Not set",
    );
    const choose = el("button", "pill-btn", "Choose…");
    choose.addEventListener("click", () => {
      void this.hooks?.pickFolder(value).then((p) => {
        if (p) void apply(p);
      });
    });
    wrap.append(lab, val, choose);
    if (clearable && value) {
      const clear = el("button", "pill-btn", "Clear");
      clear.addEventListener("click", () => void apply(null));
      wrap.appendChild(clear);
    }
    const n = el("p", "prefs-note", note);
    const outer = el("div", "rm-block");
    outer.append(wrap, n);
    return outer;
  }

  private typeRow(
    key: "boxart" | "screenshots" | "titles" | "trailers",
    type: string,
    label: string,
    blurb: string,
  ): HTMLElement {
    const s = this.settings();
    const ts = s[key];
    const outer = el("div", "rm-block");
    const row = el("label", "prefs-row prefs-toggle");
    const box = document.createElement("input");
    box.type = "checkbox";
    box.checked = ts.enabled;
    box.addEventListener("change", () => {
      ts.enabled = box.checked;
      void this.save().then(() => this.render());
    });
    const text = el("span", "prefs-row-label", label);
    row.append(box, text);
    outer.appendChild(row);
    outer.appendChild(el("p", "prefs-note", blurb));
    if (ts.enabled && type === "trailer") {
      const tierRow = el("div", "rm-tier-row");
      tierRow.appendChild(el("span", "prefs-row-label", "Video quality"));
      const sel = document.createElement("select");
      sel.className = "rm-select";
      const opts: Array<[string, string]> = [
        ["small", "Small (240p) — kindest to disk"],
        ["standard", "Standard (480p) — about 3× the space"],
        ["hd", "HD (1080p) — the full banquet"],
      ];
      for (const [v, t] of opts) {
        const o = document.createElement("option");
        o.value = v;
        o.textContent = t;
        o.selected = s.trailerTier === v;
        sel.appendChild(o);
      }
      sel.addEventListener("change", () => {
        s.trailerTier = sel.value;
        void this.save();
      });
      tierRow.appendChild(sel);
      outer.appendChild(tierRow);
    }
    if (ts.enabled) {
      const loc = el("div", "rm-override-row");
      const where =
        ts.dirOverride && ts.dirOverride.trim() !== ""
          ? `Stored separately: ${ts.dirOverride}`
          : "Stored with everything else, in the media folder.";
      loc.appendChild(el("span", "prefs-note", where));
      const choose = el("button", "pill-btn", "Store elsewhere…");
      choose.addEventListener("click", () => {
        void this.hooks?.pickFolder(ts.dirOverride).then((p) => {
          if (p) {
            ts.dirOverride = p;
            void this.save().then(() => this.render());
          }
        });
      });
      loc.appendChild(choose);
      if (ts.dirOverride) {
        const back = el("button", "pill-btn", "Use the media folder");
        back.addEventListener("click", () => {
          ts.dirOverride = null;
          void this.save().then(() => this.render());
        });
        loc.appendChild(back);
      }
      outer.appendChild(loc);
      outer.appendChild(
        el(
          "p",
          "prefs-note",
          "Sending one kind elsewhere splits it onto that drive; Fable links it back into the media folder so frontends still see one tree. On drives that cannot hold links (exFAT, most big USB drives), the link becomes a real second copy — that costs the space twice, and it is the drive's rule, not ours.",
        ),
      );
    }
    return outer;
  }

  private statusLine(text: string): HTMLElement {
    return el("p", "rm-status", text);
  }

  private libretroBlock(): HTMLElement {
    const outer = el("div", "rm-source");
    outer.appendChild(el("h5", "rm-source-name", "Libretro"));
    outer.appendChild(
      this.statusLine("Always on · free · no account"),
    );
    outer.appendChild(
      el(
        "p",
        "prefs-note",
        "Box art, screenshots and title screens, named exactly like your verified games — the same picture library RetroArch itself uses. This one source makes most collections respectable before you sign up for anything.",
      ),
    );
    return outer;
  }

  private credentialFields(
    userPlaceholder: string,
    passPlaceholder: string,
  ): { wrap: HTMLElement; user: HTMLInputElement; pass: HTMLInputElement } {
    const wrap = el("div", "rm-cred-row");
    const user = document.createElement("input");
    user.type = "text";
    user.className = "rm-input";
    user.placeholder = userPlaceholder;
    user.autocomplete = "off";
    const pass = document.createElement("input");
    pass.type = "password";
    pass.className = "rm-input";
    pass.placeholder = passPlaceholder;
    pass.autocomplete = "off";
    wrap.append(user, pass);
    return { wrap, user, pass };
  }

  /** Account block with Connect: saves to the keyring, then asks the
   * backend to verify, and shows the plain-language verdict inline. */
  private accountBlock(opts: {
    kind: string;
    name: string;
    connectedUser: string | null;
    hasSecret: boolean;
    teacher: string;
    userPlaceholder: string;
    onConnectedUser: (user: string | null) => void;
    extra?: (host: HTMLElement) => void;
  }): HTMLElement {
    const outer = el("div", "rm-source");
    outer.appendChild(el("h5", "rm-source-name", opts.name));
    const status = this.statusLine(
      opts.hasSecret && opts.connectedUser
        ? `Signed in as ${opts.connectedUser}`
        : opts.hasSecret
          ? "Details saved — Connect again to check them"
          : "Not connected",
    );
    outer.appendChild(status);
    outer.appendChild(el("p", "prefs-note", opts.teacher));
    const { wrap, user, pass } = this.credentialFields(
      opts.userPlaceholder,
      "Password",
    );
    outer.appendChild(wrap);
    const verdict = el("p", "rm-verdict");
    verdict.hidden = true;
    const btnRow = el("div", "rm-cred-row");
    const connect = el("button", "pill-btn primary", "Connect");
    connect.addEventListener("click", () => {
      void (async () => {
        const u = user.value.trim();
        const p = pass.value;
        if (u === "" && p === "") {
          // Nothing typed: just re-validate what's saved.
        } else if (u === "" || p === "") {
          verdict.hidden = false;
          verdict.textContent = "Both the user name and the password, please — one without the other cannot sign in.";
          return;
        } else {
          try {
            await api.setRomMediaSecret(
              opts.kind,
              JSON.stringify({ user: u, pass: p }),
            );
            opts.onConnectedUser(u);
            await this.save();
          } catch (err) {
            verdict.hidden = false;
            verdict.textContent = `Could not save those details: ${errText(err)}`;
            return;
          }
        }
        connect.textContent = "Checking…";
        try {
          const v = await api.validateRomMediaSource(opts.kind);
          verdict.hidden = false;
          verdict.textContent = v.message;
          status.textContent = v.ok
            ? `Signed in as ${opts.connectedUser ?? u}`
            : "Not connected";
        } catch (err) {
          verdict.hidden = false;
          verdict.textContent = `The check itself failed: ${errText(err)}`;
        } finally {
          connect.textContent = "Connect";
        }
      })();
    });
    btnRow.appendChild(connect);
    if (opts.hasSecret) {
      const forget = el("button", "pill-btn", "Forget");
      forget.addEventListener("click", () => {
        void (async () => {
          await api.setRomMediaSecret(opts.kind, null);
          opts.onConnectedUser(null);
          await this.save();
          this.render();
          Toast.info(`${opts.name} details removed from the keyring.`);
        })();
      });
      btnRow.appendChild(forget);
    }
    outer.appendChild(btnRow);
    outer.appendChild(verdict);
    opts.extra?.(outer);
    return outer;
  }

  private screenscraperBlock(): HTMLElement {
    const view = this.view!;
    const s = this.settings();
    return this.accountBlock({
      kind: "screenscraper",
      name: "ScreenScraper",
      connectedUser: s.screenscraperUser,
      hasSecret: view.secretPresent["screenscraper"] ?? false,
      teacher:
        "The deep well: 3D boxes, logos, manuals, video clips — and it finds games by their fingerprint (the same hashes Fable already computed), so the Japanese box lands on the Japanese game. A free account lifts you past the anonymous slow lane: one thread and busy-hour refusals become a roomy daily allowance. No account yet? It is free — screenscraper.fr → create an account → come back with the name and password. One honest wrinkle: ScreenScraper also asks each app to identify itself with developer credentials; Fable's own registration is in progress, and until it lands this source can only switch on with developer credentials of your own (free to request on their site) — pasted under Advanced. Nothing else on this page waits for that.",
      userPlaceholder: "ScreenScraper user name",
      onConnectedUser: (u) => {
        s.screenscraperUser = u;
      },
      extra: (host) => {
        const details = document.createElement("details");
        details.className = "rm-advanced";
        const sum = document.createElement("summary");
        sum.textContent = "Advanced: your own ScreenScraper developer credentials";
        details.appendChild(sum);
        details.appendChild(
          el(
            "p",
            "prefs-note",
            "If you hold free ScreenScraper developer credentials (devid + devpassword), paste them here and the source lights up today. They identify the software, not you, and they live in the keyring like everything else.",
          ),
        );
        const { wrap, user, pass } = this.credentialFields("Developer id", "Developer password");
        details.appendChild(wrap);
        const save = el("button", "pill-btn", "Save developer credentials");
        save.addEventListener("click", () => {
          void (async () => {
            if (user.value.trim() === "" || pass.value === "") {
              Toast.error("Both the developer id and its password, please.");
              return;
            }
            await api.setRomMediaSecret(
              "screenscraper-dev",
              JSON.stringify({ user: user.value.trim(), pass: pass.value }),
            );
            Toast.success("Developer credentials saved — ScreenScraper can switch on now.");
            this.render();
          })();
        });
        details.appendChild(save);
        if (view.secretPresent["screenscraper-dev"]) {
          details.appendChild(
            el("p", "rm-status", "Developer credentials: saved"),
          );
        }
        host.appendChild(details);
      },
    });
  }

  private emumoviesBlock(): HTMLElement {
    const view = this.view!;
    const s = this.settings();
    return this.accountBlock({
      kind: "emumovies",
      name: "EmuMovies",
      connectedUser: s.emumoviesUser,
      hasSecret: view.secretPresent["emumovies"] ?? false,
      teacher:
        "The trailer library — the deepest collection of game videos in the hobby, in small, standard and HD sizes. You sign in with the name on your EmuMovies account. Free accounts browse the website; paid membership opens the file server Fable connects to, with full-quality video and no download limits — Connect will tell you honestly which door your account opens.",
      userPlaceholder: "EmuMovies user name",
      onConnectedUser: (u) => {
        s.emumoviesUser = u;
      },
    });
  }

  private keyBlock(
    kind: string,
    name: string,
    hasSecret: boolean,
    teacher: string,
    placeholder: string,
  ): HTMLElement {
    const outer = el("div", "rm-source");
    outer.appendChild(el("h5", "rm-source-name", name));
    const status = this.statusLine(hasSecret ? "Key saved" : "Not connected");
    outer.appendChild(status);
    outer.appendChild(el("p", "prefs-note", teacher));
    const row = el("div", "rm-cred-row");
    const input = document.createElement("input");
    input.type = "password";
    input.className = "rm-input rm-input-wide";
    input.placeholder = placeholder;
    input.autocomplete = "off";
    row.appendChild(input);
    const verdict = el("p", "rm-verdict");
    verdict.hidden = true;
    const connect = el("button", "pill-btn primary", "Connect");
    connect.addEventListener("click", () => {
      void (async () => {
        const v = input.value.trim();
        if (v !== "") {
          try {
            await api.setRomMediaSecret(kind, v);
          } catch (err) {
            verdict.hidden = false;
            verdict.textContent = `Could not save that key: ${errText(err)}`;
            return;
          }
        }
        connect.textContent = "Checking…";
        try {
          const res = await api.validateRomMediaSource(kind);
          verdict.hidden = false;
          verdict.textContent = res.message;
          status.textContent = res.ok ? "Key saved" : "Not connected";
        } catch (err) {
          verdict.hidden = false;
          verdict.textContent = `The check itself failed: ${errText(err)}`;
        } finally {
          connect.textContent = "Connect";
        }
      })();
    });
    row.appendChild(connect);
    if (hasSecret) {
      const forget = el("button", "pill-btn", "Forget");
      forget.addEventListener("click", () => {
        void (async () => {
          await api.setRomMediaSecret(kind, null);
          this.render();
          Toast.info(`${name} key removed from the keyring.`);
        })();
      });
      row.appendChild(forget);
    }
    outer.appendChild(row);
    outer.appendChild(verdict);
    return outer;
  }
}

/** Start a media download job (all systems, or one) and toast the
 * outcome in plain language — including the "nothing to fetch"
 * truth, which arrives as an error string from the planner. */
export async function startMediaDownload(
  systemKey: string | null,
  systemLabel: string | null,
): Promise<void> {
  try {
    const start = await api.startRomMediaDownload({
      systems: systemKey ? [systemKey] : null,
      systemLabel,
      retry: [],
    });
    if (start.alreadyRunning) {
      Toast.info("That download is already running — watch it in the jobs panel.");
    } else {
      Toast.success("Media download started — watch it in the jobs panel.");
    }
  } catch (err) {
    Toast.info(errText(err));
  }
}

// ---------------------------------------------------------------------------
// The missing-media report: per system, what lacks art/snaps/video,
// why, per-item retry, one-click ambiguity picks, and the DIY note.
// ---------------------------------------------------------------------------

const TYPE_LABELS: Record<string, string> = {
  boxart: "Box art",
  screenshot: "Screenshot",
  titleshot: "Title shot",
  trailer: "Trailer",
};

export function openRomMediaReport(systemKey: string | null): void {
  document.querySelector(".rm-report-overlay")?.remove();
  const overlay = el("div", "props-overlay rm-report-overlay");
  overlay.addEventListener("mousedown", (e) => {
    if (e.target === overlay) overlay.remove();
  });
  const dialog = el("div", "prefs-dialog rm-report-dialog");
  dialog.setAttribute("role", "dialog");
  dialog.setAttribute("aria-label", "Missing media report");
  const head = el("div", "prefs-head");
  head.appendChild(el("h2", "prefs-title", "Missing media"));
  const closeBtn = el("button", "icon-btn prefs-close", "✕");
  closeBtn.title = "Close";
  closeBtn.addEventListener("click", () => overlay.remove());
  head.appendChild(closeBtn);
  dialog.appendChild(head);
  const body = el("div", "rm-report-body");
  body.appendChild(el("p", "prefs-note", "Asking the library…"));
  dialog.appendChild(body);
  overlay.appendChild(dialog);
  document.body.appendChild(overlay);

  const load = async (): Promise<void> => {
    try {
      const data = await api.romMediaReport(systemKey);
      renderReport(body, data.systems, systemKey, load);
    } catch (err) {
      body.innerHTML = "";
      body.appendChild(
        el("p", "prefs-note", `The report could not be built: ${errText(err)}`),
      );
    }
  };
  void load();
}

function coverageLine(sys: RomMediaSystemReport): string {
  return sys.coverage
    .map((c) => {
      const pct = c.total > 0 ? Math.round((c.have / c.total) * 100) : 0;
      return `${TYPE_LABELS[c.mediaType] ?? c.mediaType} ${c.have}/${c.total} (${pct}%)`;
    })
    .join(" · ");
}

function renderReport(
  body: HTMLElement,
  systems: RomMediaSystemReport[],
  systemKey: string | null,
  reload: () => Promise<void>,
): void {
  body.innerHTML = "";
  if (systems.length === 0) {
    body.appendChild(
      el(
        "p",
        "prefs-note",
        "No identified games yet. Media follows identification: once ROMs are verified against the lists, their art can be fetched.",
      ),
    );
    return;
  }
  for (const sys of systems) {
    const block = el("div", "rm-report-system");
    const title = el(
      "h4",
      "prefs-subhead",
      `${sys.display} — ${sys.totalGames} game${sys.totalGames === 1 ? "" : "s"}`,
    );
    block.appendChild(title);
    block.appendChild(el("p", "rm-coverage", coverageLine(sys)));
    if (sys.missing.length === 0) {
      block.appendChild(
        el("p", "prefs-note", "Nothing missing. Every switched-on kind is on disk."),
      );
    }
    const RENDER_CAP = 200;
    const shown = sys.missing.slice(0, RENDER_CAP);
    for (const row of shown) {
      block.appendChild(reportRow(row, systemKey, reload));
    }
    const hiddenCount =
      sys.missing.length - shown.length + sys.truncatedMissing;
    if (hiddenCount > 0) {
      block.appendChild(
        el(
          "p",
          "prefs-note",
          `…and ${hiddenCount} more in this system. The Download button fetches them all the same; this list just declines to print a phone book.`,
        ),
      );
    }
    const diy = el("div", "rm-diy");
    diy.appendChild(el("p", "rm-diy-title", "Do it yourself"));
    diy.appendChild(
      el(
        "p",
        "prefs-note",
        `Drop your own files into ${sys.diyFolder} (and its screenshots / titlescreens / videos sisters), named ${sys.diyPattern} — Fable checks there first and your emulators read the same tree. Hunting a particular set? Ask your favorite Grizzly Bear AI where to find it.`,
      ),
    );
    block.appendChild(diy);
    body.appendChild(block);
  }
}

function reportRow(
  row: RomMediaSystemReport["missing"][number],
  systemKey: string | null,
  reload: () => Promise<void>,
): HTMLElement {
  const wrap = el("div", "rm-report-row");
  const main = el("div", "rm-report-main");
  main.appendChild(
    el("span", "rm-report-name", `${row.title} — ${TYPE_LABELS[row.mediaType] ?? row.mediaType}`),
  );
  main.appendChild(el("span", "rm-report-reason", row.reason));
  wrap.appendChild(main);
  if (row.candidates.length > 0) {
    const picks = el("div", "rm-candidates");
    for (const cand of row.candidates) {
      const btn = el("button", "rm-candidate");
      const img = document.createElement("img");
      img.className = "rm-candidate-img";
      // Candidate art is remote by definition — the URL as-is.
      img.src = cand.url;
      img.alt = "";
      img.loading = "lazy";
      const lab = el(
        "span",
        "rm-candidate-label",
        `${cand.title} (${cand.source === "thegamesdb" ? "TheGamesDB" : cand.source === "steamgriddb" ? "SteamGridDB" : cand.source})`,
      );
      btn.append(img, lab);
      btn.title = "This is the right game — file this art";
      btn.addEventListener("click", () => {
        void (async () => {
          btn.textContent = "Filing…";
          try {
            const msg = await api.confirmRomMediaMatch({
              itemId: row.itemId,
              mediaType: row.mediaType,
              source: cand.source,
              url: cand.url,
            });
            Toast.success(msg);
            await reload();
          } catch (err) {
            Toast.error(errText(err));
            await reload();
          }
        })();
      });
      picks.appendChild(btn);
    }
    wrap.appendChild(picks);
  } else {
    const retry = el("button", "pill-btn", "Retry");
    retry.addEventListener("click", () => {
      void (async () => {
        try {
          const start = await api.startRomMediaDownload({
            systems: systemKey ? [systemKey] : null,
            systemLabel: null,
            retry: [{ itemId: row.itemId, mediaType: row.mediaType }],
          });
          void start;
          Toast.success("Retry started — watch it in the jobs panel.");
        } catch (err) {
          Toast.info(errText(err));
        }
      })();
    });
    wrap.appendChild(retry);
  }
  return wrap;
}
