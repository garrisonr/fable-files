/**
 * Metadata Accounts — every outside account and API key Fable
 * holds, in one place (Preferences cleanup, Dickles 2026-10-16:
 * "I have account information stored for emumovies and
 * screenscraper but it isn't clear that fable files already has
 * them… clean it up!").
 *
 * The page opens with the truth at a glance: one row per provider
 * with an honest state. "Saved" means exactly that — bytes are in
 * the system keyring; it does NOT claim the provider accepts them.
 * "Connected" is earned: only a Test that passed this session says
 * it, with the provider's own plain-language verdict beside it.
 * (EmuMovies is why this matters: its stored password is refused
 * by the server while a presence-only UI would chirp "signed in".)
 *
 * Below the strip, the providers' own blocks — the same controls
 * and commands that used to be scattered through Game Media, Media
 * Resolution, Media Completion and Music Completion — grouped by
 * the library they serve. Secrets are never rendered; the keyring
 * is the only place they exist.
 */
import { api } from "./api";
import { errText } from "./toast";
import { GameAccountsSection } from "./rommedia";
import { TmdbAccountSection } from "./mediares";
import { OpenSubtitlesAccountSection } from "./mediacomplete";
import { AcoustidAccountSection } from "./musiccomplete";

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  e.className = className;
  if (text !== undefined) e.textContent = text;
  return e;
}

type RowState =
  | { kind: "connected"; text: string }
  | { kind: "saved"; text: string }
  | { kind: "failed"; text: string }
  | { kind: "none"; text: string }
  | { kind: "unknown"; text: string };

interface ProviderRow {
  name: string;
  state: RowState;
  /** Runs the provider's existing validate command. Absent = the
   * provider offers no check (AcoustID only answers mid-scan). */
  test?: () => Promise<{ ok: boolean; message: string }>;
}

export class MetadataAccountsPage {
  async mount(host: HTMLElement): Promise<void> {
    host.appendChild(
      el(
        "p",
        "prefs-note",
        "Every account and key lives in your system keyring — never in a settings file, a job, or a log. Saved means Fable is holding it; Connected means the provider itself just said yes.",
      ),
    );
    const strip = el("div", "acct-strip");
    strip.appendChild(el("p", "prefs-note", "Asking the keyring…"));
    host.appendChild(strip);
    void this.renderStrip(strip);

    host.appendChild(el("h3", "prefs-section", "Games"));
    const games = el("div", "");
    host.appendChild(games);
    void new GameAccountsSection().mount(games);

    host.appendChild(el("h3", "prefs-section", "Movies & TV"));
    const movies = el("div", "");
    host.appendChild(movies);
    void new TmdbAccountSection().mount(movies);
    void new OpenSubtitlesAccountSection().mount(movies);

    host.appendChild(el("h3", "prefs-section", "Music"));
    const music = el("div", "");
    host.appendChild(music);
    void new AcoustidAccountSection().mount(music);
  }

  private async renderStrip(strip: HTMLElement): Promise<void> {
    let rows: ProviderRow[];
    try {
      rows = await this.collectRows();
    } catch (err) {
      strip.innerHTML = "";
      strip.appendChild(
        el("p", "prefs-note", `Account state could not be read: ${errText(err)}`),
      );
      return;
    }
    strip.innerHTML = "";
    for (const row of rows) strip.appendChild(this.rowEl(row));
  }

  private async collectRows(): Promise<ProviderRow[]> {
    const rows: ProviderRow[] = [];
    const saved = (text: string): RowState => ({ kind: "saved", text });
    const none: RowState = { kind: "none", text: "Not connected" };

    // Games (rommedia): presence + display usernames from one view.
    try {
      const view = await api.getRomMediaSettings();
      const s = view.settings;
      const has = (k: string) => view.secretPresent[k] ?? false;
      const acct = (k: string, user: string | null): RowState =>
        has(k)
          ? saved(user ? `Saved — signed in as ${user} (not checked yet)` : "Saved (not checked yet)")
          : none;
      rows.push({
        name: "ScreenScraper",
        state: !has("screenscraper")
          ? none
          : view.ssDevReady
            ? acct("screenscraper", s.screenscraperUser)
            : saved(
                `Saved — signed in as ${s.screenscraperUser ?? "?"}, but this source also needs developer credentials (below) before it can switch on`,
              ),
        test: has("screenscraper")
          ? () => api.validateRomMediaSource("screenscraper")
          : undefined,
      });
      rows.push({
        name: "EmuMovies",
        state: acct("emumovies", s.emumoviesUser),
        test: has("emumovies")
          ? () => api.validateRomMediaSource("emumovies")
          : undefined,
      });
      rows.push({
        name: "TheGamesDB",
        state: has("thegamesdb") ? saved("Key saved (not checked yet)") : none,
        test: has("thegamesdb")
          ? () => api.validateRomMediaSource("thegamesdb")
          : undefined,
      });
      rows.push({
        name: "SteamGridDB",
        state: has("steamgriddb") ? saved("Key saved (not checked yet)") : none,
        test: has("steamgriddb")
          ? () => api.validateRomMediaSource("steamgriddb")
          : undefined,
      });
    } catch {
      rows.push({ name: "Game accounts", state: { kind: "unknown", text: "Could not be read" } });
    }

    // Movies & TV.
    try {
      const view = await api.getMediaresSettings();
      rows.push({
        name: "TMDB",
        state: view.hasKey ? saved("Key saved (not checked yet)") : none,
        test: view.hasKey ? () => api.validateMediaresKey(null) : undefined,
      });
    } catch {
      rows.push({ name: "TMDB", state: { kind: "unknown", text: "Could not be read" } });
    }
    try {
      const st = await api.getMcStatus();
      const c = st.opensubs;
      rows.push({
        name: "OpenSubtitles",
        state: !c.keyringOk
          ? { kind: "unknown", text: "System keyring unavailable" }
          : c.hasApiKey
            ? saved(
                c.hasLogin
                  ? "Saved — API key + account login (not checked yet)"
                  : "Saved — API key only, no account login (not checked yet)",
              )
            : none,
        test: c.hasApiKey ? () => api.opensubsValidate() : undefined,
      });
    } catch {
      rows.push({ name: "OpenSubtitles", state: { kind: "unknown", text: "Could not be read" } });
    }

    // Music.
    try {
      const st = await api.getMuStatus();
      rows.push({
        name: "AcoustID",
        state: st.acoustidKeySaved
          ? saved("Key saved — AcoustID only answers during a scan, so there is no separate check")
          : none,
      });
    } catch {
      rows.push({ name: "AcoustID", state: { kind: "unknown", text: "Could not be read" } });
    }
    return rows;
  }

  private rowEl(row: ProviderRow): HTMLElement {
    const wrap = el("div", "acct-row");
    const dot = el("span", "acct-dot");
    const name = el("span", "acct-name", row.name);
    const state = el("span", "acct-state", row.state.text);
    wrap.append(dot, name, state);
    const paint = (s: RowState) => {
      dot.dataset.state = s.kind;
      state.textContent = s.text;
    };
    paint(row.state);
    if (row.test) {
      const test = el("button", "pill-btn acct-test", "Test");
      const verdict = el("span", "acct-verdict");
      verdict.hidden = true;
      test.addEventListener("click", () => {
        void (async () => {
          test.textContent = "Checking…";
          (test as HTMLButtonElement).disabled = true;
          try {
            const r = await row.test!();
            verdict.hidden = false;
            verdict.textContent = r.message;
            paint(
              r.ok
                ? { kind: "connected", text: "Connected — the provider just confirmed it" }
                : { kind: "failed", text: "Saved, but the check failed" },
            );
          } catch (err) {
            verdict.hidden = false;
            verdict.textContent = `The check itself failed: ${errText(err)}`;
            paint({ kind: "failed", text: "Saved, but the check failed" });
          } finally {
            test.textContent = "Test";
            (test as HTMLButtonElement).disabled = false;
          }
        })();
      });
      wrap.append(test, verdict);
    }
    return wrap;
  }
}
