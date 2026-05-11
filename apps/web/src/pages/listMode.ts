// Shared sort-mode for browse pages (Albums / Artists / Tracks). Each page
// renders one of these views; the parent route component picks the mode
// from the URL and passes it as a prop.
//
// "all" is the default for bare paths — /albums, /artists, /tracks all
// render their full library view. The sub-modes (/albums/recent, …) are
// the filtered/derived listings.

export type ListMode = "all" | "recent" | "most_played" | "random";

export const MODE_LABEL: Record<ListMode, string> = {
  all: "all",
  recent: "recently added",
  most_played: "most played",
  random: "random",
};

/** URL slug for each mode. The "all" mode has no slug — it lives at the
 *  bare section path (e.g. /albums) rather than /albums/all. */
export const MODE_SLUG: Record<ListMode, string> = {
  all: "",
  recent: "recent",
  most_played: "most-played",
  random: "random",
};

/** Subsonic getAlbumList2 type. The "frequent" type is the closest thing
 *  to "most played"; it sorts albums by play count. Will be effectively
 *  empty on a fresh install with no scrobbles, which is fine — the page
 *  shows an empty-state message in that case. */
export const MODE_ALBUM_TYPE: Record<ListMode, string> = {
  all: "alphabeticalByName",
  recent: "newest",
  most_played: "frequent",
  random: "random",
};

export function modeFromSlug(slug: string | undefined): ListMode {
  if (slug === "recent") return "recent";
  if (slug === "most-played") return "most_played";
  if (slug === "random") return "random";
  return "all";
}
