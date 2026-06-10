import { afterEach, beforeEach, describe, expect, it } from "vitest";

import {
  DEFAULT_OUTPUT_ENABLED,
  loadOutputEnabled,
  saveOutputEnabled,
} from "./outputDevice";

// Minimal Map-backed localStorage so the test is env-independent (the default
// vitest `node` env has no localStorage). Mirrors only what the module uses.
function installFakeStorage(): Map<string, string> {
  const store = new Map<string, string>();
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    value: {
      getItem: (k: string) => (store.has(k) ? store.get(k)! : null),
      setItem: (k: string, v: string) => void store.set(k, v),
      removeItem: (k: string) => void store.delete(k),
    },
  });
  return store;
}

describe("outputDevice preference", () => {
  let store: Map<string, string>;

  beforeEach(() => {
    store = installFakeStorage();
  });

  afterEach(() => {
    delete (globalThis as { localStorage?: unknown }).localStorage;
  });

  it("defaults to enabled (play here) when nothing is stored", () => {
    expect(loadOutputEnabled()).toBe(DEFAULT_OUTPUT_ENABLED);
    expect(loadOutputEnabled()).toBe(true);
  });

  it("round-trips false (remote-only)", () => {
    saveOutputEnabled(false);
    expect(loadOutputEnabled()).toBe(false);
  });

  it("round-trips true (play here)", () => {
    saveOutputEnabled(false);
    saveOutputEnabled(true);
    expect(loadOutputEnabled()).toBe(true);
  });

  it("treats any non-\"true\" stored value as disabled", () => {
    store.set("crates-music.playback.outputEnabled", "garbage");
    expect(loadOutputEnabled()).toBe(false);
  });

  it("falls back to the default when localStorage throws", () => {
    Object.defineProperty(globalThis, "localStorage", {
      configurable: true,
      get() {
        throw new Error("blocked (private mode)");
      },
    });
    expect(loadOutputEnabled()).toBe(DEFAULT_OUTPUT_ENABLED);
    // saveOutputEnabled must swallow the failure rather than throw.
    expect(() => saveOutputEnabled(false)).not.toThrow();
  });
});
