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
import { api, mediaSrc, type Item } from "./api";

export interface AudioTags {
  title?: string;
  artist?: string;
  album?: string;
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
      return {
        title: pick(["Title"]),
        artist: pick(["Artist"]),
        album: pick(["Album"]),
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

const artCache = new Map<string, Promise<string | null>>();

/** URL of the folder art for the folder containing `item`, or null
 * when the folder carries none. Cached per (location, folder). */
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
      if (art?.provider_key) return await mediaSrc(art.provider_key);
      return null;
    } catch {
      return null;
    }
  })();
  artCache.set(key, p);
  return p;
}
