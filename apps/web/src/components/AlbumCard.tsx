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
  const albumHref = `/albums/${album.id}`;
  // Outer wrapper is a <div>, not a <Link>, so the artist sub-link can
  // be a real anchor. Same split-click-target pattern as AlbumHeroCard
  // — nesting <a> inside <a> is invalid HTML and the inner artist link
  // wouldn't be reachable.
  return (
    <div className="tile">
      {/*
        SPEC NOTE: <button> nested inside <a> is invalid HTML — the
        spec's "interactive content" rule forbids interactive
        descendants of <a> (or <button>). We do it anyway because the
        cover-as-link + play-overlay pattern is the standard music-app
        affordance, and the spec-compliant alternatives are worse:
        a sibling-positioned button loses the "click anywhere on the
        cover navigates" area; making the cover itself a <button>
        loses middle-/right-click "open in new tab", which is the
        whole reason it's an anchor.

        Three mitigations keep this safe in practice; do NOT remove
        any of them when refactoring:
          1. e.preventDefault() + e.stopPropagation() in onClick —
             without these, clicking play fires the play handler AND
             navigates the wrapping <a> to the album page.
          2. type="button" defuses the implicit type="submit" gotcha
             if this card ever renders inside a <form>.
          3. aria-label on both <Link> and <button> gives screen
             readers an unambiguous announcement regardless of how
             the nested roles are interpreted.
      */}
      <Link to={albumHref} className="tile-cover" aria-label={album.name}>
        <Cover
          coverArt={album.coverArt}
          seed={album.name}
          size={400}
          alt={album.name}
        />
        <button
          type="button"
          className="tile-play"
          aria-label={`play ${album.name}`}
          onClick={(e) => {
            e.preventDefault();
            e.stopPropagation();
            if (onPlay) onPlay();
            else navigate(albumHref);
          }}
        >
          <Play size={18} fill="currentColor" strokeWidth={0} />
        </button>
      </Link>
      <Link to={albumHref} className="tile-title">
        {album.name}
      </Link>
      <div className="tile-sub">
        {album.artistId && album.artist ? (
          <Link to={`/artists/${album.artistId}`}>{album.artist}</Link>
        ) : (
          (album.artist ?? "—")
        )}
      </div>
    </div>
  );
}
