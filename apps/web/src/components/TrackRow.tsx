// Single source of truth for "what a tracklist row looks like." Used by
// both TrackTable (small lists) and VirtualTrackTable (windowed). The
// outer <table> + <thead> still lives in each parent because the
// virtualized variant needs to wrap the body with padding rows; only
// the row itself is shared.

import { Play } from "lucide-react";
import { forwardRef, useState } from "react";
import { Link } from "../router";
import { Cover } from "./Cover";
import { TrackRowMenu } from "./TrackRowMenu";
import { beginTrackDrag, usePointerFine } from "../dnd/trackDrag";
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
    // Desktop-only: let mouse users drag a row onto a sidebar playlist. Off on
    // touch (see dnd/trackDrag) so it never fights list scrolling; the row
    // menu's "add to playlist" remains the universal path.
    const pointerFine = usePointerFine();
    // While this row is the drag source, fade it so the user sees which
    // track left the list (the pill under the cursor is the copy). Cleared
    // on dragend whether the drop landed or was cancelled.
    const [dragging, setDragging] = useState(false);
    // The "#" cell shows the row's ordinal. In a single-album list
    // (showAlbum=false, i.e. the album page) the album track-number tag
    // is that ordinal. In a multi-album list (playlist, all-tracks,
    // search, liked, downloads, station) the album tag is meaningless as
    // a position, so number by the row's place in *this* list instead —
    // same "spans multiple albums" signal that drives showCover.
    const displayNum = showAlbum ? index + 1 : (t.track ?? index + 1);
    return (
      <tr
        ref={ref}
        className={`${isPlaying ? "is-playing" : ""}${dragging ? " is-dragging" : ""}`}
        onDoubleClick={() => onPlay(index)}
        draggable={pointerFine}
        onDragStart={
          pointerFine
            ? (e) => {
                beginTrackDrag(e.dataTransfer, t.id, t.title);
                setDragging(true);
              }
            : undefined
        }
        onDragEnd={pointerFine ? () => setDragging(false) : undefined}
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
          <span className="num-text tabular">{displayNum}</span>
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
          {/* Phone-only second line: the dedicated artist column is
              hidden at ≤640px (see components.css) and the artist is
              stacked under the title instead — the standard mobile list
              row. Plain text, not a Link, so it can't fight the row's
              play-on-tap; artist navigation stays reachable via the row
              menu's "go to artist". */}
          <span className="row-sub-artist">{t.artist ?? "—"}</span>
        </td>
        <td className="col-artist">
          {t.artistId && t.artist ? (
            // draggable=false so a drag starting on this link drags the *track*
            // (via the row's draggable) rather than the anchor's URL.
            <Link to={`/artists/${t.artistId}`} draggable={false}>{t.artist}</Link>
          ) : (
            (t.artist ?? "—")
          )}
        </td>
        {showAlbum && (
          <td className="col-album">
            {t.albumId && t.album ? (
              <Link to={`/albums/${t.albumId}`} draggable={false}>{t.album}</Link>
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
