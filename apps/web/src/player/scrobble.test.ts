// Threshold rules follow the de-facto Subsonic / Last.fm standard:
//   - tracks shorter than 30 s never scrobble (avoids stingers + silence padding)
//   - submission fires at min(50% of duration, 4 minutes), once
//   - now_playing fires once when playback first becomes evaluable
// We do NOT couple the function to the audio element — it's pure over the
// four-field state, so PlayerContext can call it from any handler that has
// the numbers. Returning a single decision (vs. emitting both) keeps the
// caller's branching trivial and forces an explicit "I emitted X, set the
// flag, now ask again" round-trip — which matches how the network calls
// will land async anyway.

import { describe, expect, it } from "vitest";
import { evaluateScrobble, type ScrobbleState } from "./scrobble";

const base: ScrobbleState = {
  trackDurationMs: 0,
  elapsedMs: 0,
  hasEmittedNowPlaying: false,
  hasEmittedSubmission: false,
};

describe("evaluateScrobble — preconditions", () => {
  it("returns none when duration is unknown (still loading)", () => {
    expect(evaluateScrobble({ ...base, trackDurationMs: 0 })).toBe("none");
  });

  it("returns none for tracks shorter than 30 seconds", () => {
    expect(
      evaluateScrobble({ ...base, trackDurationMs: 29_999, elapsedMs: 28_000 })
    ).toBe("none");
  });

  it("returns none for negative elapsed (clock skew, seek bugs)", () => {
    expect(
      evaluateScrobble({ ...base, trackDurationMs: 60_000, elapsedMs: -1 })
    ).toBe("none");
  });

  it("returns none post-end if both events already emitted", () => {
    // After ended, audio elements may briefly report elapsed > duration.
    // We must not re-fire a submission in that window.
    expect(
      evaluateScrobble({
        trackDurationMs: 60_000,
        elapsedMs: 60_500,
        hasEmittedNowPlaying: true,
        hasEmittedSubmission: true,
      })
    ).toBe("none");
  });
});

describe("evaluateScrobble — now_playing", () => {
  it("fires once at the start of a sufficiently long track", () => {
    expect(
      evaluateScrobble({ ...base, trackDurationMs: 180_000, elapsedMs: 0 })
    ).toBe("now_playing");
  });

  it("does not repeat once recorded", () => {
    expect(
      evaluateScrobble({
        ...base,
        trackDurationMs: 180_000,
        elapsedMs: 0,
        hasEmittedNowPlaying: true,
      })
    ).toBe("none");
  });

  it("is suppressed for sub-30s tracks", () => {
    expect(
      evaluateScrobble({ ...base, trackDurationMs: 25_000, elapsedMs: 0 })
    ).toBe("none");
  });
});

describe("evaluateScrobble — submission via 50% rule (short tracks)", () => {
  it("does not fire just before 50%", () => {
    expect(
      evaluateScrobble({
        trackDurationMs: 90_000,
        elapsedMs: 44_999,
        hasEmittedNowPlaying: true,
        hasEmittedSubmission: false,
      })
    ).toBe("none");
  });

  it("fires at exactly 50%", () => {
    expect(
      evaluateScrobble({
        trackDurationMs: 90_000,
        elapsedMs: 45_000,
        hasEmittedNowPlaying: true,
        hasEmittedSubmission: false,
      })
    ).toBe("submission");
  });
});

describe("evaluateScrobble — submission via 4-minute cap (long tracks)", () => {
  it("does not fire just before 4 minutes", () => {
    expect(
      evaluateScrobble({
        trackDurationMs: 600_000, // 10 min
        elapsedMs: 239_999,
        hasEmittedNowPlaying: true,
        hasEmittedSubmission: false,
      })
    ).toBe("none");
  });

  it("fires at exactly 4 minutes", () => {
    expect(
      evaluateScrobble({
        trackDurationMs: 600_000,
        elapsedMs: 240_000,
        hasEmittedNowPlaying: true,
        hasEmittedSubmission: false,
      })
    ).toBe("submission");
  });

  it("4-minute cap dominates the 50% rule for long tracks", () => {
    // 30-min track: 50% would be 15 min but the 4-min cap fires first.
    expect(
      evaluateScrobble({
        trackDurationMs: 30 * 60_000,
        elapsedMs: 240_000,
        hasEmittedNowPlaying: true,
        hasEmittedSubmission: false,
      })
    ).toBe("submission");
  });
});

describe("evaluateScrobble — once-only submission", () => {
  it("does not repeat after emission", () => {
    expect(
      evaluateScrobble({
        trackDurationMs: 90_000,
        elapsedMs: 80_000,
        hasEmittedNowPlaying: true,
        hasEmittedSubmission: true,
      })
    ).toBe("none");
  });
});

describe("evaluateScrobble — ordering", () => {
  it("prefers now_playing when both could fire on the same call", () => {
    // Edge case: caller didn't get a chance to emit now_playing yet but
    // elapsed already crossed the submission threshold (e.g., track
    // started already-buffered + a very fast first timeupdate). We emit
    // now_playing first; submission lands on the next call.
    expect(
      evaluateScrobble({
        trackDurationMs: 90_000,
        elapsedMs: 50_000,
        hasEmittedNowPlaying: false,
        hasEmittedSubmission: false,
      })
    ).toBe("now_playing");
  });

  it("emits submission once now_playing has been recorded", () => {
    expect(
      evaluateScrobble({
        trackDurationMs: 90_000,
        elapsedMs: 50_000,
        hasEmittedNowPlaying: true,
        hasEmittedSubmission: false,
      })
    ).toBe("submission");
  });
});
