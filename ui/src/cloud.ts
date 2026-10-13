/**
 * CloudMount — rclone-backed cloud drives in the sidebar.
 * Mounts are persistent by construction: the backend installs a
 * systemd user unit per mount, so drives return at every login.
 * OAuth backends authorize through the built-in terminal
 * (`rclone authorize <type>`) and paste the token back here.
 */
import { api } from "./api";
import { Toast } from "./toast";

interface FieldDef {
  key: string;
  label: string;
  secret?: boolean;
  placeholder?: string;
}

interface BackendDef {
  type: string;
  label: string;
  group: string;
  oauth?: boolean;
  fields: FieldDef[];
}

const USER_PASS: FieldDef[] = [
  { key: "user", label: "Username / email" },
  { key: "pass", label: "Password", secret: true },
];
const URL_USER_PASS: FieldDef[] = [
  { key: "url", label: "Server URL", placeholder: "https://cloud.example.com" },
  ...USER_PASS,
];

export const RCLONE_BACKENDS: BackendDef[] = [
  // ——— Popular ———
  { type: "drive", label: "Google Drive", group: "Popular", oauth: true, fields: [] },
  { type: "dropbox", label: "Dropbox", group: "Popular", oauth: true, fields: [] },
  { type: "onedrive", label: "Microsoft OneDrive", group: "Popular", oauth: true, fields: [] },
  { type: "pcloud", label: "pCloud", group: "Popular", oauth: true, fields: [] },
  { type: "box", label: "Box", group: "Popular", oauth: true, fields: [] },
  {
    type: "mega", label: "MEGA", group: "Popular",
    fields: USER_PASS,
  },
  {
    type: "s3", label: "Amazon S3", group: "Popular",
    fields: [
      { key: "provider", label: "Provider", placeholder: "AWS" },
      { key: "access_key_id", label: "Access key ID" },
      { key: "secret_access_key", label: "Secret access key", secret: true },
      { key: "region", label: "Region", placeholder: "us-east-1" },
      { key: "endpoint", label: "Endpoint (optional)" },
    ],
  },
  {
    type: "b2", label: "Backblaze B2", group: "Popular",
    fields: [
      { key: "account", label: "Account ID" },
      { key: "key", label: "Application key", secret: true },
    ],
  },
  // ——— Self-hosted & FOSS ———
  { type: "webdav", label: "Nextcloud (WebDAV)", group: "Self-hosted & FOSS", fields: URL_USER_PASS },
  {
    type: "owncloud", label: "ownCloud", group: "Self-hosted & FOSS",
    fields: URL_USER_PASS,
  },
  {
    type: "seafile", label: "Seafile", group: "Self-hosted & FOSS",
    fields: [
      { key: "url", label: "Server URL" },
      ...USER_PASS,
      { key: "library", label: "Library name" },
    ],
  },
  {
    type: "s3", label: "MinIO / self-hosted S3", group: "Self-hosted & FOSS",
    fields: [
      { key: "provider", label: "Provider", placeholder: "Minio" },
      { key: "access_key_id", label: "Access key ID" },
      { key: "secret_access_key", label: "Secret access key", secret: true },
      { key: "endpoint", label: "Endpoint URL", placeholder: "https://minio.example.com" },
    ],
  },
  {
    type: "sftp", label: "SFTP / SSH", group: "Self-hosted & FOSS",
    fields: [
      { key: "host", label: "Host" },
      { key: "user", label: "Username" },
      { key: "pass", label: "Password (or use key file)", secret: true },
      { key: "key_file", label: "Key file path (optional)" },
      { key: "port", label: "Port (optional)", placeholder: "22" },
    ],
  },
  {
    type: "ftp", label: "FTP", group: "Self-hosted & FOSS",
    fields: [{ key: "host", label: "Host" }, ...USER_PASS],
  },
  {
    type: "smb", label: "SMB / Windows share", group: "Self-hosted & FOSS",
    fields: [
      { key: "host", label: "Host" },
      ...USER_PASS,
      { key: "domain", label: "Domain (optional)" },
    ],
  },
  {
    type: "storj", label: "Storj (decentralized)", group: "Self-hosted & FOSS",
    fields: [
      { key: "access_grant", label: "Access grant", secret: true },
    ],
  },
  {
    type: "koofr", label: "Koofr", group: "Self-hosted & FOSS",
    fields: USER_PASS,
  },
  {
    type: "internxt", label: "Internxt", group: "Self-hosted & FOSS",
    fields: USER_PASS,
  },
  { type: "webdav", label: "WebDAV (generic)", group: "Self-hosted & FOSS", fields: URL_USER_PASS },
  // ——— More services ———
  { type: "yandex", label: "Yandex Disk", group: "More services", oauth: true, fields: [] },
  { type: "mailru", label: "Mail.ru Cloud", group: "More services", fields: USER_PASS },
  { type: "opendrive", label: "OpenDrive", group: "More services", fields: USER_PASS },
  {
    type: "putio", label: "Put.io", group: "More services",
    fields: [{ key: "token", label: "OAuth token", secret: true }],
  },
  {
    type: "premiumizeme", label: "Premiumize.me", group: "More services",
    fields: [{ key: "api_key", label: "API key", secret: true }],
  },
  {
    type: "filelu", label: "FileLu", group: "More services",
    fields: [{ key: "key", label: "API key", secret: true }],
  },
];

interface MountReg {
  name: string;
  mountPoint: string;
}

const REG_KEY = "ff-cloud-mounts";

function loadReg(): MountReg[] {
  try {
    const p = JSON.parse(localStorage.getItem(REG_KEY) ?? "[]") as MountReg[];
    if (Array.isArray(p)) return p;
  } catch {
    // ignore
  }
  return [];
}

export interface CloudHooks {
  /** Run a command line in the built-in terminal (OAuth flow). */
  runInTerminal(command: string): Promise<void>;
  /** Register a mounted folder as a sidebar location. */
  addLocation(path: string, label: string): Promise<void>;
  removeLocation(path: string): Promise<void>;
  refreshSidebar(): Promise<void>;
  /** The user's home directory (mount points live under ~/Cloud). */
  homePath(): Promise<string>;
}

export class CloudMount {
  readonly element: HTMLDivElement;
  private backendSel: HTMLSelectElement;
  private fieldsEl: HTMLDivElement;
  private nameInput: HTMLInputElement;
  private tokenArea: HTMLTextAreaElement;
  private advancedArea: HTMLTextAreaElement;
  private oauthRow: HTMLDivElement;
  private mountsEl: HTMLDivElement;
  private statusEl: HTMLDivElement;

  constructor(private hooks: CloudHooks) {
    this.element = document.createElement("div");
    this.element.className = "props-overlay";
    this.element.hidden = true;
    const dialog = document.createElement("div");
    dialog.className = "cloud-dialog";
    dialog.setAttribute("role", "dialog");
    dialog.setAttribute("aria-label", "Mount Cloud Drive");
    const title = document.createElement("h2");
    title.className = "cloud-title";
    title.textContent = "☁ Mount Cloud Drive";
    const sub = document.createElement("p");
    sub.className = "cloud-sub";
    sub.textContent =
      "Powered by rclone. Mounts install a per-drive service, so they come back at every login and survive restarts.";

    const form = document.createElement("div");
    form.className = "cloud-form";
    const nameLabel = document.createElement("label");
    nameLabel.textContent = "Drive name (no spaces):";
    this.nameInput = document.createElement("input");
    this.nameInput.className = "cloud-input";
    this.nameInput.placeholder = "gdrive, my-nextcloud…";
    const typeLabel = document.createElement("label");
    typeLabel.textContent = "Service:";
    this.backendSel = document.createElement("select");
    this.backendSel.className = "cloud-input";
    const groups = new Map<string, HTMLOptGroupElement>();
    RCLONE_BACKENDS.forEach((b, i) => {
      let g = groups.get(b.group);
      if (!g) {
        g = document.createElement("optgroup");
        g.label = b.group;
        groups.set(b.group, g);
        this.backendSel.appendChild(g);
      }
      const opt = document.createElement("option");
      opt.value = String(i);
      opt.textContent = b.label;
      g.appendChild(opt);
    });
    this.backendSel.addEventListener("change", () => this.renderFields());
    this.fieldsEl = document.createElement("div");
    this.fieldsEl.className = "cloud-fields";
    this.oauthRow = document.createElement("div");
    this.oauthRow.className = "cloud-oauth";
    const authBtn = document.createElement("button");
    authBtn.className = "pill-btn";
    authBtn.textContent = "Authorize in Terminal…";
    authBtn.addEventListener("click", () => void this.authorize());
    const tokenLabel = document.createElement("label");
    tokenLabel.textContent = "…then paste the token it prints:";
    this.tokenArea = document.createElement("textarea");
    this.tokenArea.className = "cloud-input";
    this.tokenArea.rows = 3;
    this.tokenArea.placeholder = '{"access_token":"…"}';
    this.oauthRow.append(authBtn, tokenLabel, this.tokenArea);
    const advLabel = document.createElement("label");
    advLabel.textContent = "Advanced (one key=value per line, any rclone option):";
    this.advancedArea = document.createElement("textarea");
    this.advancedArea.className = "cloud-input";
    this.advancedArea.rows = 2;
    form.append(
      nameLabel, this.nameInput, typeLabel, this.backendSel,
      this.fieldsEl, this.oauthRow, advLabel, this.advancedArea,
    );

    const createRow = document.createElement("div");
    createRow.className = "cloud-createrow";
    const createBtn = document.createElement("button");
    createBtn.className = "pill-btn primary";
    createBtn.textContent = "Create & Mount";
    createBtn.addEventListener("click", () => void this.createAndMount());
    createRow.appendChild(createBtn);

    const mountsTitle = document.createElement("h3");
    mountsTitle.className = "cloud-mounts-title";
    mountsTitle.textContent = "Your cloud drives";
    this.mountsEl = document.createElement("div");
    this.mountsEl.className = "cloud-mounts";
    this.statusEl = document.createElement("div");
    this.statusEl.className = "cloud-status";
    const foot = document.createElement("div");
    foot.className = "cloud-foot";
    const closeBtn = document.createElement("button");
    closeBtn.className = "pill-btn";
    closeBtn.textContent = "Close";
    closeBtn.addEventListener("click", () => this.close());
    foot.appendChild(closeBtn);
    dialog.append(
      title, sub, form, createRow, mountsTitle, this.mountsEl,
      this.statusEl, foot,
    );
    this.element.appendChild(dialog);
    this.element.addEventListener("mousedown", (e) => {
      if (e.target === this.element) this.close();
    });
    this.renderFields();
  }

  private backend(): BackendDef {
    return RCLONE_BACKENDS[Number(this.backendSel.value)] ?? RCLONE_BACKENDS[0];
  }

  private renderFields(): void {
    const b = this.backend();
    this.fieldsEl.replaceChildren();
    for (const f of b.fields) {
      const label = document.createElement("label");
      label.textContent = `${f.label}:`;
      const input = document.createElement("input");
      input.className = "cloud-input";
      input.dataset.key = f.key;
      input.type = f.secret ? "password" : "text";
      if (f.placeholder) input.placeholder = f.placeholder;
      this.fieldsEl.append(label, input);
    }
    this.oauthRow.hidden = !b.oauth;
  }

  async open(): Promise<void> {
    this.element.hidden = false;
    await this.reconcile();
    await this.renderMounts();
  }

  close(): void {
    this.element.hidden = true;
  }

  /** On open: make sure registered mounts show in the sidebar. */
  private async reconcile(): Promise<void> {
    for (const m of loadReg()) {
      await this.hooks.addLocation(m.mountPoint, `☁ ${m.name}`);
    }
  }

  private async authorize(): Promise<void> {
    const b = this.backend();
    await this.hooks.runInTerminal(`rclone authorize ${b.type}`);
    Toast.info(
      "Sign in in the browser rclone opens, then paste the token it prints in the terminal.",
    );
  }

  private collectParams(): { params: Array<[string, string]>; obscure: string[] } {
    const b = this.backend();
    const params: Array<[string, string]> = [];
    const obscure: string[] = [];
    this.fieldsEl.querySelectorAll<HTMLInputElement>("input").forEach((input) => {
      const key = input.dataset.key;
      const value = input.value.trim();
      if (!key || !value) return;
      params.push([key, value]);
      const def = b.fields.find((f) => f.key === key);
      if (def?.secret) obscure.push(key);
    });
    if (b.oauth && this.tokenArea.value.trim()) {
      params.push(["token", this.tokenArea.value.trim()]);
    }
    for (const line of this.advancedArea.value.split("\n")) {
      const eq = line.indexOf("=");
      if (eq > 0) {
        params.push([
          line.slice(0, eq).trim(),
          line.slice(eq + 1).trim(),
        ]);
      }
    }
    return { params, obscure };
  }

  private mountPointCache = new Map<string, string>();
  private async mountPointFor(name: string): Promise<string> {
    const cached = this.mountPointCache.get(name);
    if (cached) return cached;
    const home = await this.hooks.homePath();
    const mp = `${home.replace(/\/$/, "")}/Cloud/${name}`;
    this.mountPointCache.set(name, mp);
    return mp;
  }

  private async createAndMount(): Promise<void> {
    const name = this.nameInput.value.trim().replace(/\s+/g, "-");
    if (!name) {
      Toast.error("Give the drive a name first.");
      return;
    }
    const b = this.backend();
    const { params, obscure } = this.collectParams();
    this.statusEl.textContent = `Creating ${name} (${b.label})…`;
    try {
      const existing = await api.rcloneRemotes();
      if (!existing.includes(name)) {
        await api.rcloneCreateRemote(name, b.type, params, obscure);
      }
      const mountPoint = await this.mountPointFor(name);
      await api.rcloneMount(name, mountPoint);
      const reg = loadReg().filter((m) => m.name !== name);
      reg.push({ name, mountPoint });
      localStorage.setItem(REG_KEY, JSON.stringify(reg));
      await this.hooks.addLocation(mountPoint, `☁ ${name}`);
      await this.hooks.refreshSidebar();
      this.statusEl.textContent = `✓ ${name} mounted at ${mountPoint} — and it'll be back at next login.`;
      Toast.success(`Cloud drive ${name} mounted.`);
      await this.renderMounts();
    } catch (err) {
      this.statusEl.textContent = `Failed: ${err}`;
      Toast.error(`Mount failed: ${err}`);
    }
  }

  private async renderMounts(): Promise<void> {
    this.mountsEl.replaceChildren();
    let remotes: string[] = [];
    try {
      remotes = await api.rcloneRemotes();
    } catch {
      // rclone missing — the create flow will surface it
    }
    const reg = loadReg();
    if (remotes.length === 0) {
      const empty = document.createElement("div");
      empty.className = "cloud-empty";
      empty.textContent = "No cloud drives yet — create your first above.";
      this.mountsEl.appendChild(empty);
      return;
    }
    for (const name of remotes) {
      const row = document.createElement("div");
      row.className = "cloud-mount-row";
      const entry = reg.find((m) => m.name === name);
      const label = document.createElement("span");
      label.textContent = `☁ ${name}${entry ? ` — ${entry.mountPoint}` : " (not mounted)"}`;
      row.appendChild(label);
      if (entry) {
        const unmount = document.createElement("button");
        unmount.className = "pill-btn";
        unmount.textContent = "Unmount";
        unmount.addEventListener("click", async () => {
          try {
            await api.rcloneUnmount(name, entry.mountPoint);
            localStorage.setItem(
              REG_KEY,
              JSON.stringify(loadReg().filter((m) => m.name !== name)),
            );
            await this.hooks.removeLocation(entry.mountPoint);
            await this.hooks.refreshSidebar();
            await this.renderMounts();
          } catch (err) {
            Toast.error(`Unmount failed: ${err}`);
          }
        });
        row.appendChild(unmount);
      } else {
        const mount = document.createElement("button");
        mount.className = "pill-btn";
        mount.textContent = "Mount";
        mount.addEventListener("click", async () => {
          try {
            const mp = await this.mountPointFor(name);
            await api.rcloneMount(name, mp);
            const r = loadReg();
            r.push({ name, mountPoint: mp });
            localStorage.setItem(REG_KEY, JSON.stringify(r));
            await this.hooks.addLocation(mp, `☁ ${name}`);
            await this.hooks.refreshSidebar();
            await this.renderMounts();
          } catch (err) {
            Toast.error(`Mount failed: ${err}`);
          }
        });
        row.appendChild(mount);
      }
      const del = document.createElement("button");
      del.className = "pill-btn danger";
      del.textContent = "Forget";
      del.addEventListener("click", async () => {
        try {
          if (entry) {
            await api.rcloneUnmount(name, entry.mountPoint);
            await this.hooks.removeLocation(entry.mountPoint);
          }
          await api.rcloneDeleteRemote(name);
          localStorage.setItem(
            REG_KEY,
            JSON.stringify(loadReg().filter((m) => m.name !== name)),
          );
          await this.hooks.refreshSidebar();
          await this.renderMounts();
        } catch (err) {
          Toast.error(`Forget failed: ${err}`);
        }
      });
      row.appendChild(del);
      this.mountsEl.appendChild(row);
    }
  }
}
