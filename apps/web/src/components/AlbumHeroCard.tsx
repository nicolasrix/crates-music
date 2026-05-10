// Featured album card. Shares .search-hero with TrackHeroCard /
// ArtistHeroCard. Click targets are split: cover and title navigate
// to the album page; artist name navigates to the artist page when
// an artistId is known. We can't use a single outer <Link> wrapper
// (as ArtistHeroCard does) because nesting <a> inside <a> is invalid
// HTML — the inner artist link wouldn't be reachable.

import { Album } from "../api/types";
import { Cover } from "./Cover";
import { Link } from "../router";

export function AlbumHeroCard({ album }: { album: Album }) {
  const albumHref = `/albums/${album.id}`;
  return (
    <div className="search-hero">
      <Link
        to={albumHref}
        className="search-hero-cover"
        aria-label={album.name}
      >
        <Cover
          coverArt={album.coverArt}
          seed={album.name}
          size={200}
          alt={album.name}
        />
      </Link>
      <div className="search-hero-meta">
        <Link to={albumHref} className="search-hero-title">
          {album.name}
        </Link>
        <div className="search-hero-sub">
          {album.artistId && album.artist ? (
            <Link to={`/artists/${album.artistId}`}>{album.artist}</Link>
          ) : (
            (album.artist ?? "—")
          )}
          {album.year && (
            <>
              <span className="search-hero-sep" aria-hidden>·</span>
              {album.year}
            </>
          )}
        </div>
      </div>
    </div>
  );
}
