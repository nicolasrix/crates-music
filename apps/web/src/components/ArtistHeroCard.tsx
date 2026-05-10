// Featured artist card. Shares .search-hero with TrackHeroCard /
// AlbumHeroCard for visual consistency across the search page's three
// buckets. Whole card is a Link to the artist page — there's no
// "play artist" action wired up at this layer (that lives on the
// artist detail page itself).

import { Artist } from "../api/types";
import { Cover } from "./Cover";
import { Link } from "../router";

export function ArtistHeroCard({ artist }: { artist: Artist }) {
  return (
    <Link to={`/artists/${artist.id}`} className="search-hero">
      <div className="search-hero-cover is-circle">
        <Cover
          coverArt={artist.coverArt}
          seed={artist.name}
          size={200}
          alt={artist.name}
        />
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
