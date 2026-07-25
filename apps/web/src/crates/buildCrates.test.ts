import { describe, expect, it } from "vitest";
import type { Album } from "../api/types";
import {
  buildCrates,
  buildPlaylistCrates,
  RANDOM_CRATE_MIN,
  RANDOM_CRATE_SIZE,
} from "./buildCrates";

let nextId = 0;
function album(overrides: Partial<Album>): Album {
  nextId += 1;
  return { id: `al-${nextId}`, name: `Album ${nextId}`, ...overrides };
}

describe("buildCrates decade", () => {
  it("groups by decade in ascending order with undated last", () => {
    const crates = buildCrates(
      [
        album({ year: 1994 }),
        album({ year: 1971 }),
        album({ year: 2003 }),
        album({}),
        album({ year: 1978 }),
      ],
      "decade",
    );
    expect(crates.map((c) => c.label)).toEqual([
      "1970s",
      "1990s",
      "2000s",
      "undated",
    ]);
    expect(crates[0]!.albums).toHaveLength(2);
  });

  it("orders within a crate by year then artist then title", () => {
    const a = album({ year: 1977, artist: "Zeppelin", name: "Z" });
    const b = album({ year: 1971, artist: "Bowie", name: "B" });
    const c = album({ year: 1977, artist: "Abba", name: "A" });
    const crates = buildCrates([a, b, c], "decade");
    expect(crates[0]!.albums.map((x) => x.id)).toEqual([b.id, c.id, a.id]);
  });

  it("treats year 0 as undated", () => {
    const crates = buildCrates([album({ year: 0 })], "decade");
    expect(crates).toHaveLength(1);
    expect(crates[0]!.id).toBe("decade:undated");
  });
});

describe("buildCrates genre", () => {
  it("merges genre casing and orders crates by size", () => {
    const crates = buildCrates(
      [
        album({ genre: "Jazz" }),
        album({ genre: "jazz" }),
        album({ genre: "Rock" }),
        album({ genre: "Rock" }),
        album({ genre: "Rock" }),
      ],
      "genre",
    );
    expect(crates.map((c) => c.label)).toEqual(["rock", "jazz"]);
    expect(crates[1]!.id).toBe("genre:jazz");
    expect(crates[1]!.albums).toHaveLength(2);
  });

  it("pools tiny genres and untagged albums into misc, placed last", () => {
    const crates = buildCrates(
      [
        album({ genre: "Jazz" }),
        album({ genre: "Jazz" }),
        album({ genre: "Vaporwave" }), // below MIN_GENRE_ALBUMS
        album({}), // untagged
        album({ genre: "  " }), // whitespace-only counts as untagged
      ],
      "genre",
    );
    expect(crates.map((c) => c.id)).toEqual(["genre:jazz", "genre:misc"]);
    expect(crates[1]!.albums).toHaveLength(3);
  });
});

describe("buildCrates artist", () => {
  it("gives prolific artists their own crate, alphabetical, rest pooled", () => {
    const crates = buildCrates(
      [
        album({ artistId: "ar-z", artist: "Zappa", year: 1970 }),
        album({ artistId: "ar-z", artist: "Zappa", year: 1972 }),
        album({ artistId: "ar-z", artist: "Zappa", year: 1971 }),
        album({ artistId: "ar-a", artist: "Abba", year: 1975 }),
        album({ artistId: "ar-a", artist: "Abba", year: 1976 }),
        album({ artistId: "ar-a", artist: "Abba", year: 1977 }),
        album({ artistId: "ar-b", artist: "Beck" }), // single album → pooled
      ],
      "artist",
    );
    expect(crates.map((c) => c.label)).toEqual(["Abba", "Zappa", "odds & ends"]);
    // chronological within the artist crate
    expect(crates[1]!.albums.map((a) => a.year)).toEqual([1970, 1971, 1972]);
  });

  it("falls back to artist name when artistId is missing", () => {
    const crates = buildCrates(
      [
        album({ artist: "Various", year: 1999 }),
        album({ artist: "Various", year: 2001 }),
        album({ artist: "Various", year: 2000 }),
      ],
      "artist",
    );
    expect(crates).toHaveLength(1);
    expect(crates[0]!.id).toBe("artist:Various");
  });

  it("albums with no artist at all land in odds & ends", () => {
    const crates = buildCrates([album({})], "artist");
    expect(crates).toHaveLength(1);
    expect(crates[0]!.id).toBe("artist:odds");
  });
});

describe("buildCrates random", () => {
  it("shuffles deterministically for a given seed and keeps every album", () => {
    const albums = Array.from({ length: 60 }, () => album({}));
    const a = buildCrates(albums, "random", 42);
    const b = buildCrates(albums, "random", 42);
    expect(a.map((c) => c.albums.map((x) => x.id))).toEqual(
      b.map((c) => c.albums.map((x) => x.id)),
    );
    const ids = a.flatMap((c) => c.albums.map((x) => x.id)).sort();
    expect(ids).toEqual(albums.map((x) => x.id).sort());
  });

  it("different seeds give different shuffles", () => {
    const albums = Array.from({ length: 60 }, () => album({}));
    const a = buildCrates(albums, "random", 1).flatMap((c) =>
      c.albums.map((x) => x.id),
    );
    const b = buildCrates(albums, "random", 2).flatMap((c) =>
      c.albums.map((x) => x.id),
    );
    expect(a).not.toEqual(b);
  });

  it("folds a tiny trailing crate into the previous one", () => {
    const count = RANDOM_CRATE_SIZE + RANDOM_CRATE_MIN - 1;
    const crates = buildCrates(
      Array.from({ length: count }, () => album({})),
      "random",
      7,
    );
    expect(crates).toHaveLength(1);
    expect(crates[0]!.albums).toHaveLength(count);
  });

  it("does not mutate the input array", () => {
    const albums = Array.from({ length: 30 }, () => album({}));
    const before = albums.map((x) => x.id);
    buildCrates(albums, "random", 3);
    expect(albums.map((x) => x.id)).toEqual(before);
  });
});

describe("buildPlaylistCrates", () => {
  it("builds one crate per playlist with unique albums in playlist order", () => {
    const a = album({});
    const b = album({});
    const c = album({});
    const crates = buildPlaylistCrates(
      [
        { id: "pl-1", name: "Roadtrip", albumIds: [b.id, a.id, b.id, c.id] },
        { id: "pl-2", name: "Focus", albumIds: [c.id] },
      ],
      [a, b, c],
    );
    expect(crates.map((x) => x.label)).toEqual(["Roadtrip", "Focus"]);
    expect(crates[0]!.id).toBe("playlist:pl-1");
    expect(crates[0]!.albums.map((x) => x.id)).toEqual([b.id, a.id, c.id]);
  });

  it("drops unknown album ids and skips playlists that resolve to nothing", () => {
    const a = album({});
    const crates = buildPlaylistCrates(
      [
        { id: "pl-1", name: "Ghost", albumIds: ["gone-1", "gone-2"] },
        { id: "pl-2", name: "Alive", albumIds: ["gone-1", a.id] },
      ],
      [a],
    );
    expect(crates).toHaveLength(1);
    expect(crates[0]!.id).toBe("playlist:pl-2");
    expect(crates[0]!.albums.map((x) => x.id)).toEqual([a.id]);
  });
});
