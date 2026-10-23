import { describe, expect, it } from "vitest";

import { sourcedItems } from "./queueSources";
import type { QueueItem } from "./types";

const album = { kind: "album", id: "al-1" };
const mint = (() => {
  let n = 0;
  return () => `new-${(n += 1)}`;
})();

describe("sourcedItems", () => {
  it("carries each track's source over from the live queue", () => {
    const live: QueueItem[] = [
      { item_id: "a", track_id: "t1", source: album },
      { item_id: "b", track_id: "t2", source: album },
    ];
    const items = sourcedItems(["t2", "t1"], mint, live);
    expect(items.map((i) => [i.track_id, i.source])).toEqual([
      ["t2", album],
      ["t1", album],
    ]);
    expect(new Set(items.map((i) => i.item_id)).size).toBe(2);
  });

  it("lets an explicit source win over the carried one", () => {
    const live: QueueItem[] = [{ item_id: "a", track_id: "t1", source: album }];
    const rec = { kind: "smart_shuffle" };
    const items = sourcedItems(["t1", "r1"], mint, live, (id) =>
      id === "r1" ? rec : undefined,
    );
    expect(items[0]!.source).toEqual(album);
    expect(items[1]!.source).toEqual(rec);
  });

  it("omits the field when nothing is known", () => {
    const [item] = sourcedItems(["t9"], mint, []);
    expect(item).not.toHaveProperty("source");
  });
});
