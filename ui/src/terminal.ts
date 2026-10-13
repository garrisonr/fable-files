/**
 * TerminalPanel — the built-in terminal: a PTY-backed xterm living in
 * a bottom panel. Opens in the folder you're browsing (Ctrl+`).
 */
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "@xterm/xterm/css/xterm.css";

function b64decode(data: string): Uint8Array {
  const bin = atob(data);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return bytes;
}

export class TerminalPanel {
  readonly el: HTMLDivElement;
  private term: Terminal;
  private fit = new FitAddon();
  private sessionId: string | null = null;
  private cwdLabel: HTMLSpanElement;
  private currentCwd = "";
  visible = false;

  constructor() {
    this.el = document.createElement("div");
    this.el.className = "term-panel";
    this.el.hidden = true;
    const head = document.createElement("div");
    head.className = "term-head";
    const title = document.createElement("span");
    title.className = "term-title";
    title.textContent = "⌨ Terminal";
    this.cwdLabel = document.createElement("span");
    this.cwdLabel.className = "term-cwd";
    const close = document.createElement("button");
    close.className = "icon-btn";
    close.textContent = "✕";
    close.title = "Close terminal (the shell keeps running)";
    close.addEventListener("click", () => this.hide());
    head.append(title, this.cwdLabel, close);
    const body = document.createElement("div");
    body.className = "term-body";
    this.el.append(head, body);

    this.term = new Terminal({
      fontSize: 13,
      fontFamily: "ui-monospace, 'JetBrains Mono', Menlo, monospace",
      theme: {
        background: "#101014",
        foreground: "#e8e8ec",
        cursor: "#f5c518",
        selectionBackground: "#3a3f4b",
      },
      cursorBlink: true,
      convertEol: false,
    });
    this.term.loadAddon(this.fit);
    this.term.open(body);
    this.term.onData((data) => {
      if (this.sessionId) {
        void invoke("term_write", { id: this.sessionId, data }).catch(
          () => undefined,
        );
      }
    });
    void listen<{ id: string; data: string }>("term-output", (e) => {
      if (e.payload.id === this.sessionId) {
        this.term.write(b64decode(e.payload.data));
      }
    }).catch(() => undefined);
    void listen<{ id: string }>("term-exit", (e) => {
      if (e.payload.id === this.sessionId) {
        this.sessionId = null;
        this.term.write("\r\n\x1b[33m[shell exited — reopen the panel for a new one]\x1b[0m\r\n");
      }
    }).catch(() => undefined);
    new ResizeObserver(() => {
      if (this.visible) this.fitAndResize();
    }).observe(body);
  }

  async toggle(cwd: string): Promise<void> {
    if (this.visible) {
      this.hide();
      return;
    }
    this.el.hidden = false;
    this.visible = true;
    await this.ensureSession(cwd);
    this.fitAndResize();
    this.term.focus();
  }

  /** Show the panel (opening a session if needed) and type a
   * command into the shell — the OAuth authorization flow uses it. */
  async runInFolder(command: string, cwd: string): Promise<void> {
    if (!this.visible) {
      this.el.hidden = false;
      this.visible = true;
    }
    await this.ensureSession(cwd);
    this.fitAndResize();
    if (this.sessionId) {
      await invoke("term_write", {
        id: this.sessionId,
        data: command + "\n",
      }).catch(() => undefined);
    }
    this.term.focus();
  }

  hide(): void {
    this.el.hidden = true;
    this.visible = false;
  }

  /** cd the live shell to a new folder (when navigating with the
   * panel open and "follow" is desired). */
  async cdTo(path: string): Promise<void> {
    if (this.sessionId && path && path !== this.currentCwd) {
      this.currentCwd = path;
      this.cwdLabel.textContent = path;
      await invoke("term_write", {
        id: this.sessionId,
        data: `cd -- '${path.replace(/'/g, "'\\''")}'\n`,
      }).catch(() => undefined);
    }
  }

  private async ensureSession(cwd: string): Promise<void> {
    if (this.sessionId) {
      await this.cdTo(cwd);
      return;
    }
    this.currentCwd = cwd;
    this.cwdLabel.textContent = cwd;
    this.fit.fit();
    const dims = this.fit.proposeDimensions();
    this.sessionId = await invoke<string>("term_open", {
      cwd,
      cols: dims?.cols ?? 80,
      rows: dims?.rows ?? 24,
    });
  }

  private fitAndResize(): void {
    try {
      this.fit.fit();
      const dims = this.fit.proposeDimensions();
      if (this.sessionId && dims) {
        void invoke("term_resize", {
          id: this.sessionId,
          cols: dims.cols,
          rows: dims.rows,
        }).catch(() => undefined);
      }
    } catch {
      // panel mid-layout; the next observer tick will fit again
    }
  }
}
