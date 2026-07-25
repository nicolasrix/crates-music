import { describe, expect, it } from "vitest";
import {
  DEFAULT_CACHE_SETTINGS,
  normalizeCacheSettings,
  qualityParams,
} from "./cacheSettings";

describe("downloadQuality normalization", () => {
  it("defaults when absent (pre-existing persisted blobs)", () => {
    const s = normalizeCacheSettings({ regularBudgetBytes: 1024 });
    expect(s.downloadQuality).toBe("original");
  });

  it("accepts known values", () => {
    expect(normalizeCacheSettings({ downloadQuality: "opus128" }).downloadQuality).toBe(
      "opus128",
    );
    expect(normalizeCacheSettings({ downloadQuality: "mp3128" }).downloadQuality).toBe(
      "mp3128",
    );
  });

  it("rejects junk back to the default", () => {
    expect(normalizeCacheSettings({ downloadQuality: "flac9000" }).downloadQuality).toBe(
      DEFAULT_CACHE_SETTINGS.downloadQuality,
    );
    expect(normalizeCacheSettings({ downloadQuality: 128 }).downloadQuality).toBe(
      DEFAULT_CACHE_SETTINGS.downloadQuality,
    );
  });
});

describe("qualityParams", () => {
  it("original means passthrough (no params)", () => {
    expect(qualityParams("original")).toBeNull();
  });

  it("maps transcode targets to Subsonic stream params", () => {
    expect(qualityParams("opus128")).toEqual({ format: "opus", maxBitRate: 128 });
    expect(qualityParams("mp3128")).toEqual({ format: "mp3", maxBitRate: 128 });
  });
});
