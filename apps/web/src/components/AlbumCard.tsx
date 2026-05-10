import { Play } from "lucide-react";
import { Album } from "../api/types";
import { Cover } from "./Cover";
import { Link, navigate } from "../router";

export function AlbumCard({
  album,
  onPlay,
}: {
  album: Album;
  /** Optional — if provided, the floating play overlay button calls this
   *  instead of navigating to the album page. */
  onPlay?: () => void;
}) {
  return (
    <Link to={`/albums/${album.id}`} className="tile">
      <div className="tile-cover">
        <Cover
          coverArt={album.coverArt}
          seed={album.name}
          size={400}
          alt={album.name}
        />
        <button
          className="tile-play"
          aria-label={`play ${album.name}`}
          onClick={(e) => {
            // Prevent the parent <Link> from also navigating.
            e.preventDefault();
            e.stopPropagation();
            if (onPlay) onPlay();
            else navigate(`/albums/${album.id}`);
          }}
        >
          <Play size={18} fill="currentColor" strokeWidth={0} />
        </button>
      </div>
      <div className="tile-title">{album.name}</div>
      <div className="tile-sub">{album.artist ?? "—"}</div>
    </Link>
  );
}
