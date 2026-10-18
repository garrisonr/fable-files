import { errText } from "./toast";
/**
 * Fable Files — adaptive shell entry point.
 *
 * One shell, three layout classes (spec: adaptive shell):
 * - desktop:  full chrome (sidebar, tab bar, inspector)
 * - handheld: full browse/search/preview; compact chrome
 * - tv:       media-first browse, reduced capability policy
 *
 * The shell renders ONLY from --ufm-* theme tokens installed by the theme
 * manager. No hard-coded colors anywhere in this tree.
 */
import { Shell } from "./shell";
import { maybeShowFirstRunWelcome } from "./welcome";
import { VideoPlayer } from "./video";
import { api } from "./api";
import { emit } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import "./styles.css";
import "./distro-themes.css";

type LayoutClass = "desktop" | "handheld" | "tv";

function detectLayout(): LayoutClass {
  const w = window.innerWidth;
  const coarse = window.matchMedia("(pointer: coarse)").matches;
  const tv =
    window.matchMedia("(display-mode: fullscreen)").matches && w > 1600;
  if (tv) return "tv";
  if (coarse || w < 820) return "handheld";
  return "desktop";
}

async function boot(): Promise<void> {
  document.body.dataset.layout = detectLayout();
  window.addEventListener("resize", () => {
    document.body.dataset.layout = detectLayout();
  });

  const shell = new Shell();
  document.getElementById("app-root")!.appendChild(shell.el);
  // The first-run welcome (2026-10-16): a self-playing presentation
  // over the booting shell. Not awaited — the shell boots underneath
  // it, and the tour's own gate decides whether it stays.
  maybeShowFirstRunWelcome();
  try {
    await shell.boot();
  } catch (e) {
    // Never leave a blank window: surface the failure in the status bar.
    const status = shell.el.querySelector(".status-left");
    if (status) status.textContent = `Boot failed: ${errText(e)}`;
  }
}

/** Detach (pop-out) player windows boot here instead of the full
 * shell. UI round item 5: the window receives the playback state
 * (start position + autoplay) so playback continues mid-stream,
 * streams its own state home while it plays, and its ⤺ button
 * hands the final position back before closing — the trip is
 * bidirectional, nothing restarts from zero in either direction. */
function bootPlayer(params: URLSearchParams): void {
  const kind = params.get("kind") ?? "video";
  if (kind === "audio") {
    bootAudioPlayer(params);
    return;
  }
  if (kind === "audio-stage") {
    bootStagePlayer(params);
    return;
  }
  document.body.classList.add("player-body");
  const path = params.get("path") ?? "";
  const name = params.get("name") ?? "Video";
  document.title = `${name} — Fable Files`;
  const root = document.getElementById("app-root")!;
  const start = Number(params.get("start") ?? 0) || 0;
  const autoplay = params.get("autoplay") === "1";
  const player = new VideoPlayer({
    path,
    standalone: true,
    startAt: start,
    autoPlay: autoplay,
    onState: (t, playing) => {
      void emit("player-state", { path, t, playing }).catch(
        () => undefined,
      );
    },
    onReturn: () => {
      void emit("player-return", {
        path,
        t: player.position(),
        playing: player.isPlaying(),
      })
        .catch(() => undefined)
        .finally(() => void getCurrentWebviewWindow().close());
    },
  });
  root.appendChild(player.el);
  // The window closing IS the surface closing: tear playback down
  // with it (fix block item 3) — but hand the final position home
  // FIRST (fix batch item 4): a plain window close carries no
  // ⤺ click, so without this the trip home resumes from the last
  // streamed state (up to 5 s stale) instead of where it stopped.
  window.addEventListener("beforeunload", () => {
    void emit("player-state", {
      path,
      t: player.position(),
      playing: player.isPlaying(),
    }).catch(() => undefined);
    player.destroy();
  });
}

/** The audio detach window: the Mini arrangement of the shared
 * transport core (Music Glow-Up Phase 2). The window boots from
 * the core's snapshot, claims ownership, and renders the shared
 * TransportBar — same queue, same position, same volume as every
 * other surface; its ⤺ button claims the Panel back and closes.
 * The queue lives in the core; when the track ends the core
 * advances it — no return-packet choreography remains. */
function bootAudioPlayer(params: URLSearchParams): void {
  document.body.classList.add("player-body", "audio-player-body");
  const path = params.get("path") ?? "";
  const name = params.get("name") ?? "Audio";
  document.title = `${name} — Fable Files`;
  const root = document.getElementById("app-root")!;
  const start = Number(params.get("start") ?? 0) || 0;
  const autoplay = params.get("autoplay") === "1";

  const wrap = document.createElement("div");
  wrap.className = "mini-audio";
  const art = document.createElement("img");
  art.className = "mini-audio-art";
  art.alt = "";
  art.hidden = true;
  const glyph = document.createElement("div");
  glyph.className = "mini-audio-glyph";
  glyph.textContent = "🎵";
  art.addEventListener("error", () => {
    art.hidden = true;
    glyph.hidden = false;
  });
  const main = document.createElement("div");
  main.className = "mini-audio-main";
  const title = document.createElement("div");
  title.className = "mini-audio-title";
  title.textContent = name.replace(/\.[^.]+$/, "").replace(/_+/g, " ").trim();
  const sub = document.createElement("div");
  sub.className = "mini-audio-sub";
  main.append(title, sub);
  wrap.append(art, glyph, main);
  root.appendChild(wrap);
  void (async () => {
    try {
      const stashed = await api.getPlayerArt(path);
      const url = stashed ?? (await api.getThumbnail("", path, true));
      if (url) {
        art.src = url;
        art.hidden = false;
        glyph.hidden = true;
      }
    } catch {
      // no art anywhere — the glyph stays
    }
  })();

  void (async () => {
    const pc = await import("./playercore");
    const client = new pc.PlayerCoreClient();
    await client.start();
    const barHost = document.createElement("div");
    main.appendChild(barHost);
    type CastDlg = {
      openPicker: (n: string, v: boolean) => Promise<{ name: string; kind: string; address: string } | null>;
      element: HTMLDivElement;
    };
    let castDlg: CastDlg | null = null;
    const castPicker = async (n: string, v: boolean) => {
      if (!castDlg) {
        const { CastDialog } = await import("./cast");
        castDlg = new CastDialog();
        document.body.appendChild(castDlg.element);
      }
      return castDlg.openPicker(n, v);
    };
    // The bar is built first; its hooks reach the engine through
    // this late-bound reference (the engine's snapshot callback
    // fires the moment it subscribes).
    const eng: { e: import("./playercore").OwnerEngine | null } = { e: null };
    const cycleRepeat = () => {
      const s = client.snap;
      if (s) {
        void client.setRepeat(
          s.repeat === "off" ? "all" : s.repeat === "all" ? "one" : "off",
        );
      }
    };
    const bar = new pc.TransportBar(
      {
        onToggle: () => void eng.e?.toggle(),
        onStep: (d) => void client.step(d),
        onSeek: (s) => void eng.e?.seekTo(s),
        onRepeat: cycleRepeat,
        onShuffle: () => {
          const s = client.snap;
          if (s) void client.setShuffle(!s.shuffle);
        },
        onVolume: (v) => void client.setVolume(v),
        onMute: () => {
          const s = client.snap;
          if (s) void client.setMuted(!s.muted);
        },
        onCast: () => void eng.e?.startCastFlow(),
      },
      { compact: true },
    );
    barHost.appendChild(bar.el);
    const ret = document.createElement("button");
    ret.className = "tbar-btn mini-return";
    ret.textContent = "⤺";
    ret.title = "Return to the main window";
    ret.addEventListener("click", () => {
      void client
        .claim("panel")
        .catch(() => undefined)
        .finally(() => void getCurrentWebviewWindow().close());
    });
    bar.el.appendChild(ret);
    const engine = new pc.OwnerEngine("mini", client, {
      castPicker,
      onSnapshot: (s) => {
        bar.update(s);
        const e = s.index !== null ? s.queue[s.index] : null;
        if (e) {
          title.textContent = e.name
            .replace(/\.[^.]+$/, "")
            .replace(/_+/g, " ")
            .trim();
          sub.textContent =
            s.owner === "mini"
              ? `${pc.fmtClock(s.positionSecs)} / ${s.durationSecs > 0 ? pc.fmtClock(s.durationSecs) : "–:––"}`
              : "Playing elsewhere…";
        }
      },
    });
    eng.e = engine;
    // Claim: this window exists to play THIS path. When the core
    // is already cued to it, claim ownership in place; otherwise
    // open it as a single-entry queue (the legacy direct path).
    const snap = client.snap;
    const cur = snap && snap.index !== null ? snap.queue[snap.index] : null;
    if (cur && cur.providerKey === path) {
      await client.claim("mini");
    } else {
      await client.openQueue({
        owner: "mini",
        entries: [{ providerKey: path, name, durationSecs: null }],
        startIndex: 0,
        startPosition: start,
        autoplay,
      });
    }
    window.addEventListener("keydown", (e) => {
      const s = client.snap;
      if (!s) return;
      if (e.key === " ") {
        e.preventDefault();
        void engine.toggle();
      } else if (e.key === "ArrowRight") {
        void engine.seekTo(Math.min(s.durationSecs || 0, s.positionSecs + 5));
      } else if (e.key === "ArrowLeft") {
        void engine.seekTo(Math.max(0, s.positionSecs - 5));
      }
    });
    window.addEventListener("beforeunload", () => {
      // No element outlives its surface; the core lapses via the
      // window-closed event and the panel's tombstone watch.
      engine.destroy();
    });
  })();
}

/** The Stage: the third arrangement of the shared core (Music
 * Glow-Up Phase 2) — a real window with the big art, the full
 * lyrics, the spectrum, and the Up Next rail, all rendering from
 * the same snapshots the Panel renders. */
function bootStagePlayer(params: URLSearchParams): void {
  document.body.classList.add("player-body", "stage-player-body");
  const path = params.get("path") ?? "";
  const name = params.get("name") ?? "Audio";
  document.title = `${name} — Fable Files`;
  const root = document.getElementById("app-root")!;

  const wrap = document.createElement("div");
  wrap.className = "stage-audio";
  wrap.innerHTML = `
    <div class="stage-head">
      <div class="stage-art"><span class="stage-art-glyph">♪</span><img class="stage-art-img" alt="" hidden /></div>
      <div class="stage-meta">
        <div class="stage-title"></div>
        <div class="stage-sub"></div>
      </div>
      <span class="stage-badge"></span>
    </div>
    <div class="stage-banner" hidden></div>
    <div class="stage-main">
      <div class="stage-lyrics-host"></div>
      <div class="stage-rail"></div>
    </div>
    <div class="stage-spectrum" hidden></div>
    <div class="stage-bar-host"></div>`;
  root.appendChild(wrap);
  const q = <T extends HTMLElement>(sel: string) => wrap.querySelector(sel) as T;
  const titleEl = q<HTMLDivElement>(".stage-title");
  const subEl = q<HTMLDivElement>(".stage-sub");
  const badgeEl = q<HTMLSpanElement>(".stage-badge");
  const bannerEl = q<HTMLDivElement>(".stage-banner");
  const railEl = q<HTMLDivElement>(".stage-rail");
  const artImg = q<HTMLImageElement>(".stage-art-img");
  const artGlyph = q<HTMLSpanElement>(".stage-art-glyph");
  artImg.addEventListener("error", () => {
    artImg.hidden = true;
    artGlyph.hidden = false;
  });

  void (async () => {
    const pc = await import("./playercore");
    const { LyricsView } = await import("./lyrics");
    const { audioBadge } = await import("./audiometa");
    const client = new pc.PlayerCoreClient();
    await client.start();
    type CastDlg = {
      openPicker: (n: string, v: boolean) => Promise<{ name: string; kind: string; address: string } | null>;
      element: HTMLDivElement;
    };
    let castDlg: CastDlg | null = null;
    const castPicker = async (n: string, v: boolean) => {
      if (!castDlg) {
        const { CastDialog } = await import("./cast");
        castDlg = new CastDialog();
        document.body.appendChild(castDlg.element);
      }
      return castDlg.openPicker(n, v);
    };
    const lyrics = new LyricsView();
    lyrics.el.classList.add("stage-lyrics");
    q<HTMLDivElement>(".stage-lyrics-host").appendChild(lyrics.el);
    const spectrum = new pc.SpectrumCanvas();
    const specHost = q<HTMLDivElement>(".stage-spectrum");
    specHost.appendChild(spectrum.el);
    let spectrumOn = false;
    try {
      spectrumOn = localStorage.getItem("ff-music-spectrum") === "1";
    } catch { /* default off */ }
    specHost.hidden = !spectrumOn;
    void pc.onPlayerSpectrum((bands) => {
      if (spectrumOn) spectrum.draw(bands);
    }).catch(() => undefined);

    const eng: { e: import("./playercore").OwnerEngine | null } = { e: null };
    lyrics.onSeek = (secs) => void eng.e?.seekTo(secs);
    const bar = new pc.TransportBar(
      {
        onToggle: () => void eng.e?.toggle(),
        onStep: (d) => void client.step(d),
        onSeek: (s) => void eng.e?.seekTo(s),
        onRepeat: () => {
          const s = client.snap;
          if (s) {
            void client.setRepeat(
              s.repeat === "off" ? "all" : s.repeat === "all" ? "one" : "off",
            );
          }
        },
        onShuffle: () => {
          const s = client.snap;
          if (s) void client.setShuffle(!s.shuffle);
        },
        onVolume: (v) => void client.setVolume(v),
        onMute: () => {
          const s = client.snap;
          if (s) void client.setMuted(!s.muted);
        },
        onCast: () => void eng.e?.startCastFlow(),
      },
      {},
    );
    q<HTMLDivElement>(".stage-bar-host").appendChild(bar.el);

    let lyricsKey = "";
    let railKey = "";
    let artKey = "";
    const engine = new pc.OwnerEngine("stage", client, {
      castPicker,
      onSnapshot: (s) => {
        bar.update(s);
        const e = s.index !== null ? s.queue[s.index] : null;
        if (!e) return;
        titleEl.textContent = e.name
          .replace(/\.[^.]+$/, "")
          .replace(/_+/g, " ")
          .trim();
        subEl.textContent =
          s.owner === "stage" ? "" : "Playing in another window…";
        badgeEl.textContent = audioBadge(e.name);
        if (s.cast) {
          bannerEl.hidden = false;
          bannerEl.textContent = `📡 Casting to ${s.cast.deviceName}`;
        } else {
          bannerEl.hidden = true;
        }
        const lkey = `${e.providerKey}:${s.playToken}`;
        if (lkey !== lyricsKey) {
          lyricsKey = lkey;
          void lyrics.load(e.providerKey);
        }
        if (lyrics.isSynced) lyrics.setPosition(s.positionSecs * 1000);
        const pos = s.index !== null ? s.order.indexOf(s.index) : -1;
        const nextIdx = s.order.slice(pos + 1);
        const rkey = nextIdx.join(",") + ":" + (s.index ?? -1);
        if (rkey !== railKey) {
          railKey = rkey;
          railEl.innerHTML = "";
          const h = document.createElement("div");
          h.className = "music-rail-head";
          h.innerHTML = "<span>Up Next</span>";
          railEl.appendChild(h);
          for (const qi of nextIdx.slice(0, 30)) {
            const entry = s.queue[qi];
            if (!entry) continue;
            const row = document.createElement("div");
            row.className = "music-rail-row";
            const t = document.createElement("span");
            t.className = "music-rail-t";
            t.textContent = entry.name
              .replace(/\.[^.]+$/, "")
              .replace(/_+/g, " ")
              .trim();
            t.addEventListener("click", () => void client.playIndex(qi));
            const x = document.createElement("button");
            x.className = "music-row-x";
            x.textContent = "✕";
            x.title = "Remove from queue";
            x.addEventListener("click", () => void client.remove(qi));
            row.append(t, x);
            railEl.appendChild(row);
          }
        }
        if (artKey !== e.providerKey) {
          artKey = e.providerKey;
          void (async () => {
            try {
              const stashed = await api.getPlayerArt(e.providerKey);
              const url =
                stashed ?? (await api.getThumbnail("", e.providerKey, true));
              if (url && artKey === e.providerKey) {
                artImg.src = url;
                artImg.hidden = false;
                artGlyph.hidden = true;
              }
            } catch { /* glyph stays */ }
          })();
        }
      },
    });
    eng.e = engine;
    const feed = new pc.SpectrumFeed(engine, () => client.isOwner("stage"));
    if (spectrumOn) {
      if (!feed.enable()) specHost.hidden = true;
    }
    // Claim the sound for the Stage (the window was opened from a
    // playing surface for exactly this).
    const snap = client.snap;
    if (snap && snap.queue.length > 0) {
      await client.claim("stage");
    } else {
      await client.openQueue({
        owner: "stage",
        entries: [{ providerKey: path, name, durationSecs: null }],
        startIndex: 0,
        startPosition: Number(params.get("start") ?? 0) || 0,
        autoplay: params.get("autoplay") === "1",
      });
    }
    client.installMediaSession(() => {
      const e = client.currentEntry();
      return { title: e ? e.name.replace(/\.[^.]+$/, "") : "Fable Files" };
    });
    window.addEventListener("keydown", (e2) => {
      const s = client.snap;
      if (!s) return;
      if (e2.key === " ") {
        e2.preventDefault();
        void engine.toggle();
      } else if (e2.key === "ArrowRight") {
        void engine.seekTo(Math.min(s.durationSecs || 0, s.positionSecs + 5));
      } else if (e2.key === "ArrowLeft") {
        void engine.seekTo(Math.max(0, s.positionSecs - 5));
      }
    });
    window.addEventListener("beforeunload", () => {
      // Hand the sound back to the panel on the way out.
      if (client.snap?.owner === "stage") {
        void client.claim("panel").catch(() => undefined);
      }
      engine.destroy();
    });
  })();
}

const bootParams = new URLSearchParams(location.search);
if (bootParams.get("mode") === "player") {
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", () => bootPlayer(bootParams));
  } else {
    bootPlayer(bootParams);
  }
} else if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", () => void boot());
} else {
  void boot();
}
