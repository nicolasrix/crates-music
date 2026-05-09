// Bottom-fixed transport. Three-column grid: now-playing on the left,
// transport+scrubber centered, autoplay toggle right-aligned. Reads the
// active artwork palette from the ArtworkPalette context so the scrubber
// fill tints to match the page the user is on.

import {
  ListMusic,
  Pause,
  Play,
  Repeat,
  SkipBack,
  SkipForward,
  Sparkles,
} from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { coverArtUrl } from "../api/client";
import { useArtwork } from "../components/ArtworkPalette";
import { Link, useRoute } from "../router";
import { fmtDuration } from "../utils/format";
import { useAutoplay } from "./AutoplayContext";
import { usePlayer } from "./PlayerContext";
import { VolumeControl } from "./VolumeControl";

export function PlayerBar() {
  const { nowPlaying, isPlaying, togglePlay, next, prev, hasNext, hasPrev, queueLength } =
    usePlayer();
  const { palette } = useArtwork();
  const { path } = useRoute();
  const { autoplay, setAutoplay } = useAutoplay();
  const onQueuePage = path === "/queue";

  // Empty state — keep the chrome bar visible so the layout doesn't shift.
  if (!nowPlaying) {
    return (
      <div className="player">
        <div className="text-fg-faint text-sm px-3">
          nothing playing — pick an album.
        </div>
        <div />
        <div />
      </div>
    );
  }

  const cover = nowPlaying.coverArt ? coverArtUrl(nowPlaying.coverArt, 100) : null;
  const artAccent = palette?.accent ?? "var(--accent)";

  return (
    <div className="player" style={{ ["--art-accent" as never]: artAccent }}>
      <div className="np">
        <div className="cover">
          {cover && <img src={cover} alt="" />}
        </div>
        <div className="meta">
          <div className="title">{nowPlaying.title}</div>
          <div className="sub">
            {nowPlaying.artistId && nowPlaying.artist ? (
              <Link to={`/artists/${nowPlaying.artistId}`}>
                {nowPlaying.artist}
              </Link>
            ) : (
              (nowPlaying.artist ?? "—")
            )}
            <span className="sep"> · </span>
            {nowPlaying.albumId && nowPlaying.album ? (
              <Link to={`/albums/${nowPlaying.albumId}`}>
                {nowPlaying.album}
              </Link>
            ) : (
              (nowPlaying.album ?? "—")
            )}
          </div>
        </div>
      </div>

      <div className="transport-col">
        <div className="transport">
          <button
            className="icon-btn"
            disabled
            title="repeat (not yet wired)"
            aria-label="repeat"
          >
            <Repeat size={18} strokeWidth={1.5} />
          </button>
          <button
            className="icon-btn"
            onClick={prev}
            disabled={!hasPrev}
            aria-label="previous"
            title="previous"
          >
            <SkipBack size={18} strokeWidth={1.5} />
          </button>
          <button
            className="icon-btn play-bar"
            onClick={togglePlay}
            aria-label={isPlaying ? "pause" : "play"}
            title={isPlaying ? "pause" : "play"}
          >
            {isPlaying ? (
              <Pause size={16} fill="currentColor" strokeWidth={0} />
            ) : (
              <Play size={16} fill="currentColor" strokeWidth={0} />
            )}
          </button>
          <button
            className="icon-btn"
            onClick={next}
            disabled={!hasNext}
            aria-label="next"
            title="next"
          >
            <SkipForward size={18} strokeWidth={1.5} />
          </button>
          <button
            className={`icon-btn ${autoplay ? "is-on" : ""}`}
            onClick={() => setAutoplay(!autoplay)}
            aria-label="autoplay"
            title="autoplay (keeps the queue topped up with recommendations)"
          >
            <Sparkles size={18} strokeWidth={1.5} />
          </button>
        </div>
        <Scrubber />
      </div>

      <div className="right-cluster">
        <VolumeControl />
        <Link
          to="/queue"
          className={`icon-btn queue-btn ${onQueuePage ? "is-on" : ""}`}
          aria-label={`open queue (${queueLength} item${queueLength === 1 ? "" : "s"})`}
        >
          <ListMusic size={18} strokeWidth={1.5} />
          {queueLength > 0 && <span className="queue-count tabular">{queueLength}</span>}
        </Link>
        <button
          className={`autoplay-toggle ${autoplay ? "is-on" : ""}`}
          onClick={() => setAutoplay(!autoplay)}
          aria-pressed={autoplay}
        >
          <span className="dot" />
          autoplay {autoplay ? "on" : "off"}
        </button>
      </div>
    </div>
  );
}

// Scrubber — pulls position + duration from the underlying <audio> element
// (exposed via PlayerContext) on each animation frame while playing. We
// don't broadcast timeupdate through React state on the context itself
// because it would re-render every consumer 4–60×/s.
//
// Seek model: pointerdown captures the pointer and starts a drag. While
// dragging, the bar shows a *preview* ratio (so the thumb tracks the
// cursor immediately, decoupled from the RAF read of audio.currentTime).
// pointerup commits the preview to audio.currentTime. Click-without-drag
// is just a degenerate drag — same code path.
function Scrubber() {
  const { isPlaying, audio } = usePlayer();
  const [position, setPosition] = useState(0);
  const [duration, setDuration] = useState(0);
  const rafRef = useRef<number | null>(null);
  const draggingRef = useRef(false);
  const [dragPct, setDragPct] = useState<number | null>(null);

  useEffect(() => {
    if (!audio) return;
    const tick = () => {
      // Don't fight the user: while they're dragging, the preview owns the
      // displayed position. Reading audio.currentTime here would still be
      // safe (we don't write it until release) but the extra setState
      // would just churn re-renders that get overridden by dragPct anyway.
      if (!draggingRef.current) {
        setPosition(audio.currentTime || 0);
        setDuration(audio.duration || 0);
      }
      rafRef.current = requestAnimationFrame(tick);
    };
    if (isPlaying) {
      tick();
    } else {
      setPosition(audio.currentTime || 0);
      setDuration(audio.duration || 0);
    }
    return () => {
      if (rafRef.current !== null) cancelAnimationFrame(rafRef.current);
    };
  }, [isPlaying, audio]);

  function ratioFromEvent(
    e: React.PointerEvent<HTMLDivElement>,
    el: HTMLDivElement
  ): number {
    const rect = el.getBoundingClientRect();
    const ratio = (e.clientX - rect.left) / rect.width;
    return Math.max(0, Math.min(1, ratio));
  }

  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!audio || !audio.duration) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    draggingRef.current = true;
    setDragPct(ratioFromEvent(e, e.currentTarget) * 100);
  };
  const onPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!draggingRef.current) return;
    setDragPct(ratioFromEvent(e, e.currentTarget) * 100);
  };
  const onPointerUp = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!draggingRef.current) return;
    draggingRef.current = false;
    if (e.currentTarget.hasPointerCapture(e.pointerId)) {
      e.currentTarget.releasePointerCapture(e.pointerId);
    }
    if (dragPct !== null && audio?.duration) {
      audio.currentTime = (dragPct / 100) * audio.duration;
      setPosition(audio.currentTime);
    }
    setDragPct(null);
  };

  const playPct = duration > 0 ? Math.min(100, (position / duration) * 100) : 0;
  const pct = dragPct ?? playPct;
  // While dragging, also show the previewed time on the left readout — this
  // makes "I want to go to ~2:45" land in seconds rather than guess-and-check.
  const previewSeconds =
    dragPct !== null && duration > 0 ? (dragPct / 100) * duration : position;

  return (
    <div className="scrub">
      <span className="time">{fmtDuration(previewSeconds)}</span>
      <div
        className={`scrub-bar ${dragPct !== null ? "is-dragging" : ""}`}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
        role="slider"
        aria-label="seek"
        aria-valuemin={0}
        aria-valuemax={Math.round(duration)}
        aria-valuenow={Math.round(previewSeconds)}
      >
        <div className="fill" style={{ width: `${pct}%` }} />
        <div className="thumb" style={{ left: `${pct}%` }} />
      </div>
      <span className="time">{fmtDuration(duration)}</span>
    </div>
  );
}
