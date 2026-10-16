/** Sidebar section icons — the adaptive MONO set (Dickles' ruling,
 * 2026-10-16): single-color glyphs, fill/stroke = currentColor, the
 * amber accents of the two-tone design rendered as negative-space
 * cutouts, so ONE set reads correctly on every theme. Hand-authored
 * masters live in ~/workspace/fable-icons/sections/mono/ (regenerate
 * there with make_mono.py); the inner SVG markup is inlined here so
 * the glyph takes the sidebar's own text color. Door → icon mapping
 * per sections/README.md. */
export const SECTION_ICON_SVG: Record<string, string> = {
  home: `<path fill="currentColor" fill-rule="evenodd" d="M24 5 L45 24 h-6 v15 a3 3 0 0 1 -3 3 H12 a3 3 0 0 1 -3 -3 V24 H3 Z M20.5 42 v-9.5 a3.5 3.5 0 0 1 7 0 V42 Z"/>`,
  documents: `<path fill="currentColor" fill-rule="evenodd" d="M14 4 h13 l9 9 v28 a3.5 3.5 0 0 1 -3.5 3.5 h-18.5 A3.5 3.5 0 0 1 10.5 41 V7.5 A3.5 3.5 0 0 1 14 4 Z M27 4 l9 9 h-9 Z M16.5 20 h13 v3 h-13 Z M16.5 27 h13 v3 h-13 Z M16.5 34 h8.5 v3 h-8.5 Z"/>`,
  downloads: `<path fill="currentColor" d="M21 3 h6 v13.2 l6.2 -6.2 L37 13.8 L24 26.8 L11 13.8 l3.8 -3.8 L21 16.2 Z"/> <path fill="currentColor" fill-rule="evenodd" d="M6 27 h7.2 l3 5.4 h15.6 l3 -5.4 H42 v10 a5 5 0 0 1 -5 5 H11 a5 5 0 0 1 -5 -5 Z M17 34.6 h14 v3 h-14 Z"/>`,
  videos: `<path fill="currentColor" fill-rule="evenodd" d="M9 9 h30 a5 5 0 0 1 5 5 v18 a5 5 0 0 1 -5 5 H9 a5 5 0 0 1 -5 -5 V14 a5 5 0 0 1 5 -5 Z M20.5 17 L32 23 L20.5 29 Z"/> <rect x="16" y="40.5" width="16" height="3.4" rx="1.7" fill="currentColor"/>`,
  pictures: `<path fill="currentColor" fill-rule="evenodd" d="M6 7 h36 a3 3 0 0 1 3 3 v28 a3 3 0 0 1 -3 3 H6 a3 3 0 0 1 -3 -3 V10 a3 3 0 0 1 3 -3 Z M11.5 14.5 v20 h25 v-20 Z"/> <path fill="currentColor" d="M11.5 34.5 L20 24 L26 30.5 L29.5 27 L36.5 34.5 Z"/> <circle cx="31" cy="19.5" r="3.1" fill="currentColor"/>`,
  music: `<path fill="currentColor" fill-rule="evenodd" d="M20.4 11.6 L40.6 8.2 v7.6 L20.4 19.2 Z M17.4 11.6 h3.4 v21 h-3.4 Z M37.4 8.2 h3.4 v21 h-3.4 Z M17.4 21.4 h23.4 v2.2 h-23.4 Z"/> <ellipse cx="14.8" cy="35" rx="5.6" ry="4.1" transform="rotate(-16 14.8 35)" fill="currentColor"/> <ellipse cx="34.8" cy="31.6" rx="5.6" ry="4.1" transform="rotate(-16 34.8 31.6)" fill="currentColor"/>`,
  trash: `<rect x="8" y="10.5" width="32" height="4.4" rx="2.2" fill="currentColor"/> <rect x="18.5" y="5.5" width="11" height="4.2" rx="2.1" fill="currentColor"/> <path fill="currentColor" fill-rule="evenodd" d="M11.5 17.5 h25 l-2.1 21.7 a4.5 4.5 0 0 1 -4.5 4.1 H18.1 a4.5 4.5 0 0 1 -4.5 -4.1 Z M18.6 23 h3.2 v12.5 h-3.2 Z M26.2 23 h3.2 v12.5 h-3.2 Z"/>`,
  starred: `<path fill="currentColor" stroke="currentColor" stroke-width="2.4" stroke-linejoin="round" d="M24 4.5 L30 18.2 L44.8 19.2 L33.6 28.7 L37.1 43.2 L24 35.4 L10.9 43.2 L14.4 28.7 L3.2 19.2 L18 18.2 Z"/>`,
  auditor: `<path fill="currentColor" fill-rule="evenodd" d="M11 4 h16 l8 8 v30 a3.5 3.5 0 0 1 -3.5 3.5 H11 A3.5 3.5 0 0 1 7.5 42 V7.5 A3.5 3.5 0 0 1 11 4 Z M13 19 h11 v2.8 h-11 Z M13 25.5 h11 v2.8 h-11 Z"/> <circle cx="31.5" cy="30" r="8.6" fill="none" stroke="currentColor" stroke-width="4.6"/> <line x1="37.8" y1="36.3" x2="44" y2="42.5" stroke="currentColor" stroke-width="5" stroke-linecap="round"/> <path d="M26.8 30.4 L30.2 33.8 L36.4 26.8" fill="none" stroke="currentColor" stroke-width="3.1" stroke-linecap="round" stroke-linejoin="round"/>`,
  audiobooks: `<path fill="none" stroke="currentColor" stroke-width="2.9" stroke-linecap="round" d="M35.5 4.6 a10.5 10.5 0 0 1 6 6 M33 8.2 a6 6 0 0 1 3.2 3.2"/> <path fill="currentColor" fill-rule="evenodd" d="M23 13.2 C18.2 9.8 11.5 8.9 5.8 10.6 v26.2 c5.7 -1.7 12.4 -0.8 17.2 2.6 c4.8 -3.4 11.5 -4.3 17.2 -2.6 V10.6 C34.5 8.9 27.8 9.8 23 13.2 Z M21.9 14.5 h2.2 v23.5 h-2.2 Z"/>`,
  movies: `<path fill="currentColor" fill-rule="evenodd" d="M11 4 h26 a3.5 3.5 0 0 1 3.5 3.5 v33 A3.5 3.5 0 0 1 37 44 H11 a3.5 3.5 0 0 1 -3.5 -3.5 v-33 A3.5 3.5 0 0 1 11 4 Z M12.6 8.6 h3 v4.6 h-3 Z M12.6 16.9 h3 v4.6 h-3 Z M12.6 25.2 h3 v4.6 h-3 Z M12.6 33.5 h3 v4.6 h-3 Z M32.4 8.6 h3 v4.6 h-3 Z M32.4 16.9 h3 v4.6 h-3 Z M32.4 25.2 h3 v4.6 h-3 Z M32.4 33.5 h3 v4.6 h-3 Z M19 9.5 h10 v9.5 H19 Z M19 25 h10 v11 H19 Z"/> <path fill="currentColor" d="M21.8 27.5 L29.5 32.2 L21.8 36.9 Z"/>`,
  spreadsheets: `<path fill="currentColor" fill-rule="evenodd" d="M13.5 4 h21 A3.5 3.5 0 0 1 38 7.5 v33 a3.5 3.5 0 0 1 -3.5 3.5 h-21 A3.5 3.5 0 0 1 10 40.5 v-33 A3.5 3.5 0 0 1 13.5 4 Z M10 11 h28 v2.6 h-28 Z M10 22.5 h28 v2 h-28 Z M10 30 h28 v2 h-28 Z M10 37.5 h28 v2 h-28 Z M18.4 13.6 h2 v30.4 h-2 Z M27.7 13.6 h2 v30.4 h-2 Z"/>`,
  roms: `<path fill="currentColor" fill-rule="evenodd" d="M15 13.5 h18 c6.8 0 11.8 5.2 11.8 12 c0 5.6 -3.4 9.5 -7.4 9.5 c-2.9 0 -5.3 -1.7 -7.7 -4.4 l-1.2 -1.4 H19.6 l-1.2 1.4 C16 33.3 13.6 35 10.7 35 C6.6 35 3.2 31.1 3.2 25.5 c0 -6.8 5 -12 11.8 -12 Z M10.6 20 h4.2 v9.6 h-4.2 Z M7.9 22.7 h9.6 v4.2 h-9.6 Z M29.900000000000002 20.6 a2.7 2.7 0 1 0 5.4 0 a2.7 2.7 0 1 0 -5.4 0 Z M35.3 26 a2.7 2.7 0 1 0 5.4 0 a2.7 2.7 0 1 0 -5.4 0 Z"/>`,
  books: `<path fill="currentColor" fill-rule="evenodd" d="M14 4 h20 a3.5 3.5 0 0 1 3.5 3.5 v33 A3.5 3.5 0 0 1 34 44 H14 a3.5 3.5 0 0 1 -3.5 -3.5 v-33 A3.5 3.5 0 0 1 14 4 Z M14.5 4 h5.5 v40 h-5.5 Z M25 13 h8 v2.8 h-8 Z M25 19.5 h8 v2.8 h-8 Z"/>`,
  folder: `<path fill="currentColor" fill-rule="evenodd" d="M4.5 12.5 A3.5 3.5 0 0 1 8 9 h8.6 l4.2 4.6 H40 a3.5 3.5 0 0 1 3.5 3.5 V37 a3.5 3.5 0 0 1 -3.5 3.5 H8 A3.5 3.5 0 0 1 4.5 37 Z M4.5 19.5 h39 v3.4 h-39 Z"/>`,
};

/** Build a sidebar glyph span for icon `name` (falls back to folder). */
export function sectionGlyph(name: string): HTMLSpanElement {
  const span = document.createElement("span");
  span.className = "side-glyph";
  const inner = SECTION_ICON_SVG[name] ?? SECTION_ICON_SVG.folder;
  span.innerHTML = `<svg viewBox="0 0 48 48" aria-hidden="true">${inner}</svg>`;
  return span;
}

