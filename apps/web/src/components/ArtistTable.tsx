// Compact list of artists. Used in the /search overview to show ranked
// results below the hero strip — same visual style as TrackTable
// (reuses the `.tracks` CSS class) so the three search sections feel
// like siblings.
//
// Each row is a click target navigating to /artists/:id. We don't
// embed nested <Link>s; the row's onClick is the canonical navigation
// path, which keeps the markup flat and the keyboard tab order short
// (the ⋯ menu is the one other stop).
//
// The leading column is a circular artist photo, mirroring the
// ArtistHeroCard convention so the hero strip and this list look like
// two views onto the same data. The trailing ⋯ column matches
// TrackTable / AlbumTable.

import { Artist } from "../api/types";
import { ArtistRowMenu } from "./ArtistRowMenu";
import { Cover } from "./Cover";
import { navigate } from "../router";

interface Props {
  artists: readonly Artist[];
}

export function ArtistTable({ artists }: Props) {
  return (
    <table className="tracks">
      <thead>
        <tr>
          <th className="col-cover" aria-hidden />
          <th className="col-title">artist</th>
          <th className="col-time">albums</th>
          <th className="col-menu" aria-hidden />
        </tr>
      </thead>
      <tbody>
        {artists.map((a) => (
          <tr
            key={a.id}
            onClick={() => navigate(`/artists/${a.id}`)}
            role="link"
            tabIndex={0}
            onKeyDown={(e) => {
              // Only when the row itself is focused — Enter on the nested
              // ⋯ trigger must open the menu, not also navigate away.
              if (e.key === "Enter" && e.target === e.currentTarget) {
                navigate(`/artists/${a.id}`);
              }
            }}
          >
            <td className="col-cover">
              <div className="cover-thumb is-circle">
                <Cover
                  coverArt={a.coverArt}
                  seed={a.name}
                  size={96}
                  alt=""
                />
              </div>
            </td>
            <td className="col-title">{a.name}</td>
            <td className="col-time">{a.albumCount ?? "—"}</td>
            <td className="col-menu" onClick={(e) => e.stopPropagation()}>
              <ArtistRowMenu artist={a} />
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}
