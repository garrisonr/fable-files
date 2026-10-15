/**
 * TemplatesSection — Preferences ▸ Folder Templates (Phase 6, §22).
 *
 * A folder template is a little tree of folders (plus small
 * placeholder files) that New Folder can stamp out in one go — as a
 * normal journaled job, so it lands in the Jobs panel like any other
 * work. This section manages the set: what exists, making new ones
 * (blank, or captured from a folder you already like), renaming, and
 * deleting. Capturing copies files up to 1 MB; anything bigger is
 * left out and reported, never silently dropped.
 */
import { api, type FolderTemplateInfo } from "./api";
import { Toast, confirmDialog, errText, promptDialog } from "./toast";

export interface TemplatesHooks {
  pickFolder(initial: string | null): Promise<string | null>;
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

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

export class TemplatesSection {
  private body: HTMLElement | null = null;
  private hooks: TemplatesHooks | null = null;
  private templates: FolderTemplateInfo[] = [];

  async mount(host: HTMLElement, hooks: TemplatesHooks): Promise<void> {
    this.hooks = hooks;
    host.appendChild(el("h3", "prefs-section", "Folder Templates"));
    host.appendChild(
      el(
        "p",
        "prefs-note",
        "Templates stamp out a whole folder tree at once — a project skeleton, a photo shoot's RAW / Selects / Exports — from the background right-click menu (New Folder from Template). Applying one runs as a normal job. Capture a folder you already like and it becomes a template; small placeholder files come along, files over 1 MB are left out and reported.",
      ),
    );
    const actions = el("div", "rm-actions");
    const blank = el("button", "pill-btn", "New Blank Template…");
    blank.addEventListener("click", () => void this.createFlow(null));
    const fromFolder = el("button", "pill-btn", "New Template from Folder…");
    fromFolder.addEventListener("click", () => void this.fromFolderFlow());
    actions.append(blank, fromFolder);
    host.appendChild(actions);
    this.body = el("div", "p6-body");
    this.body.appendChild(el("p", "prefs-note", "Loading…"));
    host.appendChild(this.body);
    await this.refresh();
  }

  private async refresh(): Promise<void> {
    if (!this.body) return;
    try {
      this.templates = await api.listFolderTemplates();
    } catch (err) {
      this.body.replaceChildren(
        el("p", "prefs-note", `Couldn't read the templates: ${errText(err)}`),
      );
      return;
    }
    this.render();
  }

  private render(): void {
    if (!this.body) return;
    this.body.replaceChildren();
    if (this.templates.length === 0) {
      this.body.appendChild(el("p", "prefs-note", "No templates yet — make one above."));
      return;
    }
    for (const t of this.templates) this.body.appendChild(this.row(t));
  }

  private row(t: FolderTemplateInfo): HTMLElement {
    const card = el("div", "rm-source");
    const head = el("div", "p6-row-head");
    head.appendChild(el("span", "rm-source-name", t.name));
    if (t.starter) head.appendChild(el("span", "p6-version", "Starter"));
    card.appendChild(head);
    card.appendChild(
      el(
        "p",
        "prefs-note",
        `${t.folders} folder${t.folders === 1 ? "" : "s"} · ${t.files} placeholder file${t.files === 1 ? "" : "s"} · ${fmtBytes(t.bytes)}`,
      ),
    );
    const actions = el("div", "rm-cred-row");
    const rename = el("button", "pill-btn", "Rename…");
    rename.addEventListener("click", () => void this.renameFlow(t));
    const del = el("button", "pill-btn", "Delete");
    del.addEventListener("click", () => void this.deleteFlow(t));
    actions.append(rename, del);
    card.appendChild(actions);
    return card;
  }

  private async fromFolderFlow(): Promise<void> {
    if (!this.hooks) return;
    const folder = await this.hooks.pickFolder(null);
    if (!folder) return;
    const base = folder.split("/").filter(Boolean).pop() ?? "";
    await this.createFlow(folder, base);
  }

  private async createFlow(fromPath: string | null, initial = ""): Promise<void> {
    const name = await promptDialog(
      fromPath ? "New Template from Folder" : "New Blank Template",
      "e.g. Podcast Episode",
      initial,
    );
    if (!name) return;
    try {
      const res = await api.createFolderTemplate(name, fromPath);
      if (res.skippedLargeFiles > 0) {
        Toast.show(
          `Template “${name}” created — ${res.skippedLargeFiles} file${res.skippedLargeFiles === 1 ? "" : "s"} over 1 MB left out.`,
        );
      } else {
        Toast.success(`Template “${name}” created.`);
      }
      await this.refresh();
    } catch (err) {
      Toast.error(`Couldn't create the template: ${errText(err)}`);
    }
  }

  private async renameFlow(t: FolderTemplateInfo): Promise<void> {
    const name = await promptDialog("Rename Template", "", t.name);
    if (!name || name === t.name) return;
    try {
      await api.renameFolderTemplate(t.id, name);
      Toast.success(`Renamed to “${name}”.`);
      await this.refresh();
    } catch (err) {
      Toast.error(`Rename failed: ${errText(err)}`);
    }
  }

  private async deleteFlow(t: FolderTemplateInfo): Promise<void> {
    const ok = await confirmDialog(
      `Delete template “${t.name}”?`,
      "The template goes away. Folders you already made from it are not touched.",
      "Delete Template",
      true,
    );
    if (!ok) return;
    try {
      await api.deleteFolderTemplate(t.id);
      Toast.success(`Template “${t.name}” deleted.`);
      await this.refresh();
    } catch (err) {
      Toast.error(`Couldn't delete the template: ${errText(err)}`);
    }
  }
}
