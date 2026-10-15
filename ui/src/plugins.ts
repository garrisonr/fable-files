/**
 * PluginsSection — Preferences ▸ Plugins (Phase 6, spec §§17–18).
 *
 * Plugins are small WebAssembly programs other people write to teach
 * Fable new tricks. This section is where they live: what's installed,
 * what each one declares it wants to touch, and whether it's enabled.
 * Per the guidance law the copy teaches, in plain language: plugins
 * run in a sandbox (they can only see the one item you invoke them
 * on, they can't reach the network or your other files), and a plugin
 * can never change a file by itself — it can only *propose*, and you
 * confirm. The section owns no storage: the backend rescans the
 * plugins folder on every read, and enable/disable writes through.
 */
import { api, type PluginInfo, type PluginRun } from "./api";
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

export class PluginsSection {
  private body: HTMLElement | null = null;
  private plugins: PluginInfo[] = [];

  async mount(host: HTMLElement): Promise<void> {
    host.appendChild(el("h3", "prefs-section", "Plugins"));
    host.appendChild(
      el(
        "p",
        "prefs-note",
        "Plugins are small programs that teach Fable new tricks. They run in a sandbox: a plugin can only see the one item you use it on, it can't reach the network or the rest of your files, and it can never change anything by itself — it can only suggest a change and wait for you to confirm it. To add one, drop its folder into the plugins folder and it appears here, no restart needed.",
      ),
    );
    const folderRow = el("div", "rm-cred-row");
    const folderBtn = el("button", "pill-btn", "Plugins Folder…");
    const folderPath = el("span", "rm-path-value");
    folderBtn.addEventListener("click", () => {
      void api
        .pluginsFolder()
        .then((p) => {
          folderPath.textContent = p;
          folderPath.title = p;
        })
        .catch((err) => Toast.error(`Couldn't find the plugins folder: ${errText(err)}`));
    });
    folderRow.append(folderBtn, folderPath);
    host.appendChild(folderRow);
    this.body = el("div", "p6-body");
    this.body.appendChild(el("p", "prefs-note", "Loading…"));
    host.appendChild(this.body);
    await this.refresh();
  }

  private async refresh(): Promise<void> {
    if (!this.body) return;
    try {
      this.plugins = await api.listPlugins();
    } catch (err) {
      this.body.replaceChildren(
        el("p", "prefs-note", `Couldn't read the plugins folder: ${errText(err)}`),
      );
      return;
    }
    this.render();
  }

  private render(): void {
    if (!this.body) return;
    this.body.replaceChildren();
    if (this.plugins.length === 0) {
      this.body.appendChild(
        el("p", "prefs-note", "No plugins yet. Fable ships with one example — it should appear here."),
      );
      return;
    }
    for (const p of this.plugins) this.body.appendChild(this.row(p));
  }

  private row(p: PluginInfo): HTMLElement {
    const card = el("div", "rm-source");
    const head = el("div", "p6-row-head");
    head.appendChild(el("span", "rm-source-name", p.valid ? p.name : p.key));
    if (p.valid && p.version) head.appendChild(el("span", "p6-version", `v${p.version}`));
    card.appendChild(head);
    if (p.valid) {
      if (p.author) card.appendChild(el("p", "prefs-note", `by ${p.author}`));
      if (p.description) card.appendChild(el("p", "prefs-note", p.description));
      const declares = [...p.capabilities];
      if (p.permissions.length > 0) {
        declares.push(`asks: ${p.permissions.join(", ")}`);
      }
      card.appendChild(
        el("p", "prefs-note", `Declares: ${declares.length ? declares.join(" · ") : "nothing"}`),
      );
      if (!p.wired) {
        card.appendChild(
          el(
            "p",
            "prefs-note",
            "This build runs plugins from a file's right-click menu (context actions). This plugin's declared capabilities aren't wired up yet, so it can't be run.",
          ),
        );
      }
      const toggleRow = el("label", "prefs-row prefs-toggle");
      const box = document.createElement("input");
      box.type = "checkbox";
      box.checked = p.enabled;
      box.addEventListener("change", () => {
        void api
          .setPluginEnabled(p.key, box.checked)
          .then(() => this.refresh())
          .catch((err) => {
            box.checked = !box.checked;
            Toast.error(`Couldn't update the plugin: ${errText(err)}`);
          });
      });
      toggleRow.append(box, el("span", "prefs-row-label", p.enabled ? "Enabled" : "Disabled"));
      card.appendChild(toggleRow);
    } else {
      card.appendChild(
        el(
          "p",
          "prefs-note rm-warn",
          `This plugin's manifest couldn't be read${p.error ? `: ${p.error}` : "."} It isn't loaded, and it can't affect anything.`,
        ),
      );
    }
    return card;
  }
}

/**
 * The result of running a plugin on an item: what it said (its log)
 * and what it proposes. A proposal becomes real only when the user
 * confirms it here — confirmation starts a normal journaled job.
 */
export function showPluginRunDialog(pluginName: string, run: PluginRun): void {
  const overlay = el("div", "picker-overlay");
  const card = el("div", "picker-card p6-run-card");
  card.setAttribute("role", "dialog");
  card.setAttribute("aria-label", `${pluginName} result`);
  card.appendChild(el("h3", "picker-title", pluginName));

  if (run.logs.length > 0) {
    const pre = el("pre", "p6-log");
    pre.textContent = run.logs.join("\n");
    card.appendChild(pre);
  } else if (run.proposals.length === 0) {
    card.appendChild(el("p", "prefs-note", "The plugin ran and had nothing to say."));
  }

  for (const proposal of run.proposals) {
    const block = el("div", "rm-freebox");
    block.appendChild(el("p", "rm-freebox-title", `Proposed file: ${proposal.name}`));
    block.appendChild(
      el("p", "prefs-note", `In ${proposal.targetDir} — written only if you confirm. Fable runs it as a normal job you can watch in the Jobs panel.`),
    );
    const pre = el("pre", "p6-log");
    pre.textContent =
      proposal.contents.length > 2000
        ? `${proposal.contents.slice(0, 2000)}\n…`
        : proposal.contents;
    block.appendChild(pre);
    const btn = el("button", "pill-btn primary", "Write this file");
    btn.addEventListener("click", () => {
      btn.disabled = true;
      void api
        .confirmPluginProposal(
          proposal.pluginKey,
          proposal.pluginName,
          proposal.targetDir,
          proposal.name,
          proposal.contents,
        )
        .then(() => {
          btn.textContent = "Job started ✓";
          Toast.success(`Writing ${proposal.name} — watch it in Jobs.`);
        })
        .catch((err) => {
          btn.disabled = false;
          Toast.error(`Couldn't start the job: ${errText(err)}`);
        });
    });
    block.appendChild(btn);
    card.appendChild(block);
  }

  const actions = el("div", "picker-actions");
  const done = el("button", "pill-btn primary", "Done");
  done.addEventListener("click", () => overlay.remove());
  actions.appendChild(done);
  card.appendChild(actions);
  overlay.appendChild(card);
  overlay.addEventListener("pointerdown", (e) => {
    if (e.target === overlay) overlay.remove();
  });
  document.body.appendChild(overlay);
}
