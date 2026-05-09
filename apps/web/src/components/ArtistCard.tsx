import { Artist } from "../api/types";
import { coverArtUrl } from "../api/client";
import { Link } from "../router";

export function ArtistCard({ artist }: { artist: Artist }) {
  const cover = coverArtUrl(artist.coverArt, 400);
  return (
    <Link to={`/artists/${artist.id}`} className="tile">
      <div className={`tile-cover is-circle ${cover ? "" : "is-placeholder"}`}>
        {cover && <img src={cover} alt={artist.name} loading="lazy" />}
      </div>
      <div className="tile-title">{artist.name}</div>
      {artist.albumCount != null && (
        <div className="tile-sub">{artist.albumCount} albums</div>
      )}
    </Link>
  );
}
