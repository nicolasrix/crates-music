import { Album } from "../api/types";
import { coverArtUrl } from "../api/client";
import { Link } from "../router";

export function AlbumCard({ album }: { album: Album }) {
  const cover = coverArtUrl(album.coverArt);
  return (
    <Link
      to={`/albums/${album.id}`}
      className="block group"
    >
      <div className="aspect-square bg-stone-800 rounded overflow-hidden">
        {cover ? (
          <img src={cover} alt={album.name} className="w-full h-full object-cover" />
        ) : (
          <div className="w-full h-full flex items-center justify-center text-stone-600 text-xs">
            no cover
          </div>
        )}
      </div>
      <div className="mt-2 truncate font-medium group-hover:text-stone-300">
        {album.name}
      </div>
      <div className="truncate text-sm text-stone-400">{album.artist ?? "—"}</div>
    </Link>
  );
}
