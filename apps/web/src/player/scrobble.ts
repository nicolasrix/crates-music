// Pure scrobble decision function. Tested in scrobble.test.ts.
//
// The Subsonic / Last.fm convention this implements:
//   - tracks under MIN_DURATION_MS never scrobble
//   - submission threshold = min(50% of duration, SUBMISSION_CAP_MS)
//   - now_playing fires once at the start of a sufficiently long track
// All time fields are milliseconds.

const MIN_DURATION_MS = 30_000;
const SUBMISSION_CAP_MS = 240_000; // 4 minutes

export type ScrobbleDecision = "none" | "now_playing" | "submission";

export interface ScrobbleState {
  trackDurationMs: number;
  elapsedMs: number;
  hasEmittedNowPlaying: boolean;
  hasEmittedSubmission: boolean;
}

export function evaluateScrobble(state: ScrobbleState): ScrobbleDecision {
  const { trackDurationMs, elapsedMs, hasEmittedNowPlaying, hasEmittedSubmission } = state;

  // Preconditions: not enough info, or values that signal a transient
  // bad state we should ride out rather than act on.
  if (trackDurationMs < MIN_DURATION_MS) return "none";
  if (elapsedMs < 0) return "none";

  if (!hasEmittedNowPlaying) return "now_playing";

  if (!hasEmittedSubmission) {
    const threshold = Math.min(trackDurationMs / 2, SUBMISSION_CAP_MS);
    if (elapsedMs >= threshold) return "submission";
  }

  return "none";
}
