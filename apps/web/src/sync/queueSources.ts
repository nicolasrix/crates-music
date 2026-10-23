// Source tagging for rewritten queue tails.
//
// `replace_upcoming` re-mints every upcoming item (a reshuffle is a new
// list, not a reorder), so the `source` each item was queued with would be
// lost on the first mode flip. Carrying it over by track id keeps "this
// came from album X" attached however often the tail is reshuffled.

import type { PlaySource, QueueItem } from "./types";

/** Fresh queue items for `trackIds`. Each takes its source from
 *  `sourceFor`, else from the same track in `live`, else none. */
export function sourcedItems(
  trackIds: readonly string[],
  mintId: () => string,
  live: readonly QueueItem[],
  sourceFor?: (trackId: string) => PlaySource | undefined,
): QueueItem[] {
  const carried = new Map<string, PlaySource>();
  for (const item of live) {
    if (item.source && !carried.has(item.track_id)) carried.set(item.track_id, item.source);
  }
  return trackIds.map((id) => {
    const source = sourceFor?.(id) ?? carried.get(id);
    return { item_id: mintId(), track_id: id, ...(source ? { source } : {}) };
  });
}
