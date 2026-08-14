// activeLineIndex is the whole highlight feature. The cases that matter
// are the boundaries: before the first line, exactly on a timestamp, past
// the last line, and the degenerate documents (empty, single line).

import { describe, expect, it } from "vitest";
import { activeLineIndex, sortLines } from "./activeLine";
import type { LyricLine } from "../api/lyrics";

const LINES: LyricLine[] = [
  { start_ms: 0, text: "first" },
  { start_ms: 5_000, text: "second" },
  { start_ms: 12_500, text: "third" },
  { start_ms: 20_000, text: "fourth" },
];

describe("activeLineIndex", () => {
  it("returns -1 before the first line starts", () => {
    const lines: LyricLine[] = [{ start_ms: 4_000, text: "late start" }];
    expect(activeLineIndex(lines, 0)).toBe(-1);
    expect(activeLineIndex(lines, 3_999)).toBe(-1);
  });

  it("activates a line exactly on its timestamp", () => {
    expect(activeLineIndex(LINES, 5_000)).toBe(1);
    expect(activeLineIndex(LINES, 12_500)).toBe(2);
  });

  it("holds a line until the next one starts", () => {
    expect(activeLineIndex(LINES, 5_001)).toBe(1);
    expect(activeLineIndex(LINES, 12_499)).toBe(1);
  });

  it("stays on the last line through the outro", () => {
    expect(activeLineIndex(LINES, 20_000)).toBe(3);
    expect(activeLineIndex(LINES, 999_999)).toBe(3);
  });

  it("survives an empty document", () => {
    expect(activeLineIndex([], 1_000)).toBe(-1);
  });

  it("handles a single-line document at both edges", () => {
    const one: LyricLine[] = [{ start_ms: 1_000, text: "only" }];
    expect(activeLineIndex(one, 999)).toBe(-1);
    expect(activeLineIndex(one, 1_000)).toBe(0);
  });

  it("picks the last of several lines sharing a timestamp", () => {
    // Repeated stamps happen: an LRC chorus block expands to identical
    // marks. Any of them is defensible; being deterministic is the point.
    const dupes: LyricLine[] = [
      { start_ms: 1_000, text: "a" },
      { start_ms: 1_000, text: "b" },
      { start_ms: 2_000, text: "c" },
    ];
    expect(activeLineIndex(dupes, 1_500)).toBe(1);
  });
});

describe("sortLines", () => {
  it("orders by start and leaves the input untouched", () => {
    const unsorted: LyricLine[] = [
      { start_ms: 900, text: "b" },
      { start_ms: 100, text: "a" },
    ];
    const sorted = sortLines(unsorted);
    expect(sorted.map((l) => l.text)).toEqual(["a", "b"]);
    expect(unsorted.map((l) => l.text)).toEqual(["b", "a"]);
  });
});
