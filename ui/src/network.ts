/**
 * Network Locations — the Connect to Server dialog.
 *
 * One dialog for every scheme (smb / sftp / ftp / ftps / dav / davs /
 * nfs / afp): address, optional sign-in, and a Remember choice. The
 * password goes to the backend, which keeps it ONLY in the OS
 * keyring (and only when Remember is checked and the mount works) —
 * it is never written into a stored address or bookmark here.
 *
 * Scheme help text is honest about each protocol (the ROM Media
 * guidance law: settings teach). AFP's note carries the audit
 * finding: the client works; modern Apple systems stopped SERVING
 * AFP, so an afp:// target means a legacy server.
 */
import { api, type LocationSummary } from "./api";
import { Toast, errText } from "./toast";

const SCHEME_HELP: Record<string, string> = {
  smb: "Windows shares and most NAS boxes. Example: smb://nas.local/photos",
  sftp: "SSH file transfer — any server you can ssh into. Example: sftp://user@server/share",
  ssh: "SSH file transfer — any server you can ssh into. Example: ssh://user@server/share",
  ftp: "Plain FTP. Heads-up: the password crosses the network unencrypted — prefer ftps:// or sftp:// when the server offers them.",
  ftps: "FTP over TLS — the encrypted form of FTP.",
  dav: "WebDAV — Nextcloud, ownCloud, and many NAS web shares. Plain dav:// is unencrypted; davs:// is the encrypted form.",
  davs: "WebDAV over HTTPS — Nextcloud, ownCloud, and many NAS web shares.",
  nfs: "An NFS export, like nfs://server/export/path. (A share already mounted by the system shows up as a regular drive on its own.)",
  afp: "Apple Filing Protocol — Apple's older file sharing. Fable can connect, but modern Macs no longer serve AFP, so this reaches legacy servers: Netatalk, Time Capsules, older NAS boxes. For a current Mac, use smb:// instead.",
};

const DEFAULT_HELP =
  "Type a server address: smb:// · sftp:// · ftp:// · ftps:// · dav:// · davs:// · nfs:// · afp://";

export function schemeOfAddress(address: string): string | null {
  const m = address.trim().toLowerCase().match(/^([a-z][a-z0-9+.-]*):\/\//);
  return m ? m[1] : null;
}

let openOverlay: HTMLElement | null = null;

/**
 * Open the Connect to Server dialog. `onConnected` receives the
 * fresh online LocationSummary; the caller refreshes + navigates.
 */
export function openConnectDialog(
  opts: { initialUri?: string; initialUsername?: string },
  onConnected: (loc: LocationSummary) => void,
): void {
  openOverlay?.remove();
  const overlay = document.createElement("div");
  overlay.className = "confirm-overlay";
  openOverlay = overlay;
  overlay.innerHTML = `
    <div class="confirm-card net-card" role="dialog" aria-label="Connect to Server">
      <h3>Connect to Server</h3>
      <label class="net-field">
        <span>Server address</span>
        <input class="prompt-input net-address" type="text" spellcheck="false"
               placeholder="sftp://user@server/share" />
      </label>
      <p class="net-help"></p>
      <label class="net-field">
        <span>Username</span>
        <input class="prompt-input net-user" type="text" spellcheck="false" autocomplete="username" />
      </label>
      <label class="net-field">
        <span>Password</span>
        <input class="prompt-input net-pass" type="password" autocomplete="current-password" />
      </label>
      <label class="net-field">
        <span>Domain / workgroup <em>(SMB only)</em></span>
        <input class="prompt-input net-domain" type="text" spellcheck="false" />
      </label>
      <label class="net-check">
        <input type="checkbox" class="net-anon" /> Connect without signing in (anonymous / guest)
      </label>
      <label class="net-check">
        <input type="checkbox" class="net-remember" checked /> Remember the password in the system keyring
      </label>
      <div class="net-error" role="alert" hidden>
        <span class="net-error-text"></span>
        <button class="net-error-copy" type="button">Copy</button>
      </div>
      <div class="confirm-actions">
        <button class="confirm-cancel">Cancel</button>
        <button class="confirm-ok primary net-connect-btn">Connect</button>
      </div>
    </div>`;
  document.body.appendChild(overlay);

  const $ = <T extends HTMLElement>(sel: string) =>
    overlay.querySelector(sel) as T;
  const address = $<HTMLInputElement>(".net-address");
  const help = $<HTMLElement>(".net-help");
  const user = $<HTMLInputElement>(".net-user");
  const pass = $<HTMLInputElement>(".net-pass");
  const domain = $<HTMLInputElement>(".net-domain");
  const anon = $<HTMLInputElement>(".net-anon");
  const remember = $<HTMLInputElement>(".net-remember");
  const errBox = $<HTMLElement>(".net-error");
  const errTextEl = $<HTMLElement>(".net-error-text");
  const connectBtn = $<HTMLButtonElement>(".net-connect-btn");

  address.value = opts.initialUri ?? "";
  user.value = opts.initialUsername ?? "";
  help.textContent = DEFAULT_HELP;

  const refreshHelp = () => {
    const scheme = schemeOfAddress(address.value);
    help.textContent = (scheme && SCHEME_HELP[scheme]) || DEFAULT_HELP;
    // An address can carry the user (sftp://user@host) — mirror it
    // into the username field when that's still empty.
    const m = address.value.match(/^[a-z]+:\/\/([^/@:]+)@/i);
    if (m && !user.value) user.value = m[1];
  };
  address.addEventListener("input", refreshHelp);
  refreshHelp();

  const refreshAnon = () => {
    const off = anon.checked;
    user.disabled = off;
    pass.disabled = off;
    domain.disabled = off;
    remember.disabled = off;
  };
  anon.addEventListener("change", refreshAnon);

  const close = () => {
    overlay.remove();
    if (openOverlay === overlay) openOverlay = null;
  };
  $<HTMLButtonElement>(".confirm-cancel").addEventListener("click", close);
  overlay.addEventListener("pointerdown", (e) => {
    if (e.target === overlay) close();
  });
  overlay.addEventListener("keydown", (e) => {
    if (e.key === "Escape") close();
    e.stopPropagation();
  });
  $<HTMLButtonElement>(".net-error-copy").addEventListener("click", () => {
    void navigator.clipboard
      .writeText(errTextEl.textContent ?? "")
      .then(() => Toast.success("Error copied."));
  });

  const submit = async () => {
    const uri = address.value.trim();
    if (!uri) {
      address.focus();
      return;
    }
    errBox.hidden = true;
    connectBtn.disabled = true;
    connectBtn.textContent = "Connecting…";
    try {
      const loc = await api.netConnect({
        uri,
        username: anon.checked ? undefined : user.value.trim() || undefined,
        password: anon.checked ? undefined : pass.value || undefined,
        domain: anon.checked ? undefined : domain.value.trim() || undefined,
        anonymous: anon.checked,
        rememberPassword: !anon.checked && remember.checked,
      });
      close();
      Toast.success(`Connected: ${loc.display_name}`);
      onConnected(loc);
    } catch (err) {
      // Stay open with the reason in plain view (and copyable) — a
      // failed sign-in is a fix-the-field moment, not a dead end.
      errTextEl.textContent = errText(err);
      errBox.hidden = false;
      connectBtn.disabled = false;
      connectBtn.textContent = "Connect";
    }
  };
  connectBtn.addEventListener("click", () => void submit());
  overlay.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && (e.target as HTMLElement).tagName === "INPUT") {
      e.preventDefault();
      void submit();
    }
  });

  address.focus();
  address.select();
}
