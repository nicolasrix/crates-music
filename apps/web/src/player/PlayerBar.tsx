// Bottom-fixed transport. Three-column grid: now-playing on the left,
// transport+scrubber centered, autoplay toggle right-aligned. Reads the
// active artwork palette from the ArtworkPalette context so the scrubber
// fill tints to match the page the user is on.

import {
  Cast,
  ListMusic,
  Pause,
  Play,
  Repeat,
  SkipBack,
  SkipForward,
  Sparkles,
  Speaker,
  ThumbsDown,
  ThumbsUp,
} from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useArtwork } from "../components/ArtworkPalette";
import { Cover } from "../components/Cover";
import { EntityRating } from "../components/EntityRating";
import { TrackRowMenu } from "../components/TrackRowMenu";
import { Link, useRoute } from "../router";
import { fmtDuration } from "../utils/format";
import { useSync } from "../sync/SyncContext";
import { useAutoplay } from "./AutoplayContext";
import { usePlayer } from "./PlayerContext";
import { useRecommendationFeedback } from "./useRecommendationFeedback";
import { VolumeControl } from "./VolumeControl";

export function PlayerBar() {
  const {
    nowPlaying,
    isPlaying,
    togglePlay,
    next,
    prev,
    hasNext,
    hasPrev,
    queueLength,
    outputEnabled,
    setOutputEnabled,
  } = usePlayer();
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

  const artAccent = palette?.accent ?? "var(--accent)";

  return (
    <div className="player" style={{ ["--art-accent" as never]: artAccent }}>
      <div className="np">
        <div className="cover">
          <Cover
            coverArt={nowPlaying.coverArt}
            seed={nowPlaying.album ?? nowPlaying.title}
            size={100}
            alt=""
          />
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
        {/* Same shape as the queue rows — the now-playing track is by
            definition already in the queue, so "play next" / "add to
            queue" are hidden. What remains: add to playlist, go to
            album, go to artist. */}
        <TrackRowMenu track={nowPlaying} showQueueActions={false} />
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
        <EntityRating kind="track" id={nowPlaying.id} />
        <RecommendationFeedback trackId={nowPlaying.id} />
        <button
          className={`icon-btn ${outputEnabled ? "is-on" : ""}`}
          onClick={() => setOutputEnabled(!outputEnabled)}
          aria-label={outputEnabled ? "audio plays on this device" : "remote control only (silent)"}
          aria-pressed={outputEnabled}
          title={
            outputEnabled
              ? "Audio output: this device. Tap to make this a silent remote — playback continues on your other device."
              : "Remote control only — this device is silent. Tap to play audio here too."
          }
        >
          {outputEnabled ? (
            <Speaker size={18} strokeWidth={1.5} />
          ) : (
            <Cast size={18} strokeWidth={1.5} />
          )}
        </button>
        <VolumeControl />
        <Link
          to="/queue"
          className={`icon-btn queue-btn ${onQueuePage ? "is-on" : ""}`}
          aria-label={`open queue (${queueLength} item${queueLength === 1 ? "" : "s"})`}
        >
          <ListMusic size={18} strokeWidth={1.5} />
          {queueLength > 0 && <span className="queue-count tabular">{queueLength}</span>}
        </Link>
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

// Thumb-up / thumb-down pill. Labeled "Rate recommendation" so the
// user understands the votes are about *the recommendation choice*
// (was this a good fit to play right now?), not the song itself — no
// risk of the user expecting this to feed a Subsonic-style "favorite"
// or "starred" list. The label is rendered inline rather than as a
// tooltip so it remains visible without hover, since hover-only
// affordances are easy to miss in a persistent player.
//
// Gating: locked unless the current queue item was pushed by the
// autoplay refill (AutoplayContext.isRecommendation). A user-picked
// track isn't a recommendation, so rating it would feed misleading
// signal to the recommender. The disabled state shows the pill so
// the user still sees the affordance exists; the tooltip explains
// why it's inactive.
function RecommendationFeedback({ trackId }: { trackId: string }) {
  const { vote, pending, cast } = useRecommendationFeedback(trackId);
  const { state } = useSync();
  const { isRecommendation } = useAutoplay();
  const idx = state.playback.now_playing_index;
  const currentItemId =
    idx !== null ? state.playback.queue.items[idx]?.item_id : undefined;
  const isRec = isRecommendation(currentItemId);

  const lockedTitle =
    "you picked this track yourself — only recommended tracks can be rated";
  return (
    <div
      className={`rec-feedback ${isRec ? "" : "is-locked"}`}
      role="group"
      aria-label="rate this recommendation"
      title={isRec ? undefined : lockedTitle}
    >
      <span className="rec-feedback__label" aria-hidden="true">
        rate rec
      </span>
      <button
        type="button"
        className={`rec-feedback__btn up ${vote === "up" && isRec ? "is-active" : ""}`}
        aria-label="good recommendation"
        aria-pressed={vote === "up" && isRec}
        title={isRec ? "good recommendation" : lockedTitle}
        disabled={pending || !isRec}
        onClick={() => cast("up")}
      >
        <ThumbsUp
          size={14}
          strokeWidth={1.75}
          fill={vote === "up" && isRec ? "currentColor" : "none"}
        />
      </button>
      <button
        type="button"
        className={`rec-feedback__btn down ${vote === "down" && isRec ? "is-active" : ""}`}
        aria-label="poor recommendation"
        aria-pressed={vote === "down" && isRec}
        title={isRec ? "poor recommendation" : lockedTitle}
        disabled={pending || !isRec}
        onClick={() => cast("down")}
      >
        <ThumbsDown
          size={14}
          strokeWidth={1.75}
          fill={vote === "down" && isRec ? "currentColor" : "none"}
        />
      </button>
    </div>
  );
}
