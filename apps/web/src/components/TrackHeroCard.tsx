// Featured track card used in the /search page's "top results" strip.
// Shares its visual shape (.search-hero) with ArtistHeroCard and
// AlbumHeroCard for stylistic consistency across the three buckets.
// What's distinct here: the cover doubles as a play button (clicking
// it replaces the queue and plays the track), and the meta column
// has artist + album as inline links.

import { Play } from "lucide-react";
import { Track } from "../api/types";
import { Cover } from "./Cover";
import { Link } from "../router";
import { fmtDuration } from "../utils/format";

interface Props {
  track: Track;
  onPlay: () => void;
}

export function TrackHeroCard({ track, onPlay }: Props) {
  return (
    <div className="search-hero">
      <button
        type="button"
        className="search-hero-cover"
        onClick={onPlay}
        aria-label={`play ${track.title}`}
      >
        <Cover
          coverArt={track.coverArt}
          seed={track.title}
          size={200}
          alt={track.title}
        />
        <span className="search-hero-play" aria-hidden>
          <Play size={20} fill="currentColor" strokeWidth={0} />
        </span>
      </button>
      <div className="search-hero-meta">
        <div className="search-hero-title">{track.title}</div>
        <div className="search-hero-sub">
          {track.artistId && track.artist ? (
            <Link to={`/artists/${track.artistId}`}>{track.artist}</Link>
          ) : (
            (track.artist ?? "—")
          )}
          {track.album && (
            <>
              <span className="search-hero-sep" aria-hidden>·</span>
              {track.albumId ? (
                <Link to={`/albums/${track.albumId}`}>{track.album}</Link>
              ) : (
                track.album
              )}
            </>
          )}
        </div>
        <div className="search-hero-time tabular">
          {fmtDuration(track.duration)}
        </div>
      </div>
    </div>
  );
}
