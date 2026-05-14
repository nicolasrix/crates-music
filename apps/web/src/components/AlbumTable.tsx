// Compact list of albums. Used in the /search overview to show ranked
// results below the hero strip — same visual style as TrackTable.
//
// Click semantics: the row navigates to /albums/:id; the artist cell
// navigates to /artists/:artistId via an inner <Link>. The artist
// cell stops propagation so clicking the artist name doesn't *also*
// trigger the row's album-navigate.
//
// Leading column is a square album-cover thumbnail, matching the
// AlbumHeroCard convention so the hero strip and table view feel like
// two presentations of the same data.

import { Album } from "../api/types";
import { Cover } from "./Cover";
import { Link, navigate } from "../router";

interface Props {
  albums: readonly Album[];
}

export function AlbumTable({ albums }: Props) {
  return (
    <table className="tracks">
      <thead>
        <tr>
          <th className="col-cover" aria-hidden />
          <th className="col-title">album</th>
          <th className="col-artist">artist</th>
          <th className="col-time">year</th>
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
              if (e.key === "Enter") navigate(`/albums/${a.id}`);
            }}
          >
            <td className="col-cover">
              <div className="cover-thumb">
                <Cover
                  coverArt={a.coverArt}
                  seed={a.name}
                  size={96}
                  alt=""
                />
              </div>
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
          </tr>
        ))}
      </tbody>
    </table>
  );
}
