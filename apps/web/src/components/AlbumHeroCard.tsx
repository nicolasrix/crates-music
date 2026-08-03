// Featured album card. Shares .search-hero with TrackHeroCard /
// ArtistHeroCard. Click targets are split: title navigates to the
// album page; artist name navigates to the artist page when an
// artistId is known. The cover follows TrackHeroCard's convention
// when an onPlay handler is given — it becomes a play button — and
// degrades to a plain album-page link otherwise. We can't use a
// single outer <Link> wrapper (as ArtistHeroCard does) because
// nesting <a> inside <a> is invalid HTML — the inner artist link
// wouldn't be reachable.

import { Play } from "lucide-react";
import { Album } from "../api/types";
import { AlbumRowMenu } from "./AlbumRowMenu";
import { Cover } from "./Cover";
import { Link } from "../router";

export function AlbumHeroCard({
  album,
  onPlay,
}: {
  album: Album;
  /** Optional — if provided, the cover becomes a play button (with a
   *  hover/touch-revealed glyph) instead of navigating. */
  onPlay?: () => void;
}) {
  const albumHref = `/albums/${album.id}`;
  const cover = (
    <Cover
      coverArt={album.coverArt}
      seed={album.name}
      size={200}
      alt={album.name}
    />
  );
  return (
    <div className="search-hero">
      {onPlay ? (
        <button
          type="button"
          className="search-hero-cover"
          onClick={onPlay}
          aria-label={`play ${album.name}`}
        >
          {cover}
          <span className="search-hero-play" aria-hidden>
            <Play size={20} fill="currentColor" strokeWidth={0} />
          </span>
        </button>
      ) : (
        <Link to={albumHref} className="search-hero-cover" aria-label={album.name}>
          {cover}
        </Link>
      )}
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
      <div className="search-hero-menu">
        <AlbumRowMenu album={album} />
      </div>
    </div>
  );
}
