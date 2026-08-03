import { describe, expect, it } from "vitest";

import { downloadTargets } from "./prefetchWindow";

const queue = (...ids: string[]) => ids.map((track_id) => ({ track_id }));
const WINDOW = { behind: 1, ahead: 1 };
const nothingCached = () => false;
const allCached = () => true;

describe("downloadTargets", () => {
  it("skips the track the element is streaming right now", () => {
    // The whole point: `b` is at the cursor and has no blob, so the browser
    // is pulling it over the network this instant. Fetching it again is the
    // duplicate download we're here to remove.
    expect(downloadTargets(queue("a", "b", "c"), 1, WINDOW, nothingCached)).toEqual(["a", "c"]);
  });

  it("includes the current track once it is blob-backed", () => {
    // Nothing is in flight for it, so a touch/no-op fetch is harmless — and
    // this is the path that keeps its LRU timestamp fresh.
    expect(downloadTargets(queue("a", "b", "c"), 1, WINDOW, allCached)).toEqual(["a", "b", "c"]);
  });

  it("picks up the just-finished track on the next advance", () => {
    // `b` was skipped while streaming at cursor 1; at cursor 2 it is behind
    // the cursor, its stream has finished, and it becomes eligible.
    const targets = downloadTargets(queue("a", "b", "c", "d"), 2, WINDOW, nothingCached);
    expect(targets).toContain("b");
    expect(targets).not.toContain("c"); // the new current, still streaming
  });

  it("clamps at both ends of the queue", () => {
    expect(downloadTargets(queue("a", "b"), 0, WINDOW, nothingCached)).toEqual(["b"]);
    expect(downloadTargets(queue("a", "b"), 1, WINDOW, nothingCached)).toEqual(["a"]);
  });

  it("returns nothing for an out-of-range or empty cursor", () => {
    expect(downloadTargets(queue("a"), -1, WINDOW, nothingCached)).toEqual([]);
    expect(downloadTargets(queue("a"), 5, WINDOW, nothingCached)).toEqual([]);
    expect(downloadTargets([], 0, WINDOW, nothingCached)).toEqual([]);
  });

  it("dedupes a track that appears twice in the window", () => {
    // Same song queued back to back — one fetch, not two.
    expect(downloadTargets(queue("a", "b", "a"), 1, WINDOW, nothingCached)).toEqual(["a"]);
  });

  it("respects a wider ahead window", () => {
    const targets = downloadTargets(queue("a", "b", "c", "d", "e"), 1, { behind: 0, ahead: 3 }, nothingCached);
    expect(targets).toEqual(["c", "d", "e"]);
  });
});
