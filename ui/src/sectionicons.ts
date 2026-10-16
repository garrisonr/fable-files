/** Sidebar section icons — the adaptive MONO set (Dickles' ruling,
 * 2026-10-16): single-color glyphs, fill/stroke = currentColor, the
 * amber accents of the two-tone design rendered as negative-space
 * cutouts, so ONE set reads correctly on every theme. Hand-authored
 * Door art for movies/documents/spreadsheets is icon set v2
 * (~/workspace/fable-icons/sections/v2/); the rest of the set's
 * masters live in ~/workspace/fable-icons/sections/mono/ (regenerate
 * there with make_mono.py); the inner SVG markup is inlined here so
 * the glyph takes the sidebar's own text color. Door → icon mapping
 * per sections/README.md. */
export const SECTION_ICON_SVG: Record<string, string> = {
  home: `<path fill="currentColor" fill-rule="evenodd" d="M24 5 L45 24 h-6 v15 a3 3 0 0 1 -3 3 H12 a3 3 0 0 1 -3 -3 V24 H3 Z M20.5 42 v-9.5 a3.5 3.5 0 0 1 7 0 V42 Z"/>`,
  documents: `<path fill=\"currentColor\" fill-rule=\"evenodd\" d=\"M14 3.5 h13.5 L36.5 12.5 V41 a3.5 3.5 0 0 1 -3.5 3.5 H14 a3.5 3.5 0 0 1 -3.5 -3.5 V7 A3.5 3.5 0 0 1 14 3.5 Z M27.5 3.5 l9 9 h-9 Z M17 19 h12 v4.4 h-12 Z M17 27.5 h12 v4.4 h-12 Z M17 36 h7.5 v4.4 h-7.5 Z\"/>`,
  downloads: `<path fill="currentColor" d="M21 3 h6 v13.2 l6.2 -6.2 L37 13.8 L24 26.8 L11 13.8 l3.8 -3.8 L21 16.2 Z"/> <path fill="currentColor" fill-rule="evenodd" d="M6 27 h7.2 l3 5.4 h15.6 l3 -5.4 H42 v10 a5 5 0 0 1 -5 5 H11 a5 5 0 0 1 -5 -5 Z M17 34.6 h14 v3 h-14 Z"/>`,
  videos: `<path fill="currentColor" fill-rule="evenodd" d="M9 9 h30 a5 5 0 0 1 5 5 v18 a5 5 0 0 1 -5 5 H9 a5 5 0 0 1 -5 -5 V14 a5 5 0 0 1 5 -5 Z M20.5 17 L32 23 L20.5 29 Z"/> <rect x="16" y="40.5" width="16" height="3.4" rx="1.7" fill="currentColor"/>`,
  pictures: `<path fill="currentColor" fill-rule="evenodd" d="M6 7 h36 a3 3 0 0 1 3 3 v28 a3 3 0 0 1 -3 3 H6 a3 3 0 0 1 -3 -3 V10 a3 3 0 0 1 3 -3 Z M11.5 14.5 v20 h25 v-20 Z"/> <path fill="currentColor" d="M11.5 34.5 L20 24 L26 30.5 L29.5 27 L36.5 34.5 Z"/> <circle cx="31" cy="19.5" r="3.1" fill="currentColor"/>`,
  music: `<path fill="currentColor" fill-rule="evenodd" d="M20.4 11.6 L40.6 8.2 v7.6 L20.4 19.2 Z M17.4 11.6 h3.4 v21 h-3.4 Z M37.4 8.2 h3.4 v21 h-3.4 Z M17.4 21.4 h23.4 v2.2 h-23.4 Z"/> <ellipse cx="14.8" cy="35" rx="5.6" ry="4.1" transform="rotate(-16 14.8 35)" fill="currentColor"/> <ellipse cx="34.8" cy="31.6" rx="5.6" ry="4.1" transform="rotate(-16 34.8 31.6)" fill="currentColor"/>`,
  trash: `<rect x="8" y="10.5" width="32" height="4.4" rx="2.2" fill="currentColor"/> <rect x="18.5" y="5.5" width="11" height="4.2" rx="2.1" fill="currentColor"/> <path fill="currentColor" fill-rule="evenodd" d="M11.5 17.5 h25 l-2.1 21.7 a4.5 4.5 0 0 1 -4.5 4.1 H18.1 a4.5 4.5 0 0 1 -4.5 -4.1 Z M18.6 23 h3.2 v12.5 h-3.2 Z M26.2 23 h3.2 v12.5 h-3.2 Z"/>`,
  starred: `<path fill="currentColor" stroke="currentColor" stroke-width="2.4" stroke-linejoin="round" d="M24 4.5 L30 18.2 L44.8 19.2 L33.6 28.7 L37.1 43.2 L24 35.4 L10.9 43.2 L14.4 28.7 L3.2 19.2 L18 18.2 Z"/>`,
  auditor: `<path fill="currentColor" fill-rule="evenodd" d="M11 4 h16 l8 8 v30 a3.5 3.5 0 0 1 -3.5 3.5 H11 A3.5 3.5 0 0 1 7.5 42 V7.5 A3.5 3.5 0 0 1 11 4 Z M13 19 h11 v2.8 h-11 Z M13 25.5 h11 v2.8 h-11 Z"/> <circle cx="31.5" cy="30" r="8.6" fill="none" stroke="currentColor" stroke-width="4.6"/> <line x1="37.8" y1="36.3" x2="44" y2="42.5" stroke="currentColor" stroke-width="5" stroke-linecap="round"/> <path d="M26.8 30.4 L30.2 33.8 L36.4 26.8" fill="none" stroke="currentColor" stroke-width="3.1" stroke-linecap="round" stroke-linejoin="round"/>`,
  audiobooks: `<path fill="none" stroke="currentColor" stroke-width="2.9" stroke-linecap="round" d="M35.5 4.6 a10.5 10.5 0 0 1 6 6 M33 8.2 a6 6 0 0 1 3.2 3.2"/> <path fill="currentColor" fill-rule="evenodd" d="M23 13.2 C18.2 9.8 11.5 8.9 5.8 10.6 v26.2 c5.7 -1.7 12.4 -0.8 17.2 2.6 c4.8 -3.4 11.5 -4.3 17.2 -2.6 V10.6 C34.5 8.9 27.8 9.8 23 13.2 Z M21.9 14.5 h2.2 v23.5 h-2.2 Z"/>`,
  movies: `<path fill=\"currentColor\" fill-rule=\"evenodd\" d=\"M11 3.5 h26 a3.5 3.5 0 0 1 3.5 3.5 v34 A3.5 3.5 0 0 1 37 44.5 H11 a3.5 3.5 0 0 1 -3.5 -3.5 V7 A3.5 3.5 0 0 1 11 3.5 Z M12.4 8 h3.4 v6 h-3.4 Z M12.4 21 h3.4 v6 h-3.4 Z M12.4 34 h3.4 v6 h-3.4 Z M32.2 8 h3.4 v6 h-3.4 Z M32.2 21 h3.4 v6 h-3.4 Z M32.2 34 h3.4 v6 h-3.4 Z M19.5 10.5 h9 v27 h-9 Z\"/> <path fill=\"currentColor\" d=\"M21.8 19.5 L28.6 24 L21.8 28.5 Z\"/> <path fill=\"currentColor\" d=\"M21.8 31 L28.6 35.5 L21.8 40 Z\" opacity=\"0.55\"/>`,
  spreadsheets: `<path fill=\"currentColor\" fill-rule=\"evenodd\" d=\"M13.5 3.5 h21 A3.5 3.5 0 0 1 38 7 V41 a3.5 3.5 0 0 1 -3.5 3.5 h-21 A3.5 3.5 0 0 1 10 41 V7 a3.5 3.5 0 0 1 3.5 -3.5 Z M10 10.5 h28 v3.4 h-28 Z M15.5 19 h7 v8 h-7 Z M26 19 h7 v8 h-7 Z M15.5 31.5 h7 v8 h-7 Z M26 31.5 h7 v8 h-7 Z\"/>`,
  roms: `<path fill="currentColor" fill-rule="evenodd" d="M15 13.5 h18 c6.8 0 11.8 5.2 11.8 12 c0 5.6 -3.4 9.5 -7.4 9.5 c-2.9 0 -5.3 -1.7 -7.7 -4.4 l-1.2 -1.4 H19.6 l-1.2 1.4 C16 33.3 13.6 35 10.7 35 C6.6 35 3.2 31.1 3.2 25.5 c0 -6.8 5 -12 11.8 -12 Z M10.6 20 h4.2 v9.6 h-4.2 Z M7.9 22.7 h9.6 v4.2 h-9.6 Z M29.900000000000002 20.6 a2.7 2.7 0 1 0 5.4 0 a2.7 2.7 0 1 0 -5.4 0 Z M35.3 26 a2.7 2.7 0 1 0 5.4 0 a2.7 2.7 0 1 0 -5.4 0 Z"/>`,
  books: `<path fill="currentColor" fill-rule="evenodd" d="M14 4 h20 a3.5 3.5 0 0 1 3.5 3.5 v33 A3.5 3.5 0 0 1 34 44 H14 a3.5 3.5 0 0 1 -3.5 -3.5 v-33 A3.5 3.5 0 0 1 14 4 Z M14.5 4 h5.5 v40 h-5.5 Z M25 13 h8 v2.8 h-8 Z M25 19.5 h8 v2.8 h-8 Z"/>`,
  folder: `<path fill="currentColor" fill-rule="evenodd" d="M4.5 12.5 A3.5 3.5 0 0 1 8 9 h8.6 l4.2 4.6 H40 a3.5 3.5 0 0 1 3.5 3.5 V37 a3.5 3.5 0 0 1 -3.5 3.5 H8 A3.5 3.5 0 0 1 4.5 37 Z M4.5 19.5 h39 v3.4 h-39 Z"/>`,
};


/** File-type glyphs — icon set v2 "The Division" (2026-10-16):
 * one unmistakable shape per document family, told apart at
 * 16px (documents the folded page, spreadsheets the chunky
 * 2x2 grid, CSV the dotted table). Same mono currentColor
 * masters as the doors (~/workspace/fable-icons/sections/v2/). */
export const FILE_TYPE_ICON_SVG: Record<string, string> = {
  "format-word": `<path fill=\"currentColor\" fill-rule=\"evenodd\" d=\"M14 3.5 h13.5 L36.5 12.5 V41 a3.5 3.5 0 0 1 -3.5 3.5 H14 a3.5 3.5 0 0 1 -3.5 -3.5 V7 A3.5 3.5 0 0 1 14 3.5 Z M27.5 3.5 l9 9 h-9 Z M17 19 h12 v4.4 h-12 Z M17 27.5 h12 v4.4 h-12 Z\"/> <path fill=\"currentColor\" d=\"M38.5 31.5 L44 40.5 L35.8 41.6 L34.6 34 Z M39.6 36.8 a1.9 1.9 0 1 0 0.1 0 Z\"/>`,
  "format-opendocument": `<path fill=\"currentColor\" fill-rule=\"evenodd\" d=\"M23 12.2 C18.2 8.8 11.5 7.9 5.8 9.6 v27.2 c5.7 -1.7 12.4 -0.8 17.2 2.6 c4.8 -3.4 11.5 -4.3 17.2 -2.6 V9.6 C34.5 7.9 27.8 8.8 23 12.2 Z M21.9 13.5 h2.2 v23.5 h-2.2 Z M10.5 16 h8 v3 h-8 Z M10.5 22.5 h8 v3 h-8 Z M27.5 16 h8 v3 h-8 Z M27.5 22.5 h8 v3 h-8 Z\"/>`,
  "format-text": `<path fill=\"currentColor\" fill-rule=\"evenodd\" d=\"M13.5 3.5 h21 A3.5 3.5 0 0 1 38 7 V41 a3.5 3.5 0 0 1 -3.5 3.5 h-21 A3.5 3.5 0 0 1 10 41 V7 a3.5 3.5 0 0 1 3.5 -3.5 Z M16 11.5 h16 v3.6 h-16 Z M16 18.5 h16 v3.6 h-16 Z M16 25.5 h16 v3.6 h-16 Z M16 32.5 h9 v3.6 h-9 Z\"/> <rect x=\"27\" y=\"31.7\" width=\"5\" height=\"5.2\" fill=\"currentColor\"/>`,
  "format-richtext": `<path fill=\"currentColor\" fill-rule=\"evenodd\" d=\"M14 3.5 h13.5 L36.5 12.5 V41 a3.5 3.5 0 0 1 -3.5 3.5 H14 a3.5 3.5 0 0 1 -3.5 -3.5 V7 A3.5 3.5 0 0 1 14 3.5 Z M27.5 3.5 l9 9 h-9 Z M17 18.5 h13 v5 h-13 Z M17 27 h13 v2.6 h-13 Z M17 33.5 h9 v1.6 h-9 Z\"/>`,
  "format-pdf": `<path fill=\"currentColor\" fill-rule=\"evenodd\" d=\"M14 3.5 h13.5 L36.5 12.5 V41 a3.5 3.5 0 0 1 -3.5 3.5 H14 a3.5 3.5 0 0 1 -3.5 -3.5 V7 A3.5 3.5 0 0 1 14 3.5 Z M27.5 3.5 l9 9 h-9 Z M17 18.5 h11 v4 h-11 Z M17 25.5 h7 v4 h-7 Z M21.0 31.5 a5.8 5.8 0 1 0 11.6 0 a5.8 5.8 0 1 0 -11.6 0 Z M24.400000000000002 31.5 a2.4 2.4 0 1 0 4.8 0 a2.4 2.4 0 1 0 -4.8 0 Z M22.7 35.8 L21.7 42 L25.3 39.7 L27.9 42.8 L29 36.2 L27 37.7 L25.1 35 Z\"/>`,
  "format-sheet": `<path fill=\"currentColor\" fill-rule=\"evenodd\" d=\"M13.5 3.5 h21 A3.5 3.5 0 0 1 38 7 V41 a3.5 3.5 0 0 1 -3.5 3.5 h-21 A3.5 3.5 0 0 1 10 41 V7 a3.5 3.5 0 0 1 3.5 -3.5 Z M10 10.5 h28 v3.4 h-28 Z M15.5 19 h7 v8 h-7 Z M26 19 h7 v8 h-7 Z M15.5 31.5 h7 v8 h-7 Z M26 31.5 h7 v8 h-7 Z\"/>`,
  "format-csv": `<path fill=\"currentColor\" fill-rule=\"evenodd\" d=\"M7 8.5 h34 a2.5 2.5 0 0 1 2.5 2.5 v26 A2.5 2.5 0 0 1 41 39.5 H7 A2.5 2.5 0 0 1 4.5 37 V11 A2.5 2.5 0 0 1 7 8.5 Z M9.5 14 v20 h29 V14 Z M9.5 20.5 h29 v2.4 h-29 Z M9.5 27 h29 v2.4 h-29 Z M19 14 h2.4 v20 h-2.4 Z M27.6 14 h2.4 v20 h-2.4 Z\"/> <g fill=\"currentColor\"><circle cx=\"14.2\" cy=\"17\" r=\"1.5\"/><circle cx=\"24\" cy=\"17\" r=\"1.5\"/><circle cx=\"33.8\" cy=\"17\" r=\"1.5\"/> <circle cx=\"14.2\" cy=\"24.7\" r=\"1.5\"/><circle cx=\"24\" cy=\"24.7\" r=\"1.5\"/><circle cx=\"33.8\" cy=\"24.7\" r=\"1.5\"/> <circle cx=\"14.2\" cy=\"31.2\" r=\"1.5\"/><circle cx=\"24\" cy=\"31.2\" r=\"1.5\"/><circle cx=\"33.8\" cy=\"31.2\" r=\"1.5\"/></g>`,
  "format-slides": `<path fill=\"currentColor\" fill-rule=\"evenodd\" d=\"M9 5.5 h30 a3 3 0 0 1 3 3 V29 a3 3 0 0 1 -3 3 H9 a3 3 0 0 1 -3 -3 V8.5 a3 3 0 0 1 3 -3 Z M12.5 24.5 L19 17.8 L24.5 21.8 L33.5 12.3 L33.9 17.4 L36.3 17.2 L35.4 9.4 L27.6 10.3 L29.8 12.2 L24.8 17.4 L19.2 13.4 L12.5 20.4 Z\"/> <path fill=\"currentColor\" d=\"M17.5 32 h4.2 L19.6 43.5 h-3.9 Z M26.3 32 h4.2 L32.4 43.5 h-3.9 Z\"/>`,
  "format-html": `<path fill=\"currentColor\" fill-rule=\"evenodd\" d=\"M8 7.5 h32 a3.5 3.5 0 0 1 3.5 3.5 v26 A3.5 3.5 0 0 1 40 40.5 H8 A3.5 3.5 0 0 1 4.5 37 V11 A3.5 3.5 0 0 1 8 7.5 Z M19.8 17.2 L11.6 24 L19.8 30.8 L19.8 25.6 L15.4 24 L19.8 22.4 Z M28.2 17.2 L36.4 24 L28.2 30.8 L28.2 25.6 L32.6 24 L28.2 22.4 Z\"/> <path fill=\"currentColor\" d=\"M26.6 15.5 h3.4 L23.6 32.5 h-3.4 Z\"/>`,
  "format-pages": `<path fill=\"currentColor\" fill-rule=\"evenodd\" d=\"M14 3.5 h13.5 L36.5 12.5 V41 a3.5 3.5 0 0 1 -3.5 3.5 H14 a3.5 3.5 0 0 1 -3.5 -3.5 V7 A3.5 3.5 0 0 1 14 3.5 Z M27.5 3.5 l9 9 h-9 Z M16.5 17.5 h15 v10.5 h-15 Z M16.5 32 h15 v3.2 h-15 Z M16.5 38 h10 v3.2 h-10 Z\"/> <path fill=\"currentColor\" d=\"M16.5 28 L22 21.5 L25.5 25 L27.8 22.8 L31.5 28 Z\"/> <circle cx=\"28.6\" cy=\"20.3\" r=\"1.7\" fill=\"currentColor\"/>`,
};

/** Extension (lowercase, no dot) -> file-type glyph name. */
export const FILE_TYPE_BY_EXT: Record<string, string> = {
  docx: "format-word",
  doc: "format-word",
  odt: "format-opendocument",
  txt: "format-text",
  rtf: "format-richtext",
  pdf: "format-pdf",
  xlsx: "format-sheet",
  xls: "format-sheet",
  csv: "format-csv",
  pptx: "format-slides",
  ppt: "format-slides",
  html: "format-html",
  pages: "format-pages",
};

/** A file-type glyph span for `name`, or null when the file's
 * extension has no v2 glyph (callers keep their fallback). */
export function fileTypeGlyph(filename: string): HTMLSpanElement | null {
  const ext = filename.split(".").pop()?.toLowerCase() ?? "";
  const glyphName = FILE_TYPE_BY_EXT[ext];
  const inner = glyphName ? FILE_TYPE_ICON_SVG[glyphName] : undefined;
  if (!inner) return null;
  const span = document.createElement("span");
  span.className = "file-type-glyph";
  span.innerHTML = `<svg viewBox="0 0 48 48" aria-hidden="true">${inner}</svg>`;
  return span;
}

/** Build a sidebar glyph span for icon `name` (falls back to folder). */
export function sectionGlyph(name: string): HTMLSpanElement {
  const span = document.createElement("span");
  span.className = "side-glyph";
  const inner = SECTION_ICON_SVG[name] ?? SECTION_ICON_SVG.folder;
  span.innerHTML = `<svg viewBox="0 0 48 48" aria-hidden="true">${inner}</svg>`;
  return span;
}

