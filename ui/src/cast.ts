/**
 * CastDialog — send the current media file to a Chromecast (catt),
 * Apple TV (pyatv) or Sonos speaker (soco). The file is served by
 * Fable's built-in LAN media server, so seeking works.
 */
import { api } from "./api";
import type { Item } from "./api";
import { Toast } from "./toast";
import { isVideoName } from "./video";

interface Device {
  name: string;
  kind: string;
  address: string;
}

const KIND_ICON: Record<string, string> = {
  chromecast: "📺",
  airplay: "🍏",
  sonos: "🔊",
};
const KIND_LABEL: Record<string, string> = {
  chromecast: "Chromecast",
  airplay: "AirPlay",
  sonos: "Sonos",
};

export class CastDialog {
  readonly element: HTMLDivElement;
  private listEl: HTMLDivElement;
  private statusEl: HTMLDivElement;
  private stopBtn: HTMLButtonElement;
  private item: Item | null = null;
  private playing: Device | null = null;

  constructor() {
    this.element = document.createElement("div");
    this.element.className = "props-overlay";
    this.element.hidden = true;
    const dialog = document.createElement("div");
    dialog.className = "cast-dialog";
    dialog.setAttribute("role", "dialog");
    dialog.setAttribute("aria-label", "Cast to device");
    const title = document.createElement("h2");
    title.className = "cast-title";
    title.textContent = "📡 Cast";
    this.listEl = document.createElement("div");
    this.listEl.className = "cast-list";
    this.statusEl = document.createElement("div");
    this.statusEl.className = "cast-status";
    const foot = document.createElement("div");
    foot.className = "cast-foot";
    const rescan = document.createElement("button");
    rescan.className = "pill-btn";
    rescan.textContent = "Rescan";
    rescan.addEventListener("click", () => void this.scan());
    this.stopBtn = document.createElement("button");
    this.stopBtn.className = "pill-btn";
    this.stopBtn.textContent = "Stop Playback";
    this.stopBtn.hidden = true;
    this.stopBtn.addEventListener("click", () => void this.stop());
    const close = document.createElement("button");
    close.className = "pill-btn";
    close.textContent = "Close";
    close.addEventListener("click", () => this.close());
    foot.append(rescan, this.stopBtn, close);
    dialog.append(title, this.listEl, this.statusEl, foot);
    this.element.appendChild(dialog);
    this.element.addEventListener("mousedown", (e) => {
      if (e.target === this.element) this.close();
    });
  }

  async open(item: Item): Promise<void> {
    this.item = item;
    this.element.hidden = false;
    this.statusEl.textContent = `“${item.name}” — pick a device.`;
    await this.scan();
  }

  close(): void {
    this.element.hidden = true;
  }

  private async scan(): Promise<void> {
    this.listEl.replaceChildren();
    const scanning = document.createElement("div");
    scanning.className = "cast-empty";
    scanning.textContent = "Scanning your network (Chromecast · AirPlay · Sonos)…";
    this.listEl.appendChild(scanning);
    try {
      const devices = await api.castScan();
      this.render(devices);
    } catch (err) {
      this.listEl.replaceChildren();
      this.statusEl.textContent = `Scan failed: ${err}`;
    }
  }

  private render(devices: Device[]): void {
    this.listEl.replaceChildren();
    if (devices.length === 0) {
      const empty = document.createElement("div");
      empty.className = "cast-empty";
      empty.textContent =
        "No cast devices found. Devices must be on the same Wi-Fi/LAN and awake.";
      this.listEl.appendChild(empty);
      return;
    }
    for (const d of devices) {
      const row = document.createElement("button");
      row.className = "cast-device";
      const isVideo = this.item ? isVideoName(this.item.name) : false;
      row.textContent =
        `${KIND_ICON[d.kind] ?? "📡"} ${d.name} · ${KIND_LABEL[d.kind] ?? d.kind}` +
        (d.kind === "sonos" && isVideo ? " (audio only)" : "");
      row.addEventListener("click", () => void this.play(d));
      this.listEl.appendChild(row);
    }
  }

  private async play(device: Device): Promise<void> {
    if (!this.item) return;
    this.statusEl.textContent = `Starting on ${device.name}…`;
    try {
      const url = await api.castRegisterFile(this.item.id);
      await api.castPlay(device, url);
      this.playing = device;
      this.stopBtn.hidden = false;
      this.statusEl.textContent = `▶ Playing on ${device.name}.`;
      Toast.success(`Casting to ${device.name}.`);
    } catch (err) {
      this.statusEl.textContent = `Cast failed: ${err}`;
      Toast.error(`Cast failed: ${err}`);
    }
  }

  private async stop(): Promise<void> {
    if (!this.playing) return;
    try {
      await api.castStop(this.playing);
      this.statusEl.textContent = `Stopped ${this.playing.name}.`;
    } catch (err) {
      Toast.error(`Stop failed: ${err}`);
    }
    this.playing = null;
    this.stopBtn.hidden = true;
  }
}
