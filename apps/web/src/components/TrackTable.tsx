// Shared tracklist. Used by Album, Playlist, and the all-Tracks view. Click
// the track-number cell (which swaps to a play glyph on hover) or the title
// to play that track. Double-click anywhere on the row also plays — kept for
// muscle-memory from desktop music players. The currently-playing row shows
// an animated 3-bar equalizer (.col-num .num-eq).

import { Play } from "lucide-react";
import { Link } from "../router";
import { fmtDuration } from "../utils/format";
import { usePlayer } from "../player/PlayerContext";
import { TrackRowMenu } from "./TrackRowMenu";
import type { Track } from "../api/types";

interface Props {
  tracks: Track[];
  showAlbum?: boolean;
  onPlay: (index: number) => void;
}

export function TrackTable({ tracks, showAlbum = false, onPlay }: Props) {
  const { nowPlaying } = usePlayer();
  const playingId = nowPlaying?.id ?? null;

  return (
    <table className="tracks">
      <thead>
        <tr>
          <th className="col-num">#</th>
          <th className="col-title">title</th>
          <th className="col-artist">artist</th>
          {showAlbum && <th className="col-album">album</th>}
          <th className="col-time">time</th>
          <th className="col-menu" aria-hidden />
        </tr>
      </thead>
      <tbody>
        {tracks.map((t, i) => {
          const isPlaying = t.id === playingId;
          return (
            <tr
              key={t.id}
              className={isPlaying ? "is-playing" : ""}
              onDoubleClick={() => onPlay(i)}
            >
              <td
                className="col-num is-clickable"
                onClick={() => onPlay(i)}
                role="button"
                tabIndex={0}
                onKeyDown={(e) => {
                  if (e.key === "Enter" || e.key === " ") {
                    e.preventDefault();
                    onPlay(i);
                  }
                }}
                aria-label={`play ${t.title}`}
              >
                <span className="num-text tabular">{t.track ?? i + 1}</span>
                <span className="num-play">
                  <Play size={14} fill="currentColor" strokeWidth={0} />
                </span>
                <span className="num-eq" aria-hidden>
                  <span />
                  <span />
                  <span />
                </span>
              </td>
              <td className="col-title is-clickable" onClick={() => onPlay(i)}>
                {t.title}
              </td>
              <td className="col-artist">{t.artist ?? "—"}</td>
              {showAlbum && (
                <td className="col-album">
                  {t.albumId && t.album ? (
                    <Link to={`/albums/${t.albumId}`}>{t.album}</Link>
                  ) : (
                    t.album ?? "—"
                  )}
                </td>
              )}
              <td className="col-time">{fmtDuration(t.duration)}</td>
              <td className="col-menu" onClick={(e) => e.stopPropagation()}>
                <TrackRowMenu track={t} />
              </td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}
