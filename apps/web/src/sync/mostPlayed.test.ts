import { describe, expect, it } from "vitest";
import { mostPlayedForArtist } from "./mostPlayed";
import type { Track } from "../api/types";

const ARTIST = { id: "art-1", name: "Mac Miller" };

function track(
  id: string,
  opts: { plays?: number; artistId?: string; artist?: string } = {},
): Track {
  return {
    id,
    title: id,
    ...(opts.plays === undefined ? {} : { playCount: opts.plays }),
    ...(opts.artistId === undefined ? {} : { artistId: opts.artistId }),
    ...(opts.artist === undefined ? {} : { artist: opts.artist }),
  };
}

/** A row by the page's artist, matched on id — the common case. */
function mine(id: string, plays?: number): Track {
  return track(id, {
    artistId: ARTIST.id,
    ...(plays === undefined ? {} : { plays }),
  });
}

describe("mostPlayedForArtist", () => {
  it("orders by play count, descending", () => {
    const out = mostPlayedForArtist(
      [mine("a", 3), mine("b", 90), mine("c", 12)],
      ARTIST,
      10,
    );
    expect(out.map((t) => t.id)).toEqual(["b", "c", "a"]);
  });

  it("drops tracks with no recorded plays", () => {
    // Navidrome omits playCount entirely at zero, so a missing field
    // and a never-played track are the same thing.
    const out = mostPlayedForArtist(
      [mine("played", 4), mine("never")],
      ARTIST,
      10,
    );
    expect(out.map((t) => t.id)).toEqual(["played"]);
  });

  it("drops an explicit zero as well as a missing field", () => {
    const out = mostPlayedForArtist(
      [mine("zero", 0), mine("one", 1)],
      ARTIST,
      10,
    );
    expect(out.map((t) => t.id)).toEqual(["one"]);
  });

  it("excludes other artists' tracks that the wide search swept in", () => {
    // searchArtistSongs queries by name, so the response contains rows
    // that merely mention the artist — those must not chart here.
    const out = mostPlayedForArtist(
      [
        mine("ours", 5),
        track("theirs", {
          plays: 999,
          artistId: "art-2",
          artist: "Someone Else",
        }),
      ],
      ARTIST,
      10,
    );
    expect(out.map((t) => t.id)).toEqual(["ours"]);
  });

  it("matches on name when the id differs, case-insensitively", () => {
    // Live libraries carry the display name "MAC MILLER" against tracks
    // tagged "Mac Miller"; an exact compare dropped ~16% of the catalog.
    const out = mostPlayedForArtist(
      [
        track("shouty", {
          plays: 7,
          artistId: "other-id",
          artist: "MAC MILLER",
        }),
      ],
      ARTIST,
      10,
    );
    expect(out.map((t) => t.id)).toEqual(["shouty"]);
  });

  it("caps at the limit, keeping the highest counts", () => {
    const out = mostPlayedForArtist(
      [mine("a", 1), mine("b", 5), mine("c", 3), mine("d", 9)],
      ARTIST,
      2,
    );
    expect(out.map((t) => t.id)).toEqual(["d", "b"]);
  });

  it("breaks ties on title so refetches don't reshuffle rows", () => {
    const out = mostPlayedForArtist(
      [mine("zeta", 4), mine("alpha", 4)],
      ARTIST,
      10,
    );
    expect(out.map((t) => t.id)).toEqual(["alpha", "zeta"]);
  });

  it("returns empty for an artist we have never played", () => {
    // Drives the section hiding itself rather than rendering an empty
    // table — the page's "no dead empty block" rule.
    expect(mostPlayedForArtist([mine("a"), mine("b", 0)], ARTIST, 10)).toEqual(
      [],
    );
  });

  it("does not mutate the input array", () => {
    // The input is a shared TanStack query result.
    const input = [mine("a", 1), mine("b", 9)];
    mostPlayedForArtist(input, ARTIST, 10);
    expect(input.map((t) => t.id)).toEqual(["a", "b"]);
  });

  it("handles an empty response", () => {
    expect(mostPlayedForArtist([], ARTIST, 5)).toEqual([]);
  });
});
