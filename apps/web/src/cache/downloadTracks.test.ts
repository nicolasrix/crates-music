import { describe, expect, it, vi } from "vitest";
import { downloadTracks, type TrackPinner } from "./downloadTracks";
import type { PinOutcome } from "./audioCache";

function pinner(outcomes: Record<string, PinOutcome | "throw">): TrackPinner {
  return {
    download: async (id) => {
      const o = outcomes[id];
      if (o === undefined || o === "throw") throw new Error(`no ${id}`);
      return o;
    },
  };
}

const PINNED: PinOutcome = { kind: "pinned" };

describe("downloadTracks", () => {
  it("counts every pinned track and reports no shortfall", async () => {
    const r = await downloadTracks(pinner({ a: PINNED, b: PINNED }), ["a", "b"]);
    expect(r).toEqual({ saved: 2, failed: 0, shortBy: null });
  });

  it("counts already-pinned tracks as saved", async () => {
    const r = await downloadTracks(
      pinner({ a: { kind: "already-pinned" } }),
      ["a"],
    );
    expect(r.saved).toBe(1);
  });

  it("tolerates a failing track and keeps going", async () => {
    const r = await downloadTracks(
      pinner({ a: PINNED, b: "throw", c: PINNED }),
      ["a", "b", "c"],
    );
    expect(r).toEqual({ saved: 2, failed: 1, shortBy: null });
  });

  it("stops at the budget and reports the shortfall", async () => {
    const seen: string[] = [];
    const cache: TrackPinner = {
      download: async (id) => {
        seen.push(id);
        return id === "b"
          ? { kind: "would-exceed-budget", overBy: 4096 }
          : PINNED;
      },
    };
    const r = await downloadTracks(cache, ["a", "b", "c"]);
    expect(r).toEqual({ saved: 1, failed: 0, shortBy: 4096 });
    // "c" must never be fetched — every later pin fails the same check.
    expect(seen).toEqual(["a", "b"]);
  });

  it("reports progress once per attempted track, failures included", async () => {
    const onProgress = vi.fn();
    await downloadTracks(
      pinner({ a: PINNED, b: "throw" }),
      ["a", "b"],
      onProgress,
    );
    expect(onProgress.mock.calls).toEqual([
      [1, 2],
      [2, 2],
    ]);
  });

  it("does not report progress for the budget-stopped track", async () => {
    const onProgress = vi.fn();
    const cache: TrackPinner = {
      download: async () => ({ kind: "would-exceed-budget", overBy: 1 }),
    };
    await downloadTracks(cache, ["a"], onProgress);
    expect(onProgress).not.toHaveBeenCalled();
  });

  it("handles an empty list", async () => {
    const r = await downloadTracks(pinner({}), []);
    expect(r).toEqual({ saved: 0, failed: 0, shortBy: null });
  });
});
