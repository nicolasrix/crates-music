// Shared tracklist. Used by Album, Playlist, and the all-Tracks view. Click
// the track-number cell (which swaps to a play glyph on hover) or the title
// to play that track. Double-click anywhere on the row also plays — kept for
// muscle-memory from desktop music players. The currently-playing row shows
// an animated 3-bar equalizer (.col-num .num-eq).
//
// Row rendering lives in <TrackRow> so this and VirtualTrackTable share a
// single source of truth for the visual.

import { usePlayer } from "../player/PlayerContext";
import { TrackRow } from "./TrackRow";
import type { Track } from "../api/types";

interface Props {
  tracks: Track[];
  showAlbum?: boolean;
  /** Optional explicit toggle for the cover column. Defaults to
   *  `showAlbum`: when the list spans multiple albums, the per-row
   *  cover is informative; when it doesn't (e.g. an album page), the
   *  same cover would repeat down every row, so it's hidden. */
  showCover?: boolean;
  onPlay: (index: number) => void;
}

export function TrackTable({ tracks, showAlbum = false, showCover, onPlay }: Props) {
  const { nowPlaying } = usePlayer();
  const playingId = nowPlaying?.id ?? null;
  const renderCover = showCover ?? showAlbum;

  return (
    <table className="tracks">
      <thead>
        <tr>
          <th className="col-num">#</th>
          {renderCover && <th className="col-cover" aria-hidden />}
          <th className="col-title">title</th>
          <th className="col-artist">artist</th>
          {showAlbum && <th className="col-album">album</th>}
          <th className="col-time">time</th>
          <th className="col-menu" aria-hidden />
        </tr>
      </thead>
      <tbody>
        {tracks.map((t, i) => (
          <TrackRow
            key={t.id}
            track={t}
            index={i}
            isPlaying={t.id === playingId}
            showAlbum={showAlbum}
            showCover={renderCover}
            onPlay={onPlay}
          />
        ))}
      </tbody>
    </table>
  );
}
