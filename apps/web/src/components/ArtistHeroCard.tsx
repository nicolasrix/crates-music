// Featured artist card. Shares .search-hero with TrackHeroCard /
// AlbumHeroCard for visual consistency across the search page's three
// buckets, and is reused for the similar-artists strip on /artists/:id.
//
// The card used to be a single <Link> wrapper. It can't be, now that it
// carries a ⋯ menu: a <button> inside an <a> is invalid HTML and the
// nested control wouldn't be reachable. Same constraint AlbumHeroCard
// documents — so this follows AlbumHeroCard's shape instead, with the
// cover and the title each their own link. Clicking the card's padding
// no longer navigates; the two real targets still do.

import { Artist } from "../api/types";
import { ArtistRowMenu } from "./ArtistRowMenu";
import { Cover } from "./Cover";
import { Link } from "../router";

export function ArtistHeroCard({ artist }: { artist: Artist }) {
  const href = `/artists/${artist.id}`;
  return (
    <div className="search-hero">
      <Link
        to={href}
        className="search-hero-cover is-circle"
        aria-label={artist.name}
      >
        <Cover
          coverArt={artist.coverArt}
          seed={artist.name}
          size={200}
          alt={artist.name}
        />
      </Link>
      <div className="search-hero-meta">
        <Link to={href} className="search-hero-title">
          {artist.name}
        </Link>
        {artist.albumCount != null && (
          <div className="search-hero-sub">
            {artist.albumCount}{" "}
            {artist.albumCount === 1 ? "album" : "albums"}
          </div>
        )}
      </div>
      <div className="search-hero-menu">
        <ArtistRowMenu artist={artist} />
      </div>
    </div>
  );
}
