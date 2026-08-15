// Which lyric line is "now"? Pure, so it can be tested without an
// <audio> element, a network, or a React tree — and so the render loop
// that calls it 60×/s stays trivially cheap.
//
// The whole highlight feature is this function plus a rule: only call
// setState when the returned index *changes*. A naive implementation
// would setState on every animation frame and re-render the entire
// lyric list 60 times a second for a value that changes roughly once
// every three seconds.

import type { LyricLine } from "../api/lyrics";

/** Highlight this far ahead of the timestamp. Sung lines land slightly
 *  after their mark, and a reader needs a beat to find the line before
 *  it is sung — a small lead reads as "in time", a zero lead as "late".
 *  Small enough that it never crosses into the previous line's territory
 *  for normally-spaced lyrics. */
export const HIGHLIGHT_LEAD_MS = 150;

/**
 * Index of the last line that has started by `positionMs`, or `-1` when
 * playback is still ahead of the first line (intro, count-in).
 *
 * Precondition: `lines` is sorted ascending by `start_ms` — use
 * {@link sortLines} on anything whose ordering you have not established.
 * Binary search, so a 200-line document costs ~8 comparisons per frame.
 */
export function activeLineIndex(lines: readonly LyricLine[], positionMs: number): number {
  let lo = 0;
  let hi = lines.length - 1;
  let found = -1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    // Non-null: mid is always within [lo, hi] ⊆ [0, length).
    if (lines[mid]!.start_ms <= positionMs) {
      found = mid;
      lo = mid + 1;
    } else {
      hi = mid - 1;
    }
  }
  return found;
}

/** A sorted copy — never a mutation of the caller's array, which is held
 *  in a query cache and shared with other renders. */
export function sortLines(lines: readonly LyricLine[]): LyricLine[] {
  return [...lines].sort((a, b) => a.start_ms - b.start_ms);
}
