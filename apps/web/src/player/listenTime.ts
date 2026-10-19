// How much of a track was actually heard, for the listening log.
//
// Playback position alone can't say that: a listener who seeks to the last
// minute "played to 95%" having heard one minute. So the player folds every
// `timeupdate` position into a clock that only counts small forward steps —
// steady playback advances ~250 ms per tick, while a seek in either
// direction jumps further and is dropped.

import type { PlaySource, QueueItem } from "../sync/types";

/** Largest position step still counted as listening. Generous enough for
 *  throttled background tabs, well under any deliberate seek. */
export const MAX_TICK_MS = 5_000;

export interface ListenClock {
  /** Milliseconds of audio heard on this load of the track. */
  readonly heardMs: number;
  /** Position at the previous tick; null before the first. */
  readonly lastPosMs: number | null;
}

export const FRESH_CLOCK: ListenClock = { heardMs: 0, lastPosMs: null };

/** Fold one playback position into the clock. */
export function tick(clock: ListenClock, posMs: number): ListenClock {
  if (clock.lastPosMs === null) return { heardMs: clock.heardMs, lastPosMs: posMs };
  const step = posMs - clock.lastPosMs;
  const counted = step > 0 && step <= MAX_TICK_MS ? step : 0;
  return { heardMs: clock.heardMs + counted, lastPosMs: posMs };
}

/** Source of the queue item at the cursor, if that item is `trackId`. A
 *  cursor that has already moved on yields nothing rather than the next
 *  item's source. */
export function sourceAtCursor(
  items: readonly QueueItem[],
  nowPlayingIndex: number | null,
  trackId: string,
): PlaySource | undefined {
  if (nowPlayingIndex === null) return undefined;
  const item = items[nowPlayingIndex];
  return item?.track_id === trackId ? item.source : undefined;
}
