import { Play } from "lucide-react";
import { Album } from "../api/types";
import { coverArtUrl } from "../api/client";
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
  const cover = coverArtUrl(album.coverArt, 400);

  return (
    <Link to={`/albums/${album.id}`} className="tile">
      <div className="tile-cover">
        {cover ? (
          <img src={cover} alt={album.name} loading="lazy" />
        ) : (
          <div className="w-full h-full flex items-center justify-center text-fg-faint text-xs">
            no cover
          </div>
        )}
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
