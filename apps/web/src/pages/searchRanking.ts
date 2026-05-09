// Client-side relevance ranking + artist derivation for the /search page.
//
// Why this exists: Subsonic's `search3` returns three independent buckets
// (artists / albums / tracks) in whatever order Navidrome's index produced
// them — there's no relevance score, and the artist bucket only matches
// tokens against artist names. That's why "take the a" finds the two
// "Take the A Train" albums but not Duke Ellington (his name doesn't
// contain any of the query tokens, even though *every* hit is by him).
//
// Two passes:
//   1. **Derive** artists from the track + album results. Anyone who
//      shows up in the result set, weighted by how many times they
//      appear, is a candidate even if `search3` didn't return them
//      under `artist`.
//   2. **Score** every item with a simple match ladder:
//        exact field match     → 1000
//        field starts-with     →  500
//        all tokens word-prefix→  200
//        each token word-prefix→   50 each
//      Per-field weights bias the score (track title outweighs album
//      title outweighs artist name, etc).
//
// Word-prefix matching (rather than substring) is what makes single-char
// tokens like "a" useful: `a` matches the word "a" in "take the a train"
// but not "satin" or "ellington". Substring matching would treat "a" as
// noise present in nearly every word.

import type { Album, Artist, Track } from "../api/types";

// Lowercase, strip diacritics, drop punctuation, collapse whitespace.
// "Café (Deluxe Edition)" → "cafe deluxe edition"
function normalize(s: string): string {
  return s
    .normalize("NFD")
    .replace(/[̀-ͯ]/g, "")
    .toLowerCase()
    .replace(/[^\p{L}\p{N}\s]/gu, " ")
    .replace(/\s+/g, " ")
    .trim();
}

function tokenize(s: string): string[] {
  return normalize(s).split(" ").filter(Boolean);
}

// Return the highest applicable bonus when matching `field` against a
// pre-normalized `query` and its `tokens`. Caller multiplies by a
// per-field weight.
function fieldMatchBonus(
  field: string | undefined,
  query: string,
  tokens: string[]
): number {
  if (!field) return 0;
  const f = normalize(field);
  if (f === query) return 1000;
  if (f.startsWith(query)) return 500;
  const words = f.split(" ");
  // "every token matches some word as a prefix" is the "all-tokens-hit"
  // signal — strong enough to outrank single-token hits regardless of
  // how many of those there are.
  const everyHits = tokens.every((t) => words.some((w) => w.startsWith(t)));
  if (everyHits) return 200;
  let any = 0;
  for (const t of tokens) {
    if (words.some((w) => w.startsWith(t))) any += 50;
  }
  return any;
}

function scoreTrack(t: Track, query: string, tokens: string[]): number {
  return (
    fieldMatchBonus(t.title, query, tokens) * 1.0 +
    fieldMatchBonus(t.artist, query, tokens) * 0.4 +
    fieldMatchBonus(t.album, query, tokens) * 0.3
  );
}

function scoreAlbum(a: Album, query: string, tokens: string[]): number {
  return (
    fieldMatchBonus(a.name, query, tokens) * 1.0 +
    fieldMatchBonus(a.artist, query, tokens) * 0.4
  );
}

function scoreArtist(
  a: Artist,
  query: string,
  tokens: string[],
  derivedHits: number
): number {
  // Each track/album by this artist in the result set adds a small bump.
  // 10 per hit means: ~50 hits ≈ a starts-with name match. Calibrated so
  // that a clearly-relevant derived artist outranks a weak name match
  // ("the" matching the article in "The Knife" when the query is
  // "duke") without overpowering a real name match.
  return fieldMatchBonus(a.name, query, tokens) * 1.0 + derivedHits * 10;
}

export interface RankedResults {
  artists: Artist[];
  albums: Album[];
  tracks: Track[];
}

export interface RawResults {
  artists: Artist[];
  albums: Album[];
  tracks: Track[];
}

// Optional canonical artist registry (typically the cached
// /rest/getArtists list). When provided, both search3 artists and
// derived artists are hydrated from it — this is how we recover
// `albumCount` (and any other fields) for artists that came in via
// the derivation pass and would otherwise be metadata-bare.
export interface RankOptions {
  knownArtists?: readonly Artist[];
}

export function rankResults(
  raw: RawResults,
  query: string,
  opts: RankOptions = {}
): RankedResults {
  const q = normalize(query);
  const tokens = tokenize(query);

  const knownById = new Map<string, Artist>();
  if (opts.knownArtists) {
    for (const a of opts.knownArtists) knownById.set(a.id, a);
  }

  // Pass 1 — count artist appearances across track + album results, and
  // remember a coverArt fallback for artists who weren't returned by
  // search3 (so the hero card has a cover instead of a placeholder
  // circle). Subsonic cover IDs are polymorphic — passing an album
  // cover ID to /rest/getCoverArt returns the album cover whether the
  // parent record is artist or album.
  const derived = new Map<
    string,
    { name: string; hits: number; coverArt?: string }
  >();
  const bump = (
    artistId: string | undefined,
    artistName: string | undefined,
    coverArt: string | undefined
  ) => {
    if (!artistId || !artistName) return;
    let e = derived.get(artistId);
    if (!e) {
      e = { name: artistName, hits: 0 };
      if (coverArt) e.coverArt = coverArt;
    }
    e.hits += 1;
    if (!e.coverArt && coverArt) e.coverArt = coverArt;
    derived.set(artistId, e);
  };
  for (const t of raw.tracks) bump(t.artistId, t.artist, t.coverArt);
  for (const a of raw.albums) bump(a.artistId, a.artist, a.coverArt);

  // Pass 2 — merge raw artists with derived, hydrating from
  // knownArtists where available. The hydration matters for derived
  // artists especially: search3's artist bucket returns full records
  // (with albumCount), but derived ones have only id + name + a
  // borrowed coverArt unless we look them up against the canonical
  // /rest/getArtists list.
  const artistById = new Map<string, Artist>();
  for (const a of raw.artists) {
    const known = knownById.get(a.id);
    // {...known, ...a} keeps fields a doesn't define (e.g. albumCount
    // when search3 omits it for some reason) while letting search3's
    // record override anything it does define.
    artistById.set(a.id, known ? { ...known, ...a } : a);
  }
  for (const [id, d] of derived) {
    if (!artistById.has(id)) {
      const known = knownById.get(id);
      const base: Artist = known ? { ...known } : { id, name: d.name };
      if (!base.coverArt && d.coverArt) base.coverArt = d.coverArt;
      artistById.set(id, base);
    }
  }

  // Pass 3 — score and sort. Array.prototype.sort is stable per ES2019,
  // so equal-score items preserve search3's original order.
  const artists = [...artistById.values()]
    .map((a) => ({
      item: a,
      score: scoreArtist(a, q, tokens, derived.get(a.id)?.hits ?? 0),
    }))
    .filter((s) => s.score > 0)
    .sort((a, b) => b.score - a.score)
    .map((s) => s.item);

  const albums = raw.albums
    .map((a) => ({ item: a, score: scoreAlbum(a, q, tokens) }))
    .filter((s) => s.score > 0)
    .sort((a, b) => b.score - a.score)
    .map((s) => s.item);

  const tracks = raw.tracks
    .map((t) => ({ item: t, score: scoreTrack(t, q, tokens) }))
    .filter((s) => s.score > 0)
    .sort((a, b) => b.score - a.score)
    .map((s) => s.item);

  return { artists, albums, tracks };
}
