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
import { VideoPlayer } from "./video";
import { api, mediaSrc } from "./api";
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
  if ((params.get("kind") ?? "video") === "audio") {
    bootAudioPlayer(params);
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

/** The audio detach window: a compact transport (the Music
 * Player's mini sibling) — title, play/pause, seek, and ⤺ home.
 * The queue stays in the Music Player; when the track ends here,
 * the return handoff carries ended=true and the main player
 * advances the queue itself. */
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
  // Cover art (fix batch item 3): the mini player wears the same
  // face as the inline preview — art the sender resolved rides
  // the open_player_window stash; the fallback is the embedded
  // art for THIS path through the thumbnailer. A filename on a
  // black card is not a player.
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
  title.textContent = name;
  const bar = document.createElement("div");
  bar.className = "mini-audio-bar";
  const playBtn = document.createElement("button");
  playBtn.className = "vp-btn";
  playBtn.textContent = "▶";
  playBtn.title = "Play / pause (Space)";
  const seek = document.createElement("input");
  seek.className = "vp-seek";
  seek.type = "range";
  seek.min = "0";
  seek.max = "1000";
  seek.value = "0";
  const time = document.createElement("span");
  time.className = "vp-time";
  time.textContent = "0:00 / 0:00";
  const ret = document.createElement("button");
  ret.className = "vp-btn vp-return";
  ret.textContent = "⤺ Return";
  ret.title = "Return to the main window";
  bar.append(playBtn, seek, time, ret);
  main.append(title, bar);
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

  const audio = new Audio();
  audio.preload = "metadata";
  const fmt = (sec: number): string => {
    if (!isFinite(sec) || sec < 0) sec = 0;
    const m = Math.floor(sec / 60);
    const s = Math.floor(sec % 60);
    return `${m}:${String(s).padStart(2, "0")}`;
  };
  const sendState = (): void => {
    void emit("player-state", {
      path,
      t: audio.currentTime,
      playing: !audio.paused && !audio.ended,
    }).catch(() => undefined);
  };
  const sync = (): void => {
    const d = audio.duration;
    const c = audio.currentTime;
    if (isFinite(d) && d > 0) {
      seek.value = String(Math.round((c / d) * 1000));
    }
    time.textContent = `${fmt(c)} / ${fmt(d)}`;
    playBtn.textContent = audio.paused ? "▶" : "⏸";
  };
  void mediaSrc(path).then((url) => {
    audio.src = url;
  });
  audio.addEventListener("loadedmetadata", () => {
    if (start > 0 && isFinite(audio.duration)) {
      try {
        audio.currentTime = Math.min(
          start,
          Math.max(0, audio.duration - 0.25),
        );
      } catch {
        // not seekable yet — plays from the top instead
      }
    }
    if (autoplay) void audio.play().catch(() => undefined);
    sync();
  });
  let lastSent = 0;
  audio.addEventListener("timeupdate", () => {
    sync();
    if (Date.now() - lastSent > 5000) {
      lastSent = Date.now();
      sendState();
    }
  });
  audio.addEventListener("play", () => {
    sync();
    sendState();
  });
  audio.addEventListener("pause", () => {
    sync();
    sendState();
  });
  audio.addEventListener("seeked", sendState);
  audio.addEventListener("ended", () => {
    void emit("player-return", {
      path,
      t: audio.currentTime,
      playing: false,
      ended: true,
    })
      .catch(() => undefined)
      .finally(() => void getCurrentWebviewWindow().close());
  });
  playBtn.addEventListener("click", () => {
    if (audio.paused) void audio.play().catch(() => undefined);
    else audio.pause();
  });
  seek.addEventListener("input", () => {
    const d = audio.duration;
    if (isFinite(d) && d > 0) {
      audio.currentTime = (Number(seek.value) / 1000) * d;
    }
  });
  ret.addEventListener("click", () => {
    void emit("player-return", {
      path,
      t: audio.currentTime,
      playing: !audio.paused,
    })
      .catch(() => undefined)
      .finally(() => void getCurrentWebviewWindow().close());
  });
  window.addEventListener("keydown", (e) => {
    if (e.key === " ") {
      e.preventDefault();
      playBtn.click();
    } else if (e.key === "ArrowRight") {
      audio.currentTime = Math.min(audio.duration || 0, audio.currentTime + 5);
    } else if (e.key === "ArrowLeft") {
      audio.currentTime = Math.max(0, audio.currentTime - 5);
    }
  });
  window.addEventListener("beforeunload", () => {
    sendState();
    // No media element outlives its surface (fix block item 3).
    audio.pause();
    audio.removeAttribute("src");
    audio.load();
  });
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
