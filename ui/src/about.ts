/**
 * AboutDialog: "About this program" — Fable Files doesn't ship with an
 * OS, so it carries its own about box, bug-report and feature-request
 * doors (they open the project's GitHub issue tracker).
 */
import { api } from "./api";
import { playWelcomeTour } from "./welcome";
import { CreditsDialog } from "./credits";

const ISSUES = "https://github.com/garrisonr/fable-files/issues/new";
const creditsDialog = new CreditsDialog();

export class AboutDialog {
  private el: HTMLElement | null = null;

  open(): void {
    this.close();
    const overlay = document.createElement("div");
    overlay.className = "props-overlay";
    overlay.addEventListener("mousedown", (e) => {
      if (e.target === overlay) this.close();
    });

    const dialog = document.createElement("div");
    dialog.className = "about-dialog";
    dialog.setAttribute("role", "dialog");
    dialog.setAttribute("aria-label", "About Fable Files");

    const bear = document.createElement("div");
    bear.className = "about-bear";
    bear.textContent = "🐻";

    const title = document.createElement("h2");
    title.className = "about-title";
    title.textContent = "Fable Files";

    const tagline = document.createElement("p");
    tagline.className = "about-tagline";
    tagline.textContent = "101 MB of pure bear-built file manager.";

    const version = document.createElement("p");
    version.className = "about-version";
    version.textContent = "Version 0.1.0 · Rust + Tauri · GPL-3.0-or-later";

    const credit = document.createElement("p");
    credit.className = "about-credit";
    credit.textContent = "Built by Richard Garrison with Grizz. Anyone bitching about the size can go use Nautilus.";

    const emuCredit = document.createElement("p");
    emuCredit.className = "about-credit";
    emuCredit.textContent = "Emulator picks and launch templates are mined from ES-DE's system definitions (ES-DE, MIT License). Emulators themselves — RetroArch and the libretro cores — are their own projects, fetched on request and never bundled.";

    const actions = document.createElement("div");
    actions.className = "about-actions";
    const bug = document.createElement("button");
    bug.className = "about-btn";
    bug.textContent = "Report a Bug";
    bug.addEventListener("click", () => {
      const body = encodeURIComponent("What happened:\n\nSteps to reproduce:\n\nWhat I expected:\n\nFable Files 0.1.0 on Omarchy (Wayland)");
      void api.openUrl(`${ISSUES}?title=${encodeURIComponent("[Bug] ")}&body=${body}&labels=bug`);
    });
    const feat = document.createElement("button");
    feat.className = "about-btn";
    feat.textContent = "Request a Feature";
    feat.addEventListener("click", () => {
      void api.openUrl(
        `${ISSUES}?title=${encodeURIComponent("[Feature] ")}&body=${encodeURIComponent("What I want:\n\nWhy it would help:\n")}&labels=enhancement`
      );
    });
    const tour = document.createElement("button");
    tour.className = "about-btn";
    tour.textContent = "Welcome Tour";
    tour.addEventListener("click", () => {
      this.close();
      playWelcomeTour();
    });
    const credits = document.createElement("button");
    credits.className = "about-btn";
    credits.textContent = "Credits";
    credits.title = "The people and projects Fable stands on";
    credits.addEventListener("click", () => {
      this.close();
      creditsDialog.open();
    });
    const closeBtn = document.createElement("button");
    closeBtn.className = "about-btn about-close";
    closeBtn.textContent = "Close";
    closeBtn.addEventListener("click", () => this.close());
    actions.append(bug, feat, tour, credits, closeBtn);

    dialog.append(bear, title, tagline, version, credit, emuCredit, actions);
    overlay.appendChild(dialog);
    document.body.appendChild(overlay);
    this.el = overlay;
    closeBtn.focus();
  }

  close(): void {
    if (this.el) {
      this.el.remove();
      this.el = null;
    }
  }
}
