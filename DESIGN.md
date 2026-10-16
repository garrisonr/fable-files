# Fable Files — Design Language

**Status:** LAW. This document is non-negotiable for the project.
**Source:** Apple's design language per the `apple-design` skill
(`~/workspace/skills/apple-design/SKILL.md`), distilled from WWDC's
*Designing Fluid Interfaces* and related talks, translated for a Tauri
(web-tech) frontend.

The through-line: **the interface must stop feeling like a computer and
start feeling like an extension of the hand.** Every rule below serves
that. When in doubt, prototype the interaction and play with it — an
interactive demo is worth a million static mockups.

## 1. Motion is physics, not decoration

- **Springs, not fixed-duration animations,** for anything the user can
  touch. New input re-targets the spring; motion stays continuous.
- **Default:** critically damped (`damping 1.0`, `response 0.3–0.4`) —
  graceful, no overshoot. Panels, sheets, sidebar collapses, view
  transitions all use this.
- **Bounce (`damping ~0.8`) only when the gesture carried momentum** —
  flicking through the gallery view, throwing a sheet away. Overshoot on
  a menu that just appeared is wrong; overshoot on a card you flicked is
  right.
- **Interruptibility is the single most important principle.** Every
  animation must be grabbable and reversible mid-flight. A closing
  preview pane the user re-opens follows the finger — it never finishes
  closing first. Never lock out input during a transition. Always animate
  from the live on-screen value, never the logical target.
- **Velocity handoff:** when a drag ends, the animation continues at the
  finger's exact release velocity — no visible seam between dragging and
  animating. Project the landing point from momentum (like scroll
  deceleration), then settle to the nearest target.
- **Rubber-band, don't hard-stop,** at scroll/list boundaries.
  Progressive resistance reads as responsive; a hard stop reads as frozen.

## 2. Direct manipulation

- File drag-and-drop tracks **1:1 with the pointer**, respecting the grab
  offset (no snapping to icon center on grab).
- Feedback on **pointer-down, not release.** Press states are instant.
  Continuous feedback *during* drags and resizes, never only at the end.
- Tap targets get ~10px hit padding; cancel-by-dragging-away works.

## 3. Materials & depth

- Sidebar, toolbar, and sheets are **translucent layers**
  (`backdrop-filter: blur()` + semi-transparent background) with content
  scrolling underneath — never opaque bars consuming fixed strips.
- Heavier materials for structural regions (sidebar), lighter for
  interactive elements. **Never stack translucent on translucent.**
- Bigger surfaces read thicker: stronger blur + deeper shadow.
- Text over blurred surfaces uses higher contrast and slightly heavier
  weight (vibrancy), never flat gray.
- Scroll edge effects (blur/gradient masks), not hard 1px dividers,
  where floating chrome meets content.
- Modals pair with a dimming scrim and push the background back;
  non-blocking panels use translucency and offset with no scrim.

## 4. Spatial consistency

- Enter and exit along the **same path**. A preview that slides in from
  the right dismisses to the right.
- Sheets, popovers, and the preview pane **anchor to their trigger**
  (transform-origin at the source element).
- Reversible transitions mirror their easing in both directions.

## 5. Typography

- **System font stack first** — it ships optical sizing and legibility
  tuning. Override only with reason.
- **Tracking is size-specific:** tighten large display text
  (e.g. `-0.02em`), keep body near `0`. Never one fixed letter-spacing.
- Leading tight on headings, comfortable on body. Hierarchy from
  weight + size + leading as a set, not size alone.
- Respect Dynamic Type: spacing in relative units so larger text never
  breaks layout.

## 6. Accessibility (non-optional)

- `prefers-reduced-motion`: springs become short opacity cross-fades.
- `prefers-reduced-transparency`: frostier/solid surfaces, blur off.
- `prefers-contrast: more`: near-solid backgrounds, defined borders.
- No full-viewport moving backgrounds, no slow oscillations, no abrupt
  brightness jumps on theme change.

## 7. The eight principles (review checklist)

Every feature and interaction is reviewed against: **Purpose, Agency,
Responsibility, Familiarity, Flexibility, Simplicity (not minimalism),
Craft, Delight.** In particular:

- **Agency:** undo for slips; confirmation dialogs only for genuinely
  destructive, irreversible actions (overusing them trains click-through).
- **Familiarity:** things that look the same behave the same and live in
  the same place. Break a familiar pattern only if provably better.
- **Simplicity:** strip the unnecessary; show the common path first,
  advanced options one level deeper. Every element earns its place.
- **Craft:** every spacing, timing, and alignment value is deliberate
  and defensible. Jittery scroll and misaligned icons read as
  carelessness.
- **Delight** is the result of getting the other seven right.

## 8. File-manager-specific applications

- **QuickLook preview:** spring-anchored to the selected file, dismisses
  along its entry path, interruptible mid-animation.
- **QuickLook attachment (Dickles, 2026-10-15):** an attached preview
  FOLLOWS the selection — click or arrow to another file, in any view
  including Smart Folder results, and the pane shows that file. Media
  popped out to a detach window stays PINNED to its item no matter
  where the selection wanders. Detaching is never a one-way door:
  every pop-out carries a labeled, always-visible **⤺ Return**
  control, and reattaching (Return, the placeholder's "Bring it back
  here", or a plain window close) adopts the current selection if it
  moved while the media was away. The decision logic lives in
  `ui/src/qlattach.ts` as a pure, unit-tested state machine.
- **Drag operations:** 1:1 tracking, velocity-aware drop, rubber-band
  resistance at invalid targets rather than silent refusal.
- **View transitions** (icons ↔ list ↔ columns ↔ gallery): critically
  damped springs; gallery flick carries momentum with slight bounce.
- **Command palette / launcher:** anchored sheet from the top, material
  blur, instant pointer-down feedback on results.
- **Copy/move jobs:** continuous progress with physical feel; pause and
  resume never jump — they ease from the live value.
- **Multimodal feedback:** subtle sound/haptic only on meaningful
  commits (drop complete, job finished, error) — same-frame as the
  visual, never chatty.
