// Crate digging — grouping logic. Pure functions: Album[] in, Crate[] out.
// A "crate" is a named, ordered slice of the library, visualised as a box
// of LPs the user flips through one by one (see pages/Crates.tsx).
//
// Five sort axes. decade/genre/artist/random derive client-side from the
// one cached listAllAlbums() result; playlist additionally needs each
// playlist's track→album mapping (fetched lazily by the page, shaped via
// buildPlaylistCrates):
//   decade   — by release year, "1970s" … "2020s", undated last
//   genre    — by the album-level genre tag, tiny genres pooled into "misc"
//   artist   — one crate per artist with a real discography, the rest
//              pooled into "odds & ends" (real stores have that crate too)
//   playlist — one crate per playlist, albums in playlist order
//   random   — the library shuffled into fixed-size "grab bag" crates

import type { Album } from "../api/types";

export type CrateSort = "decade" | "genre" | "artist" | "playlist" | "random";

export interface Crate {
  /** Stable key across rebuilds, e.g. "decade:1970", "genre:jazz". */
  id: string;
  /** The masking-tape label on the crate front. */
  label: string;
  albums: Album[];
}

export const CRATE_SORTS: readonly CrateSort[] = [
  "decade",
  "genre",
  "artist",
  "playlist",
  "random",
];

/** Genres with fewer albums than this are pooled into a "misc" crate —
 *  a one-record crate isn't diggable. */
export const MIN_GENRE_ALBUMS = 2;
/** Artists need at least this many albums to earn their own crate. */
export const MIN_ARTIST_ALBUMS = 3;
/** Target size of a random "grab bag" crate. */
export const RANDOM_CRATE_SIZE = 24;
/** A trailing random crate smaller than this folds into the previous one. */
export const RANDOM_CRATE_MIN = 8;

export function buildCrates(
  albums: Album[],
  sort: CrateSort,
  randomSeed = 1,
): Crate[] {
  switch (sort) {
    case "decade":
      return byDecade(albums);
    case "genre":
      return byGenre(albums);
    case "artist":
      return byArtist(albums);
    case "random":
      return byRandom(albums, randomSeed);
    case "playlist":
      // Needs per-playlist track data the page fetches separately —
      // see buildPlaylistCrates.
      return [];
  }
}

/** One album list per playlist, already reduced to album ids in playlist
 *  order (the page maps tracks → albumIds). */
export interface PlaylistAlbums {
  id: string;
  name: string;
  albumIds: string[];
}

/** One crate per playlist: unique albums in first-appearance order,
 *  resolved against the catalog. Playlists whose tracks resolve to no
 *  known album (deleted/empty) get no crate. */
export function buildPlaylistCrates(
  playlists: PlaylistAlbums[],
  albums: Album[],
): Crate[] {
  const byId = new Map(albums.map((a) => [a.id, a]));
  return playlists.flatMap((p) => {
    const seen = new Set<string>();
    const list: Album[] = [];
    for (const albumId of p.albumIds) {
      if (seen.has(albumId)) continue;
      seen.add(albumId);
      const album = byId.get(albumId);
      if (album) list.push(album);
    }
    if (list.length === 0) return [];
    return [{ id: `playlist:${p.id}`, label: p.name, albums: list }];
  });
}

// Seeded PRNG (mulberry32) so a given shuffle is stable across re-renders;
// the page owns the seed and regenerates it to reshuffle.
function mulberry32(seed: number): () => number {
  let s = seed >>> 0;
  return () => {
    s = (s + 0x6d2b79f5) >>> 0;
    let t = s;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function byRandom(albums: Album[], seed: number): Crate[] {
  const rand = mulberry32(seed);
  // Fisher–Yates over a copy — input stays untouched.
  const shuffled = [...albums];
  for (let i = shuffled.length - 1; i > 0; i--) {
    const j = Math.floor(rand() * (i + 1));
    [shuffled[i], shuffled[j]] = [shuffled[j]!, shuffled[i]!];
  }
  const crates: Crate[] = [];
  for (let start = 0; start < shuffled.length; start += RANDOM_CRATE_SIZE) {
    crates.push({
      id: `random:${seed}:${crates.length}`,
      label: `grab bag ${crates.length + 1}`,
      albums: shuffled.slice(start, start + RANDOM_CRATE_SIZE),
    });
  }
  // A two-record tail crate isn't diggable — fold it into the previous one.
  const last = crates[crates.length - 1];
  const prev = crates[crates.length - 2];
  if (last && prev && last.albums.length < RANDOM_CRATE_MIN) {
    return [
      ...crates.slice(0, -2),
      { ...prev, albums: [...prev.albums, ...last.albums] },
    ];
  }
  return crates;
}

// Within-crate order: oldest first, then artist, then title — flipping
// through a crate front-to-back reads as a chronology.
function sortWithinCrate(albums: Album[]): Album[] {
  return [...albums].sort(
    (a, b) =>
      (a.year ?? Infinity) - (b.year ?? Infinity) ||
      (a.artist ?? "").localeCompare(b.artist ?? "") ||
      a.name.localeCompare(b.name),
  );
}

function byDecade(albums: Album[]): Crate[] {
  const buckets = new Map<number, Album[]>();
  const undated: Album[] = [];
  for (const album of albums) {
    if (album.year == null || album.year <= 0) {
      undated.push(album);
      continue;
    }
    const decade = Math.floor(album.year / 10) * 10;
    buckets.set(decade, [...(buckets.get(decade) ?? []), album]);
  }
  const crates = [...buckets.entries()]
    .sort(([a], [b]) => a - b)
    .map(([decade, list]) => ({
      id: `decade:${decade}`,
      label: `${decade}s`,
      albums: sortWithinCrate(list),
    }));
  if (undated.length > 0) {
    crates.push({
      id: "decade:undated",
      label: "undated",
      albums: sortWithinCrate(undated),
    });
  }
  return crates;
}

function byGenre(albums: Album[]): Crate[] {
  // Group case-insensitively but label with the first-seen casing —
  // "Hip-Hop" and "hip-hop" are one crate.
  const buckets = new Map<string, { label: string; albums: Album[] }>();
  const untagged: Album[] = [];
  for (const album of albums) {
    const raw = album.genre?.trim();
    if (!raw) {
      untagged.push(album);
      continue;
    }
    const key = raw.toLowerCase();
    const bucket = buckets.get(key);
    if (bucket) {
      buckets.set(key, { ...bucket, albums: [...bucket.albums, album] });
    } else {
      buckets.set(key, { label: raw, albums: [album] });
    }
  }

  const misc: Album[] = [...untagged];
  const crates: Crate[] = [];
  for (const [key, { label, albums: list }] of buckets) {
    if (list.length < MIN_GENRE_ALBUMS) {
      misc.push(...list);
    } else {
      crates.push({
        id: `genre:${key}`,
        label: label.toLowerCase(),
        albums: sortWithinCrate(list),
      });
    }
  }
  // Biggest genres first — the crates you'd put at the front of the table.
  crates.sort(
    (a, b) => b.albums.length - a.albums.length || a.label.localeCompare(b.label),
  );
  if (misc.length > 0) {
    crates.push({
      id: "genre:misc",
      label: "misc",
      albums: sortWithinCrate(misc),
    });
  }
  return crates;
}

function byArtist(albums: Album[]): Crate[] {
  // Key by artistId when present; name is the fallback for odd rows
  // (compilations sometimes carry a name but no id).
  const buckets = new Map<string, { label: string; albums: Album[] }>();
  const stray: Album[] = [];
  for (const album of albums) {
    const key = album.artistId ?? album.artist;
    if (!key) {
      stray.push(album);
      continue;
    }
    const bucket = buckets.get(key);
    if (bucket) {
      buckets.set(key, { ...bucket, albums: [...bucket.albums, album] });
    } else {
      buckets.set(key, { label: album.artist ?? key, albums: [album] });
    }
  }

  const odds: Album[] = [...stray];
  const crates: Crate[] = [];
  for (const [key, { label, albums: list }] of buckets) {
    if (list.length < MIN_ARTIST_ALBUMS) {
      odds.push(...list);
    } else {
      crates.push({
        id: `artist:${key}`,
        label,
        albums: sortWithinCrate(list),
      });
    }
  }
  crates.sort((a, b) => a.label.localeCompare(b.label));
  if (odds.length > 0) {
    crates.push({
      id: "artist:odds",
      label: "odds & ends",
      albums: sortWithinCrate(odds),
    });
  }
  return crates;
}
