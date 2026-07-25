// Per-device audio-output preference, persisted in localStorage.
//
// Sync state is device-agnostic: one shared { is_playing, now_playing_index,
// position_ms, queue } per room, and every device logged into the same
// account obeys is_playing *unconditionally* — so two devices in a room both
// emit audio in lockstep by default. This flag lets a device opt OUT of
// producing audio while still driving the shared queue: a silent "remote
// control" that sends play/pause/skip ops and shows state, but whose <audio>
// element never plays.
//
//   • true  (default) — "play here": this device emits audio.
//   • false           — "remote only": this device is silent; audio plays on
//                        the other device(s) signed into the same account.
//
// Deliberately a LOCAL preference, not synced: it answers "should THIS
// hardware make sound?", which only makes sense per-device. (Contrast with
// is_playing, which is shared room intent.)

const STORAGE_KEY = "crates-music.playback.outputEnabled";

export const DEFAULT_OUTPUT_ENABLED = true;

export function loadOutputEnabled(): boolean {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    if (v === null) return DEFAULT_OUTPUT_ENABLED;
    return v === "true";
  } catch {
    // localStorage may be unavailable (private mode); fall back to the default.
    return DEFAULT_OUTPUT_ENABLED;
  }
}

export function saveOutputEnabled(enabled: boolean): void {
  try {
    localStorage.setItem(STORAGE_KEY, enabled ? "true" : "false");
  } catch {
    /* localStorage may be unavailable (private mode); ignore */
  }
}
