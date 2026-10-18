/**
 * CreditsDialog — the About → Credits page (THE CREDITS LAW,
 * Dickles 2026-10-18: every family app names its upstreams,
 * graciously, with what they give the app, their license, and a
 * link — full license texts one tap away, never pasted over the
 * thanks). Template: software-forge/templates/credits.md.
 *
 * Every entry here was audited against this tree on 2026-10-18
 * (Cargo manifests + registry license fields + in-tree usage) —
 * nothing is credited that Fable does not actually contain or
 * genuinely use. External companions are labeled NOT bundled.
 */
import { api } from "./api";
import { LICENSE_TEXTS } from "./licenses-data";

interface Credit {
  name: string;
  url: string;
  /** One warm plain sentence: what it gives Fable. */
  gives: string;
  license: string;
  holder?: string;
}

const CONTAINS: Credit[] = [
  {
    name: "Tauri",
    url: "https://tauri.app",
    gives:
      "The frame Fable is built in — its window, its bridge between Rust and the interface, and its packaging all ride on Tauri.",
    license: "Apache License 2.0 OR MIT License",
    holder: "The Tauri Programme in the Commons Conservancy",
  },
  {
    name: "WebKitGTK (WebKit)",
    url: "https://webkitgtk.org",
    gives:
      "The web engine that renders Fable's face — every button and shelf you see is painted by WebKit.",
    license: "GNU Lesser General Public License 2.1 (portions under other free licenses)",
    holder: "The WebKit contributors",
  },
  {
    name: "SQLite",
    url: "https://sqlite.org",
    gives:
      "The catalog itself — Fable's memory of every file lives in a bundled SQLite database (via the rusqlite crate, MIT).",
    license: "Public domain",
    holder: "D. Richard Hipp and the SQLite team",
  },
  {
    name: "ES-DE",
    url: "https://es-de.org",
    gives:
      "Fable's emulator picks and launch templates are mined from ES-DE's system definitions, so a hundred systems arrive already knowing how they're run.",
    license: "MIT License",
    holder: "The ES-DE project",
  },
  {
    name: "Playnite",
    url: "https://playnite.link",
    gives:
      "Fable reads Playnite's library format, so the PC game collection you built there walks into the Games shelf whole.",
    license: "MIT License",
    holder: "Josef Němec and contributors",
  },
  {
    name: "LiteDB",
    url: "https://www.litedb.org",
    gives:
      "The database format that importer decodes — Fable's reader is our own Rust, written against LiteDB's format.",
    license: "MIT License",
    holder: "Mauricio David and contributors",
  },
  {
    name: "MAME",
    url: "https://www.mamedev.org",
    gives:
      "The CHD disc-image format Fable opens on the games shelf comes from the MAME project, three decades of preservation engineering.",
    license: "BSD 3-Clause License (portions GNU GPL 2.0-or-later)",
    holder: "The MAME team",
  },
  {
    name: "Lofty",
    url: "https://github.com/Serial-ATA/lofty-rs",
    gives:
      "Lofty reads and writes the audio tags — titles, artists, lyrics, cover art — that Music Completion lives on.",
    license: "MIT License OR Apache License 2.0",
  },
  {
    name: "Claxon",
    url: "https://github.com/ruuda/claxon",
    gives: "A pure-Rust FLAC decoder; Fable's lossless playback path speaks Claxon.",
    license: "Apache License 2.0",
    holder: "Ruud van Asseldonk",
  },
  {
    name: "image (image-rs)",
    url: "https://github.com/image-rs/image",
    gives:
      "The image decoding behind previews and thumbnails — JPEG, PNG, WebP and friends.",
    license: "MIT License OR Apache License 2.0",
    holder: "The image-rs contributors",
  },
  {
    name: "wasmi",
    url: "https://github.com/wasmi-labs/wasmi",
    gives:
      "The WebAssembly interpreter inside Fable's plugin sandbox — plugins run in wasmi's jail, not on your machine's bare metal.",
    license: "Apache License 2.0 OR MIT License",
  },
  {
    name: "SCOWL",
    url: "http://wordlist.aspell.net",
    gives:
      "The word list bundled inside File Auditor — its spelling hunts run against SCOWL's dictionaries.",
    license: "SCOWL License (permissive)",
    holder: "Kevin Atkinson and contributors",
  },
  {
    name: "sherpa-onnx",
    url: "https://github.com/k2-fsa/sherpa-onnx",
    gives:
      "The speech engine inside Listen — it runs the narrator entirely on this machine, no cloud, no account, your books read aloud in your own house.",
    license: "Apache License 2.0",
    holder: "The k2-fsa contributors",
  },
  {
    name: "ONNX Runtime",
    url: "https://onnxruntime.ai",
    gives:
      "The neural-network runtime sherpa-onnx speaks through — the quiet arithmetic under every narrated sentence.",
    license: "MIT License",
    holder: "Microsoft and the ONNX Runtime contributors",
  },
  {
    name: "Kokoro-82M",
    url: "https://huggingface.co/hexgrad/Kokoro-82M",
    gives:
      "The narrator's voice itself — a small, gifted text-to-speech model whose seven English voices read the Public Domain Shelf aloud.",
    license: "Apache License 2.0",
    holder: "hexgrad",
  },
  {
    name: "espeak-ng",
    url: "https://github.com/espeak-ng/espeak-ng",
    gives:
      "The phoneme data that teaches the narrator how written words sound — Listen's pronunciation dictionary, bundled with the voice.",
    license: "GNU General Public License 3.0 (data and program)",
    holder: "The espeak-ng contributors",
  },
  {
    name: "rodio",
    url: "https://github.com/RustAudio/rodio",
    gives:
      "The playback queue Listen speaks through — its clock is the narration's clock, steady even when the window is hidden.",
    license: "MIT License OR Apache License 2.0",
    holder: "The RustAudio contributors",
  },
];

const BUILT_WITH: Credit[] = [
  {
    name: "Rust",
    url: "https://www.rust-lang.org",
    gives:
      "The language Fable is written in, top to bottom — the catalog, the job engine, the window, all of it.",
    license: "Apache License 2.0 OR MIT License",
    holder: "The Rust Project contributors",
  },
];

const DATA: Credit[] = [
  {
    name: "TMDB",
    url: "https://www.themoviedb.org",
    gives:
      "Film and TV details — posters, years, synopses — that enrichment hangs on your movies. This product uses the TMDB API but is not endorsed or certified by TMDB.",
    license: "TMDB API terms",
  },
  {
    name: "OpenSubtitles",
    url: "https://www.opensubtitles.com",
    gives: "The subtitle downloads that land beside your films, in your language.",
    license: "OpenSubtitles API terms",
  },
  {
    name: "MusicBrainz",
    url: "https://musicbrainz.org",
    gives:
      "The community's music encyclopedia — the recording and release facts behind music identification.",
    license: "Core data in the public domain (CC0)",
    holder: "The MetaBrainz Foundation and its contributors",
  },
  {
    name: "AcoustID",
    url: "https://acoustid.org",
    gives:
      "The fingerprint lookup that names an unknown track from its sound alone (fingerprints computed by Chromaprint, LGPL 2.1).",
    license: "AcoustID terms",
    holder: "Lukáš Lalinský",
  },
  {
    name: "Standard Ebooks",
    url: "https://standardebooks.org",
    gives:
      "The beautifully made editions on the Public Domain Shelf — volunteers who take public-domain texts and produce ebooks worthy of them, then give the work to everyone, forever.",
    license: "CC0 1.0 Universal (public domain dedication)",
    holder: "Standard Ebooks and its volunteers",
  },
  {
    name: "Project Gutenberg",
    url: "https://www.gutenberg.org",
    gives:
      "The original library of the internet — tens of thousands of public-domain books, freely given since 1971; a few of the Shelf's volumes come from its shelves.",
    license: "Public domain works; Project Gutenberg terms for its editions",
    holder: "Project Gutenberg Literary Archive Foundation",
  },
];

const ALONGSIDE: Credit[] = [
  {
    name: "ExifTool",
    url: "https://exiftool.org",
    gives:
      "The metadata engine Fable drives for reading, editing and washing the facts inside your photos and files. NOT bundled — Fable uses the copy on your system, and can offer to install it.",
    license: "GNU GPL OR Artistic License (Perl terms)",
    holder: "Phil Harvey",
  },
  {
    name: "FFmpeg",
    url: "https://ffmpeg.org",
    gives:
      "The media muscle Fable calls on for probing and conversion. NOT bundled — your system's copy does the work.",
    license: "GNU Lesser General Public License 2.1-or-later (some builds GPL)",
    holder: "The FFmpeg team",
  },
  {
    name: "RetroArch & the libretro cores",
    url: "https://www.retroarch.com",
    gives:
      "The emulators themselves are their own projects — Fable fetches them on request and never bundles them, and says thanks every launch it borrows.",
    license: "GNU GPL 3.0 (RetroArch); cores under their own licenses",
  },
];

export class CreditsDialog {
  private el: HTMLElement | null = null;

  open(): void {
    this.close();
    const overlay = document.createElement("div");
    overlay.className = "props-overlay";
    overlay.addEventListener("mousedown", (e) => {
      if (e.target === overlay) this.close();
    });
    const dialog = document.createElement("div");
    dialog.className = "about-dialog credits-dialog";
    dialog.setAttribute("role", "dialog");
    dialog.setAttribute("aria-label", "Credits");

    const title = document.createElement("h2");
    title.className = "about-title";
    title.textContent = "Credits";
    const intro = document.createElement("p");
    intro.className = "about-tagline";
    intro.textContent =
      "Fable Files stands on the work of people who gave theirs away. We're grateful — this app would not exist without them.";
    dialog.append(title, intro);

    const body = document.createElement("div");
    body.className = "credits-body";
    body.append(
      this.section("Contains code from", CONTAINS),
      this.section("Built with", BUILT_WITH),
      this.section("Data & services", DATA),
      this.section("Works alongside (not bundled)", ALONGSIDE),
      this.licenseSection(body),
    );
    dialog.appendChild(body);

    const note = document.createElement("p");
    note.className = "about-credit";
    note.textContent =
      "Fable's own icons are drawn in-house, and its type is set in your system's fonts. The full texts of the licenses named here ride along inside the app — one tap below, complete and unedited.";
    dialog.appendChild(note);

    const actions = document.createElement("div");
    actions.className = "about-actions";
    const closeBtn = document.createElement("button");
    closeBtn.className = "about-btn about-close";
    closeBtn.textContent = "Close";
    closeBtn.addEventListener("click", () => this.close());
    actions.appendChild(closeBtn);
    dialog.appendChild(actions);

    overlay.appendChild(dialog);
    document.body.appendChild(overlay);
    this.el = overlay;
    closeBtn.focus();
  }

  private section(heading: string, credits: Credit[]): HTMLElement {
    const wrap = document.createElement("div");
    wrap.className = "credits-section";
    const h = document.createElement("h3");
    h.className = "credits-head";
    h.textContent = heading;
    wrap.appendChild(h);
    for (const c of credits) {
      const row = document.createElement("div");
      row.className = "credits-row";
      const name = document.createElement("button");
      name.className = "credits-name";
      name.textContent = c.name;
      name.title = c.url;
      name.addEventListener("click", () => void api.openUrl(c.url));
      const gives = document.createElement("span");
      gives.className = "credits-gives";
      gives.textContent = ` — ${c.gives} `;
      const lic = document.createElement("span");
      lic.className = "credits-license";
      lic.textContent = `Licensed under ${c.license}.${c.holder ? ` © ${c.holder}.` : ""}`;
      row.append(name, gives, lic);
      wrap.appendChild(row);
    }
    return wrap;
  }

  /** The license texts: one tap from the thanks, complete and
   * unedited. Shown in place (a scrollable reader), with a way
   * back — never pasted over the credits list itself. */
  private licenseSection(body: HTMLElement): HTMLElement {
    const wrap = document.createElement("div");
    wrap.className = "credits-section";
    const h = document.createElement("h3");
    h.className = "credits-head";
    h.textContent = "Full license texts";
    wrap.appendChild(h);
    const row = document.createElement("div");
    row.className = "credits-licenses";
    for (const lt of LICENSE_TEXTS) {
      const b = document.createElement("button");
      b.className = "pill-btn credits-license-btn";
      b.textContent = lt.title;
      b.addEventListener("click", () => {
        const reader = document.createElement("div");
        reader.className = "credits-reader-wrap";
        const pre = document.createElement("pre");
        pre.className = "credits-reader";
        pre.textContent = lt.text;
        const back = document.createElement("button");
        back.className = "pill-btn";
        back.textContent = "← Back to the credits";
        back.addEventListener("click", () => reader.replaceWith(wrap));
        reader.append(pre, back);
        wrap.replaceWith(reader);
        body.scrollTop = 0;
      });
      row.appendChild(b);
    }
    wrap.appendChild(row);
    return wrap;
  }

  close(): void {
    if (this.el) {
      this.el.remove();
      this.el = null;
    }
  }
}
