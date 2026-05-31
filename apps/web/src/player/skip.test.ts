// evaluateSkip is pure over three numbers/flags so PlayerContext can call
// it from any gesture handler (next/prev/click/media-key) that has the
// outgoing track's position. The rules:
//   - a track that ended naturally is never a skip (auto-advance ≠ dislike)
//   - tracks shorter than 30 s are navigation, not signal (matches scrobble)
//   - the track must have actually started (playedMs > 0)
//   - the reported position is clamped to [0, duration]

import { describe, expect, it } from "vitest";
import { evaluateSkip, type SkipState } from "./skip";

const base: SkipState = {
  trackDurationMs: 180_000,
  playedMs: 0,
  endedNaturally: false,
};

describe("evaluateSkip — non-skips", () => {
  it("never emits when the track ended naturally", () => {
    expect(
      evaluateSkip({ ...base, playedMs: 179_000, endedNaturally: true })
    ).toEqual({ emit: false, playedMs: 0 });
  });

  it("does not emit for tracks shorter than 30 seconds", () => {
    expect(
      evaluateSkip({ ...base, trackDurationMs: 29_999, playedMs: 5_000 })
    ).toEqual({ emit: false, playedMs: 0 });
  });

  it("does not emit when duration is unknown (still loading)", () => {
    expect(
      evaluateSkip({ ...base, trackDurationMs: 0, playedMs: 1_000 })
    ).toEqual({ emit: false, playedMs: 0 });
  });

  it("does not emit when nothing has played yet", () => {
    expect(evaluateSkip({ ...base, playedMs: 0 })).toEqual({
      emit: false,
      playedMs: 0,
    });
  });

  it("does not emit for a negative position (clock skew / seek glitch)", () => {
    expect(evaluateSkip({ ...base, playedMs: -1 })).toEqual({
      emit: false,
      playedMs: 0,
    });
  });
});

describe("evaluateSkip — skips", () => {
  it("emits an early skip with the played position", () => {
    expect(
      evaluateSkip({ ...base, trackDurationMs: 200_000, playedMs: 3_000 })
    ).toEqual({ emit: true, playedMs: 3_000 });
  });

  it("emits a late skip just before the natural end", () => {
    expect(
      evaluateSkip({ ...base, trackDurationMs: 200_000, playedMs: 195_500 })
    ).toEqual({ emit: true, playedMs: 195_500 });
  });

  it("rounds a fractional position to whole milliseconds", () => {
    expect(
      evaluateSkip({ ...base, trackDurationMs: 200_000, playedMs: 1234.7 })
    ).toEqual({ emit: true, playedMs: 1235 });
  });

  it("clamps a position that overshoots the duration", () => {
    expect(
      evaluateSkip({ ...base, trackDurationMs: 200_000, playedMs: 200_400 })
    ).toEqual({ emit: true, playedMs: 200_000 });
  });

  it("emits at exactly the 30 s minimum duration", () => {
    expect(
      evaluateSkip({ ...base, trackDurationMs: 30_000, playedMs: 2_000 })
    ).toEqual({ emit: true, playedMs: 2_000 });
  });
});
