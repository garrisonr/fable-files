/**
 * Emulation UI (SPEC-AMENDMENT "Emulation", Architecture A):
 * the ask-once sheet ("How should Fable play <game>?"), the
 * "Play with…" picker (the same sheet in picker mode), and the
 * Preferences → ROMs cards (Emulators + Unified view).
 *
 * The backend owns every fact on these surfaces — option lists,
 * verdict lines, state words, the install decision. This file
 * renders them verbatim and reports the man's choice back. Copy
 * follows the amendment §6: warm, plain, no jargon, no dead ends.
 * Declining is always free of consequence: Play falls back to the
 * system default app, exactly as it behaved before emulation.
 */
import {
  api,
  onEmuInstallProgress,
  type EmuPlayOutcome,
  type EmuPrefs,
  type EmuSheet,
  type EmuSheetOption,
  type EmuViewState,
} from "./api";
import { Toast, errText } from "./toast";
import type { UnlistenFn } from "@tauri-apps/api/event";

export type EmuSheetResult =
  | {
      action: "launched";
      outcome: Extract<EmuPlayOutcome, { kind: "launched" }>;
    }
  | { action: "os-default" }
  | { action: "dismissed" };

export interface EmuSheetHooks {
  /** Footer door: open the emulator itself (controller setup
   * lives there — Fable builds no mapper in v1). */
  onOpenEmulator(platform: string): void;
}

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

function primaryLabel(opt: EmuSheetOption): string {
  if (opt.needsInstall) return "Install & Play";
  if (opt.state === "ready") return "Play";
  return opt.stateWord;
}

function primaryEnabled(opt: EmuSheetOption): boolean {
  return opt.needsInstall || opt.state === "ready";
}

/**
 * Render the sheet and resolve with what the man chose. The sheet
 * never traps: backdrop/Esc dismisses, "Not now" (ask mode) and
 * the system-default entry (picker mode) hand Play back to the OS.
 */
export function showEmuSheet(
  sheet: EmuSheet,
  hooks: EmuSheetHooks,
): Promise<EmuSheetResult> {
  return new Promise((resolve) => {
    let settled = false;
    let unlisten: UnlistenFn | null = null;
    const finish = (r: EmuSheetResult) => {
      if (settled) return;
      settled = true;
      if (unlisten) void unlisten();
      overlay.remove();
      document.removeEventListener("keydown", onKey, true);
      resolve(r);
    };

    const overlay = el("div", "emu-overlay");
    const dialog = el("div", "emu-sheet");
    dialog.setAttribute("role", "dialog");
    dialog.setAttribute("aria-label", `Play ${sheet.gameTitle}`);
    overlay.appendChild(dialog);

    const picker = sheet.mode === "picker";
    dialog.appendChild(
      el(
        "h2",
        "emu-sheet-title",
        picker ? `Play ${sheet.gameTitle} with…` : `Play ${sheet.gameTitle}`,
      ),
    );
    dialog.appendChild(
      el(
        "p",
        "emu-sheet-sub",
        picker
          ? `Pick the emulator for this ${sheet.shortName} game.`
          : `Fable can play ${sheet.shortName} games itself. Pick once — Fable remembers, and you can change your mind any time in Preferences → ROMs.`,
      ),
    );

    // ---- the options (re-renderable: the BIOS Add flow and the
    // post-install gate swap in fresh sheet data in place)
    let activeSheet = sheet;
    const list = el("div", "emu-opt-list");
    const radios: HTMLInputElement[] = [];
    const noteByOption = new Map<string, HTMLElement>();
    let selected = sheet.preselect;
    function renderOptions(): void {
      list.innerHTML = "";
      radios.length = 0;
      noteByOption.clear();
      for (const opt of activeSheet.options) {
        const row = el("label", "emu-opt");
        const radio = document.createElement("input");
        radio.type = "radio";
        radio.name = "emu-opt";
        radio.value = opt.id;
        radio.checked = opt.id === selected;
        radios.push(radio);
        const main = el("div", "emu-opt-main");
        main.appendChild(el("strong", "emu-opt-label", opt.label));
        main.appendChild(el("span", "emu-opt-verdict", opt.verdict));
        if (opt.installNote) {
          const note = el("span", "emu-opt-note", opt.installNote);
          note.hidden = opt.id !== selected;
          noteByOption.set(opt.id, note);
          main.appendChild(note);
        }
        const state = el(
          "span",
          `emu-opt-state${opt.state === "ready" ? " is-ready" : ""}`,
          opt.stateWord,
        );
        row.append(radio, main, state);
        radio.addEventListener("change", () => {
          if (radio.checked) {
            selected = opt.id;
            refresh();
          }
        });
        list.appendChild(row);
      }
    }
    renderOptions();
    dialog.appendChild(list);

    // ---- the BIOS row (§6): when the selected option's system
    // needs a system file that isn't there, the sheet says so
    // BEFORE any launch — with the Add flow right here.
    const biosRow = el("div", "emu-bios-row");
    biosRow.hidden = true;
    const biosText = el("p", "emu-bios-text", "");
    const biosAdd = el(
      "button",
      "emu-btn emu-bios-add",
      "Add the file…",
    ) as HTMLButtonElement;
    biosRow.append(biosText, biosAdd);
    dialog.appendChild(biosRow);
    function renderBiosRow(): void {
      const bios = selectedOpt()?.bios ?? null;
      if (!bios) {
        biosRow.hidden = true;
        return;
      }
      biosRow.hidden = false;
      biosRow.classList.toggle("is-blocking", bios.blocking);
      biosText.textContent = bios.blocking
        ? `Before ${activeSheet.systemName} games can play, Fable needs ${bios.files.join(", ")}. ${bios.note} Fable can't download these — they come from a console you own.`
        : `Nice to have: ${bios.files.join(", ")}. ${bios.note}`;
      biosAdd.textContent =
        bios.files.length > 1 ? "Add the files…" : "Add the file…";
    }
    biosAdd.addEventListener("click", () => {
      if (busy) return;
      busy = true;
      biosAdd.disabled = true;
      biosAdd.textContent = "Choose the file…";
      void (async () => {
        try {
          const result = await api.emuBiosAdd(activeSheet.systemId);
          if (!result.cancelled) {
            receipt.hidden = false;
            if (result.filed.length > 0) {
              const filedNames = result.filed
                .map((f) => f.filedAs)
                .join(", ");
              receipt.textContent = result.system.satisfied
                ? `Filed ${filedNames} — all set.`
                : `Filed ${filedNames}. Still missing: ${result.system.sets
                    .flatMap((s) => s.missing)
                    .join(", ")}.`;
            } else if (result.rejected.length > 0) {
              receipt.textContent = result.rejected[0].reason;
            }
            await reloadSheet();
          }
        } catch (err) {
          Toast.error(`That didn't work: ${errText(err)}`);
        } finally {
          busy = false;
          biosAdd.disabled = false;
          refresh();
        }
      })();
    });

    /** Re-read the sheet from the backend (states changed under
     * us — a file was filed, a core landed), keeping the man's
     * selection and the sheet's mode. */
    async function reloadSheet(): Promise<void> {
      try {
        const fresh = await api.emuSheet(sheet.providerKey, sheet.platform);
        fresh.mode = activeSheet.mode;
        activeSheet = fresh;
        if (!activeSheet.options.some((o) => o.id === selected)) {
          selected = activeSheet.preselect;
        }
        renderOptions();
      } catch {
        // Keep the current sheet; its states may lag one beat.
      }
      refresh();
    }

    // ---- picker-only doors: the OS escape + the remember choice
    let rememberBox: HTMLInputElement | null = null;
    if (picker) {
      const sep = el("div", "emu-sep");
      dialog.appendChild(sep);
      const osRow = el(
        "button",
        "emu-os-row",
        "Open with the system default app…",
      ) as HTMLButtonElement;
      osRow.addEventListener("click", () => finish({ action: "os-default" }));
      dialog.appendChild(osRow);
      const remRow = el("label", "emu-remember");
      rememberBox = document.createElement("input");
      rememberBox.type = "checkbox";
      remRow.append(
        rememberBox,
        el(
          "span",
          "",
          `Always use this for ${sheet.shortName} games`,
        ),
      );
      dialog.appendChild(remRow);
    }

    // ---- progress + receipt (install time)
    const progress = el("div", "emu-progress");
    progress.hidden = true;
    const progressLabel = el("span", "emu-progress-label", "");
    const bar = el("div", "emu-progress-bar");
    const barFill = el("div", "emu-progress-fill");
    bar.appendChild(barFill);
    progress.append(progressLabel, bar);
    dialog.appendChild(progress);
    const receipt = el("p", "emu-receipt");
    receipt.hidden = true;
    dialog.appendChild(receipt);

    // ---- actions
    const actions = el("div", "emu-actions");
    const controller = el(
      "button",
      "emu-btn emu-btn-quiet",
      "Set up my controller",
    ) as HTMLButtonElement;
    controller.addEventListener("click", () =>
      hooks.onOpenEmulator(sheet.platform),
    );
    const spacer = el("span", "emu-actions-spacer");
    const notNow = el(
      "button",
      "emu-btn",
      picker ? "Cancel" : "Not now",
    ) as HTMLButtonElement;
    notNow.addEventListener("click", () =>
      finish(picker ? { action: "dismissed" } : { action: "os-default" }),
    );
    const primary = el("button", "emu-btn primary", "Play") as HTMLButtonElement;
    actions.append(controller, spacer, notNow, primary);
    dialog.appendChild(actions);

    const selectedOpt = (): EmuSheetOption =>
      activeSheet.options.find((o) => o.id === selected) ??
      activeSheet.options[0];

    function refresh(): void {
      const opt = selectedOpt();
      primary.textContent = primaryLabel(opt);
      primary.disabled = !primaryEnabled(opt);
      for (const [id, note] of noteByOption) note.hidden = id !== selected;
      renderBiosRow();
    }

    let busy = false;
    primary.addEventListener("click", () => {
      if (busy) return;
      const opt = selectedOpt();
      if (!primaryEnabled(opt)) return;
      busy = true;
      for (const r of radios) r.disabled = true;
      primary.disabled = true;
      notNow.disabled = true;
      receipt.hidden = true;
      progress.hidden = false;
      progressLabel.textContent = opt.needsInstall
        ? "Getting things ready…"
        : "Starting…";
      barFill.style.width = "0%";
      const remember = picker ? (rememberBox?.checked ?? false) : true;
      void (async () => {
        try {
          unlisten = await onEmuInstallProgress((p) => {
            progressLabel.textContent = p.label;
            barFill.style.width =
              p.pct === null ? "100%" : `${Math.max(2, p.pct)}%`;
            bar.classList.toggle("is-indeterminate", p.pct === null);
          });
        } catch {
          // progress is garnish; the install proceeds regardless
        }
        try {
          const outcome = await api.emuInstallAndPlay(
            sheet.providerKey,
            sheet.platform,
            opt.id,
            remember,
          );
          if (outcome.kind === "launched") {
            finish({ action: "launched", outcome });
          } else if (outcome.kind === "sheet") {
            // Installed fine — but a system file is still missing.
            // The backend handed back a fresh sheet whose BIOS row
            // carries the Add flow; swap it in and say so plainly.
            busy = false;
            progress.hidden = true;
            activeSheet = outcome.sheet;
            if (activeSheet.options.some((o) => o.id === outcome.sheet.preselect)) {
              selected = outcome.sheet.preselect;
            }
            renderOptions();
            for (const r of radios) r.disabled = false;
            receipt.hidden = false;
            receipt.textContent =
              "Almost there — this system needs one of its own files first. It's listed above, and you can add it right here.";
            refresh();
            notNow.disabled = false;
            notNow.textContent = picker ? "Cancel" : "Not now";
            biosRow.scrollIntoView({ block: "nearest" });
          } else {
            // Calm receipt: what happened, nothing is broken, and
            // the doors stay open (retry, or the OS default).
            busy = false;
            progress.hidden = true;
            receipt.hidden = false;
            receipt.textContent = outcome.message;
            for (const r of radios) r.disabled = false;
            primary.disabled = !primaryEnabled(selectedOpt());
            notNow.disabled = false;
            notNow.textContent = picker ? "Cancel" : "Not now";
          }
        } catch (err) {
          busy = false;
          progress.hidden = true;
          for (const r of radios) r.disabled = false;
          primary.disabled = !primaryEnabled(selectedOpt());
          notNow.disabled = false;
          Toast.error(`That didn't work: ${errText(err)}`);
        }
      })();
    });

    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        finish({ action: "dismissed" });
      }
    };
    document.addEventListener("keydown", onKey, true);
    overlay.addEventListener("mousedown", (e) => {
      if (e.target === overlay) finish({ action: "dismissed" });
    });

    refresh();
    document.body.appendChild(overlay);
    primary.focus();
  });
}

// ---------------------------------------------------------------------------
// Preferences → ROMs: the Emulators + Unified view cards.
// ---------------------------------------------------------------------------

export interface EmuSectionHooks {
  pickFolder(initial: string | null): Promise<string | null>;
}

function homeShort(path: string): string {
  return path.replace(/^\/home\/[^/]+/, "~");
}

export class EmuSection {
  private body: HTMLElement | null = null;
  private hooks: EmuSectionHooks | null = null;
  private prefs: EmuPrefs | null = null;

  async mount(host: HTMLElement, hooks: EmuSectionHooks): Promise<void> {
    this.hooks = hooks;
    host.appendChild(el("h3", "prefs-section", "Emulators"));
    host.appendChild(
      el(
        "p",
        "prefs-note",
        "Fable plays your games itself — it picks the emulator, installs what's missing the first time you press Play, and remembers your choice per system. Your games never move; Play always opens the real file, wherever it lives.",
      ),
    );
    this.body = el("div", "emu-prefs-body");
    this.body.appendChild(el("p", "prefs-note", "Loading…"));
    host.appendChild(this.body);
    try {
      this.prefs = await api.emuPrefs();
      this.render();
    } catch (err) {
      this.body.innerHTML = "";
      this.body.appendChild(
        el(
          "p",
          "prefs-note",
          `Emulator settings could not load: ${errText(err)}`,
        ),
      );
    }
  }

  private render(): void {
    if (!this.body || !this.prefs) return;
    this.body.innerHTML = "";
    // ---- Emulators rows
    if (this.prefs.systems.length === 0) {
      this.body.appendChild(
        el(
          "p",
          "prefs-note",
          "No game systems in the library yet — when ROMs are identified, their emulators appear here.",
        ),
      );
    }
    for (const sys of this.prefs.systems) {
      this.body.appendChild(this.systemRow(sys));
    }
    // ---- System files (BIOS) card (§6)
    const bh = el("h3", "prefs-section", "System files");
    bh.style.marginTop = "1.4rem";
    this.body.appendChild(bh);
    this.body.appendChild(
      el(
        "p",
        "prefs-note",
        "Some systems need a small file from the console itself before their games can play. These files come from consoles you own — Fable never downloads them. If you have one, add it here and Fable files it where the emulator looks.",
      ),
    );
    for (const bios of this.prefs.bios) {
      this.body.appendChild(this.biosRow(bios));
    }
    // ---- Unified view card
    const h = el("h3", "prefs-section", "Unified view");
    h.style.marginTop = "1.4rem";
    this.body.appendChild(h);
    this.body.appendChild(
      el(
        "p",
        "prefs-note",
        "One combined folder where every game shows up, whichever drive it's really on — kept in step as drives come and go, and mirrored into RetroArch playlists. The folder holds links, never copies; deleting a link never deletes a game.",
      ),
    );
    const viewHost = el("div", "emu-view-host");
    viewHost.appendChild(this.viewCard(this.prefs.view));
    this.body.appendChild(viewHost);
  }

  private systemRow(sys: EmuPrefs["systems"][number]): HTMLElement {
    const row = el("div", "emu-sys-row");
    const head = el("div", "emu-sys-head");
    head.appendChild(el("strong", "", sys.name));
    head.appendChild(
      el("span", "emu-sys-count", `${sys.games} game${sys.games === 1 ? "" : "s"}`),
    );
    row.appendChild(head);
    const pickRow = el("div", "emu-sys-pickrow");
    pickRow.appendChild(el("span", "", "Play with"));
    const select = document.createElement("select");
    select.className = "emu-sys-select";
    for (const opt of sys.options) {
      const o = document.createElement("option");
      o.value = opt.id;
      o.textContent = `${opt.label} — ${opt.stateWord}`;
      select.appendChild(o);
    }
    select.value = sys.chosen;
    pickRow.appendChild(select);
    row.appendChild(pickRow);
    const verdict = el(
      "p",
      "emu-sys-verdict",
      sys.options.find((o) => o.id === sys.chosen)?.verdict ?? "",
    );
    row.appendChild(verdict);
    select.addEventListener("change", () => {
      const opt = sys.options.find((o) => o.id === select.value);
      verdict.textContent = opt?.verdict ?? "";
      void api.emuSetChoice(sys.platform, select.value).catch((err) => {
        Toast.error(`Could not save that choice: ${errText(err)}`);
      });
      if (opt && opt.state !== "ready") {
        Toast.info(
          "Saved. Fable will set that up the first time you press Play.",
        );
      }
    });
    return row;
  }

  private biosRow(bios: EmuPrefs["bios"][number]): HTMLElement {
    const row = el("div", "emu-sys-row");
    const head = el("div", "emu-sys-head");
    head.appendChild(el("strong", "", bios.title));
    const stateWord = bios.satisfied
      ? "Ready"
      : bios.need === "required"
        ? "Missing"
        : "Not added";
    head.appendChild(
      el(
        "span",
        `emu-sys-count${bios.satisfied ? " is-ready" : ""}`,
        stateWord,
      ),
    );
    row.appendChild(head);
    for (const set of bios.sets) {
      const names = set.names.join(set.all ? ", " : " or ");
      const line = set.present
        ? `${set.what}: ${names} ✓`
        : `${set.what}: ${names}`;
      row.appendChild(el("p", "emu-sys-verdict", line));
    }
    row.appendChild(el("p", "emu-sys-verdict", bios.note));
    const actions = el("div", "emu-bios-actions");
    const add = el(
      "button",
      "emu-btn",
      bios.satisfied ? "Add files…" : "Add the file…",
    ) as HTMLButtonElement;
    add.addEventListener("click", () => {
      add.disabled = true;
      add.textContent = "Choose the file…";
      void (async () => {
        try {
          const result = await api.emuBiosAdd(bios.systemId);
          if (!result.cancelled && this.prefs) {
            const idx = this.prefs.bios.findIndex(
              (b) => b.systemId === bios.systemId,
            );
            if (idx >= 0) this.prefs.bios[idx] = result.system;
            if (result.filed.length > 0) {
              Toast.success(
                result.system.satisfied
                  ? `${bios.title}: filed ${result.filed
                      .map((f) => f.filedAs)
                      .join(", ")} — all set.`
                  : `${bios.title}: filed ${result.filed
                      .map((f) => f.filedAs)
                      .join(", ")}. Still missing: ${result.system.sets
                      .flatMap((s) => s.missing)
                      .join(", ")}.`,
              );
            } else if (result.rejected.length > 0) {
              Toast.error(result.rejected[0].reason);
            }
            this.render();
          }
        } catch (err) {
          Toast.error(`That didn't work: ${errText(err)}`);
        } finally {
          add.disabled = false;
          add.textContent = bios.satisfied ? "Add files…" : "Add the file…";
        }
      })();
    });
    actions.appendChild(add);
    actions.appendChild(
      el("span", "emu-bios-home", `Lives in ${homeShort(bios.home)}`),
    );
    row.appendChild(actions);
    return row;
  }

  private viewCard(view: EmuViewState): HTMLElement {
    const card = el("div", "emu-view-card");
    const toggleRow = el("label", "emu-view-toggle");
    const box = document.createElement("input");
    box.type = "checkbox";
    box.checked = view.enabled;
    toggleRow.append(
      box,
      el("span", "", "Keep the combined folder up to date"),
    );
    card.appendChild(toggleRow);

    const stateLine = el("p", "emu-view-state", "");
    const setState = (v: EmuViewState) => {
      const bits = [
        `${v.games} game${v.games === 1 ? "" : "s"}`,
        `${v.drives} drive${v.drives === 1 ? "" : "s"}`,
        `${v.linksOnDisk} link${v.linksOnDisk === 1 ? "" : "s"} in place`,
      ];
      if (v.retroarchPresent) bits.push("RetroArch playlists in step");
      stateLine.textContent = v.enabled
        ? bits.join(" · ")
        : "Off — the folder stays as it is, and Fable loses nothing.";
      if (v.lastReport.error) {
        stateLine.textContent += ` (${v.lastReport.error})`;
      }
    };
    setState(view);
    card.appendChild(stateLine);
    card.appendChild(
      el("p", "emu-view-root", homeShort(view.root)),
    );

    const actions = el("div", "emu-view-actions");
    const rebuild = el(
      "button",
      "emu-btn",
      "Rebuild now",
    ) as HTMLButtonElement;
    rebuild.disabled = !view.enabled;
    rebuild.addEventListener("click", () => {
      rebuild.disabled = true;
      void api
        .emuRebuildView()
        .then((v) => {
          setState(v);
          rebuild.disabled = !v.enabled;
          Toast.success("The combined folder is up to date.");
        })
        .catch((err) => {
          rebuild.disabled = false;
          Toast.error(`Rebuild didn't finish: ${errText(err)}`);
        });
    });
    const change = el("button", "emu-btn", "Move…") as HTMLButtonElement;
    change.addEventListener("click", () => {
      void (async () => {
        const picked = await this.hooks?.pickFolder(view.root);
        if (!picked || picked === view.root) return;
        try {
          const v = await api.emuSetView(view.enabled, picked);
          Toast.success(`The combined folder now lives at ${homeShort(v.root)}.`);
          if (this.prefs) {
            this.prefs.view = v;
            this.render();
          }
        } catch (err) {
          Toast.error(`Could not move the folder: ${errText(err)}`);
        }
      })();
    });
    actions.append(rebuild, change);
    card.appendChild(actions);

    box.addEventListener("change", () => {
      void api
        .emuSetView(box.checked, null)
        .then((v) => {
          setState(v);
          rebuild.disabled = !v.enabled;
          if (this.prefs) this.prefs.view = v;
        })
        .catch((err) => {
          box.checked = !box.checked;
          Toast.error(`Could not save that: ${errText(err)}`);
        });
    });
    return card;
  }
}
