/**
 * Audio metadata helpers (inc-2 item 9): display titles/artists come
 * from the file's embedded tags, never the filename — a file called
 * "01 Track 1.mp3" headlines as its Title tag ("40 - Lola"), with
 * Artist / Album beneath. Tags are read through the existing
 * exiftool path (api.readMetadata) and cached per item so browsing
 * a folder of MP3s doesn't re-probe every file on every render.
 * Folder art lookup (folder.jpg / cover.jpg / …) is shared with the
 * Music Player so Quick Look and the player resolve art identically
 * — Quick Look previously skipped folder art entirely and showed
 * the bare glyph for folders that thumbnail fine in the grid.
 */
import { api, type Item } from "./api";

export interface AudioTags {
  title?: string;
  artist?: string;
  album?: string;
  /** Release year (first 4 digits of the Year/Date tag). */
  year?: string;
  genre?: string;
}

/** exiftool group priority when several tag families carry the same
 * field (ID3v2.4 wins over ID3v1, Vorbis over APE, …). */
const GROUP_PRIORITY = [
  "ID3v2_4",
  "ID3v2_3",
  "ID3v2_2",
  "Vorbis",
  "FLAC",
  "QuickTime",
  "APE",
  "ASF",
  "ID3v1",
];

const tagCache = new Map<string, Promise<AudioTags>>();

export function audioTags(item: Item): Promise<AudioTags> {
  const hit = tagCache.get(item.id);
  if (hit) return hit;
  const p = (async (): Promise<AudioTags> => {
    try {
      const tags = await api.readMetadata(item.id);
      const pick = (names: string[]): string | undefined => {
        let best: { rank: number; value: string } | null = null;
        for (const t of tags) {
          if (!names.includes(t.tag)) continue;
          const v = (t.value ?? "").trim();
          if (!v) continue;
          const rank = GROUP_PRIORITY.indexOf(t.group);
          const r = rank === -1 ? GROUP_PRIORITY.length : rank;
          if (!best || r < best.rank) best = { rank: r, value: v };
        }
        return best?.value;
      };
      const yearRaw = pick(["Year", "Date", "OriginalYear", "ReleaseDate"]);
      return {
        title: pick(["Title"]),
        artist: pick(["Artist"]),
        album: pick(["Album"]),
        year: yearRaw ? yearRaw.slice(0, 4) : undefined,
        genre: pick(["Genre"]),
      };
    } catch {
      return {};
    }
  })();
  tagCache.set(item.id, p);
  return p;
}

/** Art filenames, in the order the Music Player has always used
 * (plus AlbumArtSmall.jpg, which Windows libraries carry). */
export const FOLDER_ART_NAMES = new Set([
  "folder.jpg",
  "folder.jpeg",
  "folder.png",
  "cover.jpg",
  "cover.jpeg",
  "cover.png",
  "front.jpg",
  "front.jpeg",
  "front.png",
  "albumart.jpg",
  "albumartsmall.jpg",
]);

/** List a folder given its absolute path. `list_dir` resolves
 * directory IDS, not paths — passing the raw path silently fails.
 * Start at the item's location root and walk the path segments,
 * matching child folders by name (the Round 7.2 resolution, shared
 * now instead of living as a private copy inside the Music Player). */
export async function listFolderByPath(
  first: Item,
  folderPath: string,
): Promise<Item[] | null> {
  const locations = await api.listLocations();
  const loc =
    locations.find((l) => l.id === first.location) ?? locations[0];
  if (!loc) return null;
  const rootPath = await api.getItemPath(loc.root_item);
  if (folderPath === rootPath) {
    return api.listDir(loc.id, loc.root_item);
  }
  if (!folderPath.startsWith(rootPath + "/")) return null;
  const segments = folderPath
    .slice(rootPath.length + 1)
    .split("/")
    .filter((s) => s.length > 0);
  let curDirId = loc.root_item;
  for (const seg of segments) {
    const entries = await api.listDir(loc.id, curDirId);
    const next = entries.find((e) => e.kind === "folder" && e.name === seg);
    if (!next) return null;
    curDirId = next.id;
  }
  return api.listDir(loc.id, curDirId);
}

/** Technical credentials for one audio file (Music Glow-Up
 * Phase 2): the hi-res truth behind the honest badge — sample
 * rate, bit depth, bitrate — read through the same exiftool path
 * as the tags and cached per item. */
export interface AudioProps {
  sampleRate?: number;
  bitsPerSample?: number;
  /** kbps, for lossy formats (the badge's "MP3 320"). */
  bitrateKbps?: number;
  durationSecs?: number;
}

const propsCache = new Map<string, Promise<AudioProps>>();

function parseNumberish(v: string | undefined): number | undefined {
  if (!v) return undefined;
  const m = v.replace(/,/g, "").match(/-?\d+(?:\.\d+)?/);
  if (!m) return undefined;
  const n = parseFloat(m[0]);
  return Number.isFinite(n) ? n : undefined;
}

/** exiftool prints Duration as seconds ("217.06"), "3:37", or
 * "0:03:37" depending on the format family — accept all three. */
function parseDuration(v: string | undefined): number | undefined {
  if (!v) return undefined;
  const t = v.trim();
  if (/^\d+(?:\.\d+)?$/.test(t)) return parseFloat(t);
  const parts = t.split(":").map((p) => parseFloat(p));
  if (parts.length >= 2 && parts.every((n) => Number.isFinite(n))) {
    return parts.reduce((acc, n) => acc * 60 + n, 0);
  }
  return undefined;
}

export function audioProps(item: Item): Promise<AudioProps> {
  const hit = propsCache.get(item.id);
  if (hit) return hit;
  const p = (async (): Promise<AudioProps> => {
    try {
      const tags = await api.readMetadata(item.id);
      const pick = (names: string[]): string | undefined => {
        let best: { rank: number; value: string } | null = null;
        for (const t of tags) {
          if (!names.includes(t.tag)) continue;
          const v = (t.value ?? "").trim();
          if (!v) continue;
          const rank = GROUP_PRIORITY.indexOf(t.group);
          const r = rank === -1 ? GROUP_PRIORITY.length : rank;
          if (!best || r < best.rank) best = { rank: r, value: v };
        }
        return best?.value;
      };
      const brRaw = parseNumberish(
        pick(["AudioBitrate", "Bitrate", "NominalBitrate"]),
      );
      return {
        sampleRate: parseNumberish(pick(["SampleRate", "SamplingRate"])),
        bitsPerSample: parseNumberish(
          pick(["BitsPerSample", "BitDepth", "BitsPerSampleId3"]),
        ),
        // exiftool reports bitrate in bps for some families and
        // kbps for others; normalize anything implausibly large.
        bitrateKbps: brRaw
          ? brRaw > 10000
            ? Math.round(brRaw / 1000)
            : Math.round(brRaw)
          : undefined,
        durationSecs: parseDuration(pick(["Duration"])),
      };
    } catch {
      return {};
    }
  })();
  propsCache.set(item.id, p);
  return p;
}

/** The honest badge: never a bare "FLAC" when the credentials
 * are known — "FLAC 24/192", "DSD128", "MP3 320". Falls back to
 * the plain format (or "DSD") while props are still loading. */
export function audioBadge(name: string, props?: AudioProps): string {
  const dot = name.lastIndexOf(".");
  const ext = dot < 0 ? "" : name.slice(dot + 1).toLowerCase();
  if (ext === "dsf" || ext === "dff") {
    // DSD rates are multiples of 44.1 kHz: 2,822,400 = DSD64,
    // 5,644,800 = DSD128, 11,289,600 = DSD256.
    const n = props?.sampleRate ? Math.round(props.sampleRate / 44100) : 0;
    return n >= 64 ? `DSD${n}` : "DSD";
  }
  const base = ext.toUpperCase();
  if (!props) return base;
  if (props.bitsPerSample && props.sampleRate) {
    const khz = props.sampleRate / 1000;
    const khzText = Number.isInteger(khz) ? String(khz) : khz.toFixed(1);
    return `${base} ${props.bitsPerSample}/${khzText}`;
  }
  if (props.bitrateKbps && !props.bitsPerSample) {
    return `${base} ${props.bitrateKbps}`;
  }
  return base;
}

const artCache = new Map<string, Promise<string | null>>();

/** URL of the folder art for the folder containing `item`, or null
 * when the folder carries none. Cached per (location, folder).
 *
 * The art comes back through the thumbnailer as a data URL — NOT via
 * the loopback media server: the app CSP allows media-src from
 * 127.0.0.1 but NOT img-src, so an http://127.0.0.1 <img> was blocked
 * and folder art silently never rendered in Quick Look or the Music
 * Player (fix block item 5, 2026-10-14). Data URLs are CSP-clean and
 * the thumbnail cache makes repeats free. */
export function folderArtUrl(
  item: Item,
  folderPath: string,
): Promise<string | null> {
  const key = `${item.location ?? ""}:${folderPath}`;
  const hit = artCache.get(key);
  if (hit) return hit;
  const p = (async (): Promise<string | null> => {
    try {
      if (!item.location) return null;
      const entries = await listFolderByPath(item, folderPath);
      const art = entries?.find((e) =>
        FOLDER_ART_NAMES.has(e.name.toLowerCase()),
      );
      if (art?.provider_key) {
        return await api.getThumbnail(art.id, art.provider_key, true);
      }
      return null;
    } catch {
      return null;
    }
  })();
  artCache.set(key, p);
  return p;
}
