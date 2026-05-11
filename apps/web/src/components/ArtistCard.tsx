import { Artist } from "../api/types";
import { Cover } from "./Cover";
import { Link } from "../router";

export function ArtistCard({ artist }: { artist: Artist }) {
  return (
    <Link to={`/artists/${artist.id}`} className="tile">
      <div className="tile-cover is-circle">
        <Cover
          coverArt={artist.coverArt}
          seed={artist.name}
          size={400}
          alt={artist.name}
        />
      </div>
      <div className="tile-title">{artist.name}</div>
      {artist.albumCount != null && (
        <div className="tile-sub">{artist.albumCount} albums</div>
      )}
    </Link>
  );
}
