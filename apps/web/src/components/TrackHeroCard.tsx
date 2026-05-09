// Larger track card used in the /search page's "top results" strip.
// Distinct visual weight from the dense TrackTable rows below — the
// search page wants to make the best 1–3 hits unmissable.

import { Play } from "lucide-react";
import { Track } from "../api/types";
import { coverArtUrl } from "../api/client";
import { Link } from "../router";
import { fmtDuration } from "../utils/format";

interface Props {
  track: Track;
  onPlay: () => void;
}

export function TrackHeroCard({ track, onPlay }: Props) {
  const cover = coverArtUrl(track.coverArt, 200);
  return (
    <div className="track-hero">
      <button
        type="button"
        className={`track-hero-cover ${cover ? "" : "is-placeholder"}`}
        onClick={onPlay}
        aria-label={`play ${track.title}`}
      >
        {cover && <img src={cover} alt={track.title} loading="lazy" />}
        <span className="track-hero-play" aria-hidden>
          <Play size={22} fill="currentColor" strokeWidth={0} />
        </span>
      </button>
      <div className="track-hero-meta">
        <div className="track-hero-title">{track.title}</div>
        <div className="track-hero-sub">
          {track.artistId && track.artist ? (
            <Link to={`/artists/${track.artistId}`}>{track.artist}</Link>
          ) : (
            (track.artist ?? "—")
          )}
          {track.album && (
            <>
              <span className="track-hero-sep" aria-hidden>·</span>
              {track.albumId ? (
                <Link to={`/albums/${track.albumId}`}>{track.album}</Link>
              ) : (
                track.album
              )}
            </>
          )}
        </div>
        <div className="track-hero-time tabular">
          {fmtDuration(track.duration)}
        </div>
      </div>
    </div>
  );
}
