import { describe, expect, it } from "vitest";
import {
  WEIGHT_ANCHOR,
  WEIGHT_SCROBBLE,
  WEIGHT_USER_PICKED,
  buildAutoplaySeeds,
} from "./autoplaySeeds";
import type { QueueItem, SessionAnchor } from "../sync/types";

const item = (item_id: string, track_id: string): QueueItem => ({ item_id, track_id });
const anchor = (track_id: string): SessionAnchor => ({
  session_id: "sess-1",
  track_id,
  started_ms: 1_700_000_000_000,
});

describe("buildAutoplaySeeds", () => {
  it("anchor gets weight 3", () => {
    const seeds = buildAutoplaySeeds({
      items: [item("qi-1", "t-anchor")],
      nowPlayingIndex: 0,
      sessionAnchor: anchor("t-anchor"),
      recommendedItemIds: new Set(),
    });
    const a = seeds.find((s) => s.trackId === "t-anchor")!;
    expect(a.weight).toBe(WEIGHT_ANCHOR);
  });

  it("user-picked items at/after cursor get weight 2", () => {
    const seeds = buildAutoplaySeeds({
      items: [item("qi-1", "t-1"), item("qi-2", "t-2"), item("qi-3", "t-3")],
      nowPlayingIndex: 1,
      sessionAnchor: null,
      recommendedItemIds: new Set(),
    });
    const t2 = seeds.find((s) => s.trackId === "t-2")!;
    const t3 = seeds.find((s) => s.trackId === "t-3")!;
    expect(t2.weight).toBe(WEIGHT_USER_PICKED);
    expect(t3.weight).toBe(WEIGHT_USER_PICKED);
  });

  it("items before the cursor (already-played scrobbles) get weight 1", () => {
    const seeds = buildAutoplaySeeds({
      items: [item("qi-1", "t-played"), item("qi-2", "t-current")],
      nowPlayingIndex: 1,
      sessionAnchor: null,
      recommendedItemIds: new Set(),
    });
    const played = seeds.find((s) => s.trackId === "t-played")!;
    expect(played.weight).toBe(WEIGHT_SCROBBLE);
  });

  it("recommendation-added items contribute nothing", () => {
    const seeds = buildAutoplaySeeds({
      items: [item("qi-1", "t-user"), item("qi-2", "t-algo")],
      nowPlayingIndex: 0,
      sessionAnchor: null,
      recommendedItemIds: new Set(["qi-2"]),
    });
    const ids = seeds.map((s) => s.trackId);
    expect(ids).toContain("t-user");
    expect(ids).not.toContain("t-algo");
  });

  it("anchor + user-picked + scrobbles compose into one seed list", () => {
    // Queue: [played, anchor=current, picked-after]
    // Anchor is current, so:
    //   played → scrobble (1)
    //   anchor → 3 (and is at cursor — but anchor weight wins)
    //   picked-after → user-picked (2)
    const seeds = buildAutoplaySeeds({
      items: [
        item("qi-1", "t-played"),
        item("qi-2", "t-anchor"),
        item("qi-3", "t-picked"),
      ],
      nowPlayingIndex: 1,
      sessionAnchor: anchor("t-anchor"),
      recommendedItemIds: new Set(),
    });
    const byId = new Map(seeds.map((s) => [s.trackId, s.weight] as const));
    expect(byId.get("t-anchor")).toBe(WEIGHT_ANCHOR);
    expect(byId.get("t-picked")).toBe(WEIGHT_USER_PICKED);
    expect(byId.get("t-played")).toBe(WEIGHT_SCROBBLE);
  });

  it("duplicates by track_id collapse to the highest weight", () => {
    // Anchor track also appears as a user-picked item later — anchor
    // weight (3) wins, no duplicate entry.
    const seeds = buildAutoplaySeeds({
      items: [item("qi-1", "t-x"), item("qi-2", "t-x")],
      nowPlayingIndex: 1,
      sessionAnchor: anchor("t-x"),
      recommendedItemIds: new Set(),
    });
    const matching = seeds.filter((s) => s.trackId === "t-x");
    expect(matching).toHaveLength(1);
    expect(matching[0]!.weight).toBe(WEIGHT_ANCHOR);
  });

  it("empty queue + no anchor returns empty seed list", () => {
    const seeds = buildAutoplaySeeds({
      items: [],
      nowPlayingIndex: null,
      sessionAnchor: null,
      recommendedItemIds: new Set(),
    });
    expect(seeds).toEqual([]);
  });

  it("all-recommendation queue + no anchor returns empty seed list", () => {
    // The drift-trap case. If every queue item came from the
    // recommender (autoplay's own past output), the seed list is
    // empty — refusing to reseed is correct behaviour. Caller
    // handles the empty case (e.g. no refill until user intervenes).
    const seeds = buildAutoplaySeeds({
      items: [item("qi-1", "t-1"), item("qi-2", "t-2")],
      nowPlayingIndex: 0,
      sessionAnchor: null,
      recommendedItemIds: new Set(["qi-1", "qi-2"]),
    });
    expect(seeds).toEqual([]);
  });

  it("result is sorted by descending weight then ascending track_id", () => {
    const seeds = buildAutoplaySeeds({
      items: [item("qi-1", "t-b"), item("qi-2", "t-a"), item("qi-3", "t-c")],
      nowPlayingIndex: 1,
      sessionAnchor: anchor("t-z"),
      recommendedItemIds: new Set(),
    });
    // weights: t-z=3, t-a=2, t-c=2, t-b=1
    expect(seeds.map((s) => s.trackId)).toEqual(["t-z", "t-a", "t-c", "t-b"]);
  });
});
