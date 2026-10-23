import { describe, expect, it } from "vitest";
import { externalPlayingChange } from "./elementToggle";

describe("externalPlayingChange", () => {
  it("reports a pause the element made on its own (media key, OS control)", () => {
    expect(externalPlayingChange({ paused: true, ended: false }, true)).toBe(false);
  });

  it("reports a play the element made on its own", () => {
    expect(externalPlayingChange({ paused: false, ended: false }, false)).toBe(true);
  });

  it("ignores a stale pause event once the element is playing again", () => {
    // pause() then play() before the queued `pause` fires; the flag has
    // already moved back to true. Reporting it would re-pause the room.
    expect(externalPlayingChange({ paused: false, ended: false }, true)).toBeNull();
  });

  it("ignores a stale play event once the element is paused again", () => {
    expect(externalPlayingChange({ paused: true, ended: false }, false)).toBeNull();
  });

  it("leaves the pause that comes with a track ending to auto-advance", () => {
    expect(externalPlayingChange({ paused: true, ended: true }, true)).toBeNull();
  });
});
