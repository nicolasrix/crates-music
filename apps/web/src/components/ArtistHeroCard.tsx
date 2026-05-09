// Featured artist card. Shares .search-hero with TrackHeroCard /
// AlbumHeroCard for visual consistency across the search page's three
// buckets. Whole card is a Link to the artist page — there's no
// "play artist" action wired up at this layer (that lives on the
// artist detail page itself).

import { Artist } from "../api/types";
import { coverArtUrl } from "../api/client";
import { Link } from "../router";

export function ArtistHeroCard({ artist }: { artist: Artist }) {
  const cover = coverArtUrl(artist.coverArt, 200);
  return (
    <Link to={`/artists/${artist.id}`} className="search-hero">
      <div
        className={`search-hero-cover is-circle ${cover ? "" : "is-placeholder"}`}
      >
        {cover && <img src={cover} alt={artist.name} loading="lazy" />}
      </div>
      <div className="search-hero-meta">
        <div className="search-hero-title">{artist.name}</div>
        {artist.albumCount != null && (
          <div className="search-hero-sub">
            {artist.albumCount}{" "}
            {artist.albumCount === 1 ? "album" : "albums"}
          </div>
        )}
      </div>
    </Link>
  );
}
