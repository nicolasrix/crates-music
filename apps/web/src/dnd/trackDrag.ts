// Desktop-only drag-and-drop of a track onto a playlist. Deliberately
// scoped to fine (mouse) pointers: native HTML5 DnD doesn't fire from touch,
// and a phone has no visible sidebar drop target anyway. On touch the row
// menu ("add to playlist") stays the universal path, so drag is a pure
// progressive enhancement — sources set `draggable` only when pointer:fine.

import { useEffect, useState } from "react";

// Custom MIME type so a playlist drop zone reacts only to *our* track drags,
// not arbitrary file/text/URL drags the browser also delivers. The payload is
// the Subsonic track id (a playlist stores only ids; the server hydrates).
export const TRACK_DND_MIME = "application/x-crates-track-id";

/** Stamp the dragged track id onto a dragstart event's dataTransfer. */
export function setTrackDragData(dt: DataTransfer, trackId: string): void {
  dt.setData(TRACK_DND_MIME, trackId);
  // `move` reads oddly for "copy into playlist"; `copy` shows the right cursor.
  dt.effectAllowed = "copy";
}

/** `true` when a drag carries one of our tracks (checked in dragover, where
 *  the payload value is not yet readable — only its types are). */
export function isTrackDrag(dt: DataTransfer): boolean {
  return dt.types.includes(TRACK_DND_MIME);
}

/** Read the track id from a drop event's dataTransfer, or null if absent. */
export function getTrackDragData(dt: DataTransfer): string | null {
  const id = dt.getData(TRACK_DND_MIME);
  return id.length > 0 ? id : null;
}

/** Tracks whether the primary pointer is fine (mouse/trackpad). Drives the
 *  `draggable` attribute on track sources so touch devices never opt in. */
export function usePointerFine(): boolean {
  const [fine, setFine] = useState<boolean>(() =>
    typeof window !== "undefined" && "matchMedia" in window
      ? window.matchMedia("(pointer: fine)").matches
      : false,
  );
  useEffect(() => {
    if (typeof window === "undefined" || !("matchMedia" in window)) return;
    const mq = window.matchMedia("(pointer: fine)");
    const onChange = (e: MediaQueryListEvent) => setFine(e.matches);
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, []);
  return fine;
}
