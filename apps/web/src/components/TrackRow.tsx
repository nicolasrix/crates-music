// Single source of truth for "what a tracklist row looks like." Used by
// both TrackTable (small lists) and VirtualTrackTable (windowed). The
// outer <table> + <thead> still lives in each parent because the
// virtualized variant needs to wrap the body with padding rows; only
// the row itself is shared.

import { Play } from "lucide-react";
import { forwardRef } from "react";
import { Link } from "../router";
import { Cover } from "./Cover";
import { TrackRowMenu } from "./TrackRowMenu";
import { fmtDuration } from "../utils/format";
import type { Track } from "../api/types";

interface TrackRowProps {
  track: Track;
  /** Zero-based list index. Used for the click → play handler and as a
   *  fallback when the track itself has no track-number tag. */
  index: number;
  isPlaying: boolean;
  showAlbum: boolean;
  /** When true, render a 32px album cover thumbnail in front of the
   *  title. Defaults to `showAlbum` — the same "list spans multiple
   *  albums" signal — so consumers don't usually need to set it. */
  showCover?: boolean;
  onPlay: (index: number) => void;
  /** Forwarded to `<tr>` so the virtualizer can attach
   *  `measureElement` for accurate row-height tracking. */
  "data-index"?: number;
}

export const TrackRow = forwardRef<HTMLTableRowElement, TrackRowProps>(
  function TrackRow(
    { track, index, isPlaying, showAlbum, showCover, onPlay, ...rest },
    ref,
  ) {
    const t = track;
    const renderCover = showCover ?? showAlbum;
    return (
      <tr
        ref={ref}
        className={isPlaying ? "is-playing" : ""}
        onDoubleClick={() => onPlay(index)}
        {...rest}
      >
        <td
          className="col-num is-clickable"
          onClick={() => onPlay(index)}
          role="button"
          tabIndex={0}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              onPlay(index);
            }
          }}
          aria-label={`play ${t.title}`}
        >
          <span className="num-text tabular">{t.track ?? index + 1}</span>
          <span className="num-play">
            <Play size={14} fill="currentColor" strokeWidth={0} />
          </span>
          <span className="num-eq" aria-hidden>
            <span />
            <span />
            <span />
          </span>
        </td>
        {renderCover && (
          <td className="col-cover" onClick={() => onPlay(index)}>
            <div className="cover-thumb">
              <Cover
                coverArt={t.coverArt}
                seed={t.album ?? t.title}
                size={96}
                alt=""
              />
            </div>
          </td>
        )}
        <td className="col-title is-clickable" onClick={() => onPlay(index)}>
          {t.title}
        </td>
        <td className="col-artist">
          {t.artistId && t.artist ? (
            <Link to={`/artists/${t.artistId}`}>{t.artist}</Link>
          ) : (
            (t.artist ?? "—")
          )}
        </td>
        {showAlbum && (
          <td className="col-album">
            {t.albumId && t.album ? (
              <Link to={`/albums/${t.albumId}`}>{t.album}</Link>
            ) : (
              (t.album ?? "—")
            )}
          </td>
        )}
        <td className="col-time">{fmtDuration(t.duration)}</td>
        <td className="col-menu" onClick={(e) => e.stopPropagation()}>
          <TrackRowMenu track={t} />
        </td>
      </tr>
    );
  },
);
