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
  it("anchor gets weight 3 and is an anchorId", () => {
    const { seeds, anchorIds } = buildAutoplaySeeds({
      items: [item("qi-1", "t-anchor")],
      nowPlayingIndex: 0,
      sessionAnchor: anchor("t-anchor"),
      recommendedItemIds: new Set(),
    });
    const a = seeds.find((s) => s.trackId === "t-anchor")!;
    expect(a.weight).toBe(WEIGHT_ANCHOR);
    expect(anchorIds).toContain("t-anchor");
  });

  it("user-picked items at/after cursor get weight 2", () => {
    const { seeds } = buildAutoplaySeeds({
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
    const { seeds } = buildAutoplaySeeds({
      items: [item("qi-1", "t-played"), item("qi-2", "t-current")],
      nowPlayingIndex: 1,
      sessionAnchor: null,
      recommendedItemIds: new Set(),
    });
    const played = seeds.find((s) => s.trackId === "t-played")!;
    expect(played.weight).toBe(WEIGHT_SCROBBLE);
  });

  it("recommendation-added items contribute no boundary weight or anchor", () => {
    const { seeds, anchorIds } = buildAutoplaySeeds({
      items: [item("qi-1", "t-user"), item("qi-2", "t-algo")],
      nowPlayingIndex: 0,
      sessionAnchor: null,
      recommendedItemIds: new Set(["qi-2"]),
    });
    const ids = seeds.map((s) => s.trackId);
    expect(ids).toContain("t-user");
    // With no frontier, the algo track contributes nothing at all.
    expect(ids).not.toContain("t-algo");
    expect(anchorIds).not.toContain("t-algo");
  });

  it("anchor + user-picked + scrobbles compose into one seed list", () => {
    const { seeds, anchorIds } = buildAutoplaySeeds({
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
    expect(anchorIds.sort()).toEqual(["t-anchor", "t-picked", "t-played"]);
  });

  it("duplicates by track_id collapse to the highest weight", () => {
    const { seeds } = buildAutoplaySeeds({
      items: [item("qi-1", "t-x"), item("qi-2", "t-x")],
      nowPlayingIndex: 1,
      sessionAnchor: anchor("t-x"),
      recommendedItemIds: new Set(),
    });
    const matching = seeds.filter((s) => s.trackId === "t-x");
    expect(matching).toHaveLength(1);
    expect(matching[0]!.weight).toBe(WEIGHT_ANCHOR);
  });

  it("empty queue + no anchor returns empty seed plan", () => {
    const { seeds, anchorIds } = buildAutoplaySeeds({
      items: [],
      nowPlayingIndex: null,
      sessionAnchor: null,
      recommendedItemIds: new Set(),
    });
    expect(seeds).toEqual([]);
    expect(anchorIds).toEqual([]);
  });

  it("all-recommendation queue + no anchor returns empty boundary, no anchors", () => {
    // The drift-trap case for the *boundary*: with no frontier, an
    // all-algo queue and no anchor yields no seeds and no anchors.
    const { seeds, anchorIds } = buildAutoplaySeeds({
      items: [item("qi-1", "t-1"), item("qi-2", "t-2")],
      nowPlayingIndex: 0,
      sessionAnchor: null,
      recommendedItemIds: new Set(["qi-1", "qi-2"]),
    });
    expect(seeds).toEqual([]);
    expect(anchorIds).toEqual([]);
  });

  it("result is sorted by descending weight then ascending track_id", () => {
    const { seeds } = buildAutoplaySeeds({
      items: [item("qi-1", "t-b"), item("qi-2", "t-a"), item("qi-3", "t-c")],
      nowPlayingIndex: 1,
      sessionAnchor: anchor("t-z"),
      recommendedItemIds: new Set(),
    });
    // weights: t-z=3, t-a=2, t-c=2, t-b=1
    expect(seeds.map((s) => s.trackId)).toEqual(["t-z", "t-a", "t-c", "t-b"]);
  });

  // --- recency frontier (the travel force) -------------------------------

  it("frontier adds recency-decayed seeds for algo-added recent tracks", () => {
    // Queue: [user-anchor (played), algo-1 (played), algo-2 (now playing)].
    // Without a frontier the two algo tracks contribute nothing; with one
    // they re-enter the seed pool at β·decayᵃᵍᵉ.
    const { seeds, anchorIds } = buildAutoplaySeeds({
      items: [
        item("qi-1", "t-user"),
        item("qi-2", "t-algo1"),
        item("qi-3", "t-algo2"),
      ],
      nowPlayingIndex: 2,
      sessionAnchor: null,
      recommendedItemIds: new Set(["qi-2", "qi-3"]),
      frontier: { weight: 0.2, decay: 0.5, window: 3 },
    });
    const byId = new Map(seeds.map((s) => [s.trackId, s.weight] as const));
    // age 0 (now playing) → 0.2 ; age 1 → 0.1
    expect(byId.get("t-algo2")).toBeCloseTo(0.2, 6);
    expect(byId.get("t-algo1")).toBeCloseTo(0.1, 6);
    // t-user is a scrobble (played, non-algo) → boundary weight wins over
    // its frontier weight (age 2 → 0.05).
    expect(byId.get("t-user")).toBe(WEIGHT_SCROBBLE);
    // Frontier tracks are NOT anchors; only the user track is.
    expect(anchorIds).toEqual(["t-user"]);
  });

  it("frontier window bounds how far back recency reaches", () => {
    const { seeds } = buildAutoplaySeeds({
      items: [
        item("qi-1", "t-old"),
        item("qi-2", "t-mid"),
        item("qi-3", "t-now"),
      ],
      nowPlayingIndex: 2,
      sessionAnchor: null,
      recommendedItemIds: new Set(["qi-1", "qi-2", "qi-3"]),
      frontier: { weight: 0.2, decay: 0.5, window: 2 },
    });
    const ids = seeds.map((s) => s.trackId);
    // window 2 covers now (age 0) + mid (age 1); t-old (age 2) is out.
    expect(ids).toContain("t-now");
    expect(ids).toContain("t-mid");
    expect(ids).not.toContain("t-old");
  });

  it("frontier weight 0 disables travel (anchor-only)", () => {
    const { seeds } = buildAutoplaySeeds({
      items: [item("qi-1", "t-user"), item("qi-2", "t-algo")],
      nowPlayingIndex: 1,
      sessionAnchor: null,
      recommendedItemIds: new Set(["qi-2"]),
      frontier: { weight: 0, decay: 0.5, window: 3 },
    });
    expect(seeds.map((s) => s.trackId)).not.toContain("t-algo");
  });
});
