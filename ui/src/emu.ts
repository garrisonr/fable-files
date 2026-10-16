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

    // ---- the options
    const list = el("div", "emu-opt-list");
    const radios: HTMLInputElement[] = [];
    const noteByOption = new Map<string, HTMLElement>();
    let selected = sheet.preselect;
    for (const opt of sheet.options) {
      const row = el("label", "emu-opt");
      const radio = document.createElement("input");
      radio.type = "radio";
      radio.name = "emu-opt";
      radio.value = opt.id;
      radio.checked = opt.id === sheet.preselect;
      radios.push(radio);
      const main = el("div", "emu-opt-main");
      main.appendChild(el("strong", "emu-opt-label", opt.label));
      main.appendChild(el("span", "emu-opt-verdict", opt.verdict));
      if (opt.installNote) {
        const note = el("span", "emu-opt-note", opt.installNote);
        note.hidden = opt.id !== sheet.preselect;
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
    dialog.appendChild(list);

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
      sheet.options.find((o) => o.id === selected) ?? sheet.options[0];

    function refresh(): void {
      const opt = selectedOpt();
      primary.textContent = primaryLabel(opt);
      primary.disabled = !primaryEnabled(opt);
      for (const [id, note] of noteByOption) note.hidden = id !== selected;
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
          } else {
            // Calm receipt: what happened, nothing is broken, and
            // the doors stay open (retry, or the OS default).
            busy = false;
            progress.hidden = true;
            receipt.hidden = false;
            receipt.textContent =
              outcome.kind === "unavailable"
                ? outcome.message
                : "That still needs setup before it can play.";
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
