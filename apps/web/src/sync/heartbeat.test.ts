import { describe, expect, it } from "vitest";
import { PONG_TIMEOUT_MS, pingOverdue } from "./heartbeat";

describe("pingOverdue", () => {
  it("is never overdue with no ping outstanding", () => {
    // An idle room sends nothing, so silence alone proves nothing — and a
    // timer throttled for ten minutes in a background tab must not read
    // as a dead socket.
    expect(pingOverdue(null, 10 * 60_000)).toBe(false);
  });

  it("gives an outstanding ping the full timeout to be answered", () => {
    expect(pingOverdue(1_000, 1_000 + PONG_TIMEOUT_MS)).toBe(false);
  });

  it("declares the socket dead once the timeout passes unanswered", () => {
    expect(pingOverdue(1_000, 1_001 + PONG_TIMEOUT_MS)).toBe(true);
  });

  it("honours a shorter timeout for a wake-up probe", () => {
    expect(pingOverdue(1_000, 7_000, 5_000)).toBe(true);
    expect(pingOverdue(1_000, 5_000, 5_000)).toBe(false);
  });
});
