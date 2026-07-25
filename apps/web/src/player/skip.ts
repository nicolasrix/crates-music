// Pure skip-decision function. Tested in skip.test.ts.
//
// A "skip" is a *manual* abandonment of the current track before it ends
// naturally — clicking next/prev, picking another track, or a media-key
// next/prev. The natural end-of-track auto-advance is NOT a skip.
//
// The gateway folds skips into the per-track preference affinity, scaled by
// how far the user got (played_ms / duration). It already handles the early-
// vs-late distinction (an early skip penalises hard, a near-complete skip
// barely at all), so this function's only job is to decide *whether* to
// report the abandonment and with what position — never the penalty itself.
//
// All time fields are milliseconds.

const MIN_DURATION_MS = 30_000; // mirror scrobble: ignore stingers/interludes

export interface SkipState {
  trackDurationMs: number;
  playedMs: number;
  /** True when playback reached the natural end (the `ended` event). */
  endedNaturally: boolean;
}

export interface SkipDecision {
  emit: boolean;
  /** Position to report, clamped to [0, duration] and rounded to whole ms. */
  playedMs: number;
}

const NO_SKIP: SkipDecision = { emit: false, playedMs: 0 };

export function evaluateSkip(state: SkipState): SkipDecision {
  const { trackDurationMs, playedMs, endedNaturally } = state;

  // A track that played to its natural end was not skipped.
  if (endedNaturally) return NO_SKIP;
  // Unknown or very short duration: treat moving on as navigation past an
  // interlude, not a dislike. (trackDurationMs === 0 means we never learned
  // the duration, e.g. the track barely started loading.)
  if (trackDurationMs < MIN_DURATION_MS) return NO_SKIP;
  // Must have actually started for the abandonment to carry signal. Guards
  // the "clicked the wrong row, immediately clicked the right one" case and
  // any negative position from clock skew or seek glitches.
  if (playedMs <= 0) return NO_SKIP;

  // Clamp the reported position: some browsers briefly report currentTime
  // slightly past duration near the tail. The server clamps completion too,
  // but reporting an honest position keeps the diagnostics readable.
  return { emit: true, playedMs: Math.round(Math.min(playedMs, trackDurationMs)) };
}
