// Compact list of albums. Used in the /search overview to show ranked
// results below the hero strip — same visual style as TrackTable.
//
// Click semantics: the row navigates to /albums/:id; the artist cell
// navigates to /artists/:artistId via an inner <Link>. The artist
// cell stops propagation so clicking the artist name doesn't *also*
// trigger the row's album-navigate. When an onPlayAlbum handler is
// given, the cover thumb becomes a play button (hover/touch-revealed
// glyph, see .thumb-play) that also stops propagation.
//
// Leading column is a square album-cover thumbnail, matching the
// AlbumHeroCard convention so the hero strip and table view feel like
// two presentations of the same data. Trailing column is the ⋯ menu,
// matching TrackTable — queue / station / rating / offline actions
// without a trip to the album page.

import { Play } from "lucide-react";
import { Album } from "../api/types";
import { AlbumRowMenu } from "./AlbumRowMenu";
import { Cover } from "./Cover";
import { Link, navigate } from "../router";

interface Props {
  albums: readonly Album[];
  /** Optional — renders a play overlay on the cover thumb that plays
   *  the album in place instead of navigating. */
  onPlayAlbum?: (album: Album) => void;
}

export function AlbumTable({ albums, onPlayAlbum }: Props) {
  return (
    <table className="tracks">
      <thead>
        <tr>
          <th className="col-cover" aria-hidden />
          <th className="col-title">album</th>
          <th className="col-artist">artist</th>
          <th className="col-time">year</th>
          <th className="col-menu" aria-hidden />
        </tr>
      </thead>
      <tbody>
        {albums.map((a) => (
          <tr
            key={a.id}
            onClick={() => navigate(`/albums/${a.id}`)}
            role="link"
            tabIndex={0}
            onKeyDown={(e) => {
              // Only when the row itself is focused — Enter on the
              // nested play button must click it, not also navigate.
              if (e.key === "Enter" && e.target === e.currentTarget) {
                navigate(`/albums/${a.id}`);
              }
            }}
          >
            <td className="col-cover">
              {onPlayAlbum ? (
                <button
                  type="button"
                  className="cover-thumb"
                  onClick={(e) => {
                    e.stopPropagation();
                    onPlayAlbum(a);
                  }}
                  aria-label={`play ${a.name}`}
                >
                  <Cover coverArt={a.coverArt} seed={a.name} size={96} alt="" />
                  <span className="thumb-play" aria-hidden>
                    <Play size={14} fill="currentColor" strokeWidth={0} />
                  </span>
                </button>
              ) : (
                <div className="cover-thumb">
                  <Cover coverArt={a.coverArt} seed={a.name} size={96} alt="" />
                </div>
              )}
            </td>
            <td className="col-title">{a.name}</td>
            <td className="col-artist" onClick={(e) => e.stopPropagation()}>
              {a.artistId && a.artist ? (
                <Link to={`/artists/${a.artistId}`}>{a.artist}</Link>
              ) : (
                (a.artist ?? "—")
              )}
            </td>
            <td className="col-time">{a.year ?? "—"}</td>
            <td className="col-menu" onClick={(e) => e.stopPropagation()}>
              <AlbumRowMenu album={a} />
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}
