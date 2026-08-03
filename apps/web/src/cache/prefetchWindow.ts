// Which tracks the cache should pull, given where the queue cursor is.
//
// Pure so the interesting rule — "never download the track the <audio>
// element is streaming right now" — can be asserted without an IndexedDB or a
// network. AudioCacheContext supplies the window constants and the
// blob-backed predicate.

/** The shape the download pass needs from a queue item. */
export interface QueueItemLike {
  readonly track_id: string;
}

export interface DownloadWindow {
  /** How many items behind the cursor to include. */
  readonly behind: number;
  /** How many items ahead of the cursor to include. */
  readonly ahead: number;
}

/**
 * Track ids to fetch into the cache, in play order and deduped.
 *
 * `isBlobBacked(id)` answers "is this track already resolved to a local
 * blob: URL?". Its one load-bearing use is the current track: if the cursor's
 * track is *not* blob-backed, the element is streaming it from the network
 * this instant, and fetching it in parallel is a second full download of a
 * song already in flight. That track is skipped; it becomes eligible on the
 * next advance, when it falls into the behind-window and its stream is done.
 *
 * Tracks *around* the cursor are always fair game — that's the point. Their
 * download replaces the streaming fetch they would otherwise have paid for.
 */
export function downloadTargets(
  items: readonly QueueItemLike[],
  cursor: number,
  window: DownloadWindow,
  isBlobBacked: (trackId: string) => boolean,
): string[] {
  if (cursor < 0 || cursor >= items.length) return [];
  const currentId = items[cursor]?.track_id;
  const out: string[] = [];
  const seen = new Set<string>();
  for (let j = cursor - window.behind; j <= cursor + window.ahead; j++) {
    const id = items[j]?.track_id;
    if (id === undefined) continue;
    if (id === currentId && !isBlobBacked(id)) continue;
    if (seen.has(id)) continue;
    seen.add(id);
    out.push(id);
  }
  return out;
}
