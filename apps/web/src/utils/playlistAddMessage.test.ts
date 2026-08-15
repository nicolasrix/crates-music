import { describe, expect, it } from "vitest";

import { playlistAddMessage } from "./playlistAddMessage";

describe("playlistAddMessage", () => {
  it("confirms a plain single-track add", () => {
    expect(playlistAddMessage("Roadtrip", { added: 1, skipped: 0 })).toEqual({
      message: "added to “Roadtrip”",
      variant: "success",
    });
  });

  it("counts a plain multi-track add", () => {
    expect(playlistAddMessage("Roadtrip", { added: 12, skipped: 0 })).toEqual({
      message: "added 12 tracks to “Roadtrip”",
      variant: "success",
    });
  });

  it("says 'already in' when the only track was a duplicate", () => {
    expect(playlistAddMessage("Roadtrip", { added: 0, skipped: 1 })).toEqual({
      message: "already in “Roadtrip”",
      variant: "info",
    });
  });

  it("says so when every track of a batch was already there", () => {
    expect(playlistAddMessage("Roadtrip", { added: 0, skipped: 9 })).toEqual({
      message: "all 9 tracks are already in “Roadtrip”",
      variant: "info",
    });
  });

  it("reports both halves of a partial add", () => {
    expect(playlistAddMessage("Roadtrip", { added: 7, skipped: 2 })).toEqual({
      message: "added 7 tracks to “Roadtrip” — 2 were already there",
      variant: "success",
    });
    expect(playlistAddMessage("Roadtrip", { added: 1, skipped: 1 })).toEqual({
      message: "added 1 track to “Roadtrip” — 1 was already there",
      variant: "success",
    });
  });

  it("degrades to 'nothing to add' on an empty write", () => {
    expect(playlistAddMessage("Roadtrip", { added: 0, skipped: 0 })).toEqual({
      message: "nothing to add to “Roadtrip”",
      variant: "info",
    });
  });
});
