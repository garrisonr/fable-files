/**
 * Quick Look attachment rules — Dickles' rulings, 2026-10-15
 * 16:24 + 16:54. Kept DOM-free and side-effect-free so the rules
 * are unit-testable without a browser; QuickLook (quicklook.ts)
 * derives a snapshot from its own authoritative fields on every
 * event, steps this machine, and executes the effect.
 *
 * The three rules:
 *  1. ATTACHED FOLLOWS — while the preview pane is attached, a
 *     selection change moves the preview to the newly selected
 *     item (mouse, keyboard, every surface — the shell routes
 *     them all here).
 *  2. DETACHED STAYS PINNED — while the open item's media plays
 *     in a pop-out window, selection changes NEVER move the pane;
 *     the latest selection is remembered as pending instead.
 *  3. RETURN ADOPTS — when the pop-out goes away (its Return
 *     control, the placeholder's "Bring it back here", or a plain
 *     window close — every door is a reattach), the pane adopts
 *     the pending selection if there is one; otherwise the pinned
 *     item's card re-renders inline and playback comes home.
 *
 * The pin is explicit state (the caller passes the path of the
 * item whose pop-out is outstanding — QuickLook's detachedFor /
 * detachedAudioFor flags), never inferred from window identity.
 */

export interface AttachSnapshot {
  /** Item the pane is showing, or null when the pane is closed. */
  openId: string | null;
  /** Path pinned by an outstanding pop-out window, or null. */
  pinnedPath: string | null;
  /** Selection remembered while pinned, or null. */
  pendingId: string | null;
}

export type AttachEvent =
  | { type: "selection"; id: string | null }
  | {
      type: "detach-end";
      /** The pop-out that went away. */
      path: string;
      /** provider_key of the item the pane is showing now. */
      openPath: string | null;
    };

export type AttachEffect =
  /** Attached + selection moved: open the pane on this item. */
  | { type: "follow"; id: string }
  /** Pop-out ended with a pending selection: open the pane on it. */
  | { type: "adopt"; id: string }
  /** Pop-out ended, no pending selection: re-render the open
   *  item's card inline (playback comes home). */
  | { type: "render-home" }
  | { type: "none" };

export interface AttachStep {
  /** The pending selection after this event. */
  pendingId: string | null;
  effect: AttachEffect;
}

export function attachStep(
  s: AttachSnapshot,
  e: AttachEvent,
): AttachStep {
  if (e.type === "selection") {
    // A closed pane follows nothing and remembers nothing.
    if (s.openId === null) return { pendingId: null, effect: { type: "none" } };
    // Selection cleared: there is nowhere for a later return to
    // adopt — forget any pending selection, keep the card.
    if (e.id === null) return { pendingId: null, effect: { type: "none" } };
    // The selection came back to the item already on screen.
    if (e.id === s.openId) return { pendingId: null, effect: { type: "none" } };
    // Pinned: remember where the selection went; the card — and
    // the pop-out — stay exactly as they are (rule 2).
    if (s.pinnedPath !== null) {
      return { pendingId: e.id, effect: { type: "none" } };
    }
    // Attached: follow (rule 1).
    return { pendingId: null, effect: { type: "follow", id: e.id } };
  }
  // detach-end: only the pop-out that holds the pin can reattach
  // the pane. (QuickLook snapshots BEFORE clearing its detach
  // flags, so pinnedPath is the pre-close pin.)
  if (s.pinnedPath === null || s.pinnedPath !== e.path) {
    return { pendingId: s.pendingId, effect: { type: "none" } };
  }
  if (s.openId !== null && s.pendingId !== null && s.pendingId !== s.openId) {
    // Rule 3: adopt the selection the user moved to while the
    // media was away. The detached item's resume state stays
    // cached by the caller, so re-selecting it later still
    // lands where the pop-out left off.
    return {
      pendingId: null,
      effect: { type: "adopt", id: s.pendingId },
    };
  }
  // No pending selection: playback comes home — but only when the
  // card on screen IS the item whose pop-out closed.
  return {
    pendingId: null,
    effect:
      e.openPath !== null && e.openPath === e.path
        ? { type: "render-home" }
        : { type: "none" },
  };
}
