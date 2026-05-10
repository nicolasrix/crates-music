// Up-next view of the active playback queue. Reads from sync state
// directly; no API call needed. Track metadata for queue items lives
// in-memory in SyncContext.trackMeta — populated as we push tracks.
//
// Conceptual model: the queue is *upcoming* tracks only. The currently
// playing track is shown at the top of the page in a backgrounded card,
// not as part of the table. Items before the cursor (history) are
// hidden — the user can still jump to them via reorder, but they
// aren't part of "what's next". If we ever want a play history view,
// it's a separate page.
//
// Operations on the queue go through sync.submit so the gateway stays
// authoritative. "clear upcoming" iterates `remove` rather than calling
// `clear`, because `clear` nukes the now-playing track too and stops
// playback — not what the user means by "clear the queue".

import { ChevronDown, ChevronUp, Play, Trash2, X } from "lucide-react";
import { coverArtUrl } from "../api/client";
import { Layout } from "../components/Layout";
import { TrackRowMenu } from "../components/TrackRowMenu";
import { Link } from "../router";
import { useSync } from "../sync/SyncContext";
import { fmtDuration } from "../utils/format";
import type { Track } from "../api/types";

export function Queue() {
  const sync = useSync();
  const { queue, now_playing_index } = sync.state.playback;
  const items = queue.items;

  // Split: nothing before the cursor is shown; the cursor is the
  // "now playing" hero; everything after is the upcoming list.
  const upcomingStart = now_playing_index !== null ? now_playing_index + 1 : 0;
  const currentItem =
    now_playing_index !== null ? items[now_playing_index] : undefined;
  const currentTrack = currentItem
    ? sync.trackMeta.get(currentItem.track_id)
    : undefined;
  const upcoming = items.slice(upcomingStart);

  function jumpTo(absoluteIndex: number) {
    sync.submit({ type: "set_now_playing", index: absoluteIndex });
    sync.submit({ type: "set_playing", is_playing: true });
  }
  function remove(itemId: string) {
    sync.submit({ type: "remove", item_id: itemId });
  }
  function moveUp(itemId: string, absoluteIndex: number) {
    // Local-up within upcoming. Floor at upcomingStart so we never push
    // a queued item into history (which would re-order across the cursor
    // and is confusing).
    if (absoluteIndex <= upcomingStart) return;
    sync.submit({ type: "reorder", item_id: itemId, new_index: absoluteIndex - 1 });
  }
  function moveDown(itemId: string, absoluteIndex: number) {
    if (absoluteIndex >= items.length - 1) return;
    sync.submit({ type: "reorder", item_id: itemId, new_index: absoluteIndex + 1 });
  }
  function clearUpcoming() {
    if (upcoming.length === 0) return;
    for (const it of upcoming) {
      sync.submit({ type: "remove", item_id: it.item_id });
    }
  }

  return (
    <Layout breadcrumb="queue">
      {currentTrack && <NowPlayingCard track={currentTrack} />}

      <div className="section">
        <div className="section-head">
          <h2>up next</h2>
          {upcoming.length > 0 && (
            <span className="count tabular">
              {upcoming.length} track{upcoming.length === 1 ? "" : "s"}
            </span>
          )}
          {upcoming.length > 0 && (
            <button
              className="icon-btn ml-2"
              onClick={clearUpcoming}
              aria-label="clear upcoming"
              title="clear upcoming"
            >
              <Trash2 size={16} strokeWidth={1.5} />
            </button>
          )}
        </div>

        {upcoming.length === 0 && (
          <p className="text-fg-muted text-sm">
            nothing up next. start an album, click "add to queue" on a
            track, or hit "start station" to seed one from the recommender.
          </p>
        )}

        {upcoming.length > 0 && (
          <table className="tracks">
            <thead>
              <tr>
                <th className="col-num">#</th>
                <th className="col-title">title</th>
                <th className="col-artist">artist</th>
                <th className="col-album">album</th>
                <th className="col-time">time</th>
                <th className="col-menu" aria-hidden />
              </tr>
            </thead>
            <tbody>
              {upcoming.map((it, i) => {
                const meta = sync.trackMeta.get(it.track_id);
                const absoluteIndex = upcomingStart + i;
                return (
                  <QueueRow
                    key={it.item_id}
                    displayIndex={i + 1}
                    track={meta}
                    onJump={() => jumpTo(absoluteIndex)}
                    onRemove={() => remove(it.item_id)}
                    onMoveUp={() => moveUp(it.item_id, absoluteIndex)}
                    onMoveDown={() => moveDown(it.item_id, absoluteIndex)}
                    canMoveUp={i > 0}
                    canMoveDown={i < upcoming.length - 1}
                  />
                );
              })}
            </tbody>
          </table>
        )}
      </div>
    </Layout>
  );
}

// "Now playing" card — sits above the up-next table. The blurred cover
// is a separate absolutely-positioned <div> rather than a CSS
// background on the card itself, so we can blur + dim it independently
// of the foreground text.
function NowPlayingCard({ track }: { track: Track }) {
  const cover = coverArtUrl(track.coverArt, 600);
  const thumb = coverArtUrl(track.coverArt, 200);
  return (
    <div className="now-playing-card">
      {cover && (
        <div
          className="now-playing-backdrop"
          style={{ backgroundImage: `url(${cover})` }}
          aria-hidden
        />
      )}
      <div className="now-playing-body">
        <div className="now-playing-cover">
          {thumb ? (
            <img src={thumb} alt="" />
          ) : (
            <div className="now-playing-cover-fallback" aria-hidden />
          )}
        </div>
        <div className="now-playing-meta">
          <div className="now-playing-kind">now playing</div>
          <div className="now-playing-title">{track.title}</div>
          <div className="now-playing-sub">
            {track.artistId && track.artist ? (
              <Link to={`/artists/${track.artistId}`} className="sub-link">
                {track.artist}
              </Link>
            ) : (
              <span>{track.artist ?? "—"}</span>
            )}
            {track.album && <span aria-hidden>·</span>}
            {track.album &&
              (track.albumId ? (
                <Link to={`/albums/${track.albumId}`} className="sub-link">
                  {track.album}
                </Link>
              ) : (
                <span>{track.album}</span>
              ))}
            {track.duration !== undefined && <span aria-hidden>·</span>}
            {track.duration !== undefined && (
              <span>{fmtDuration(track.duration)}</span>
            )}
          </div>
        </div>
        {/* Same shape as the player bar: this track is by definition
            already in the queue, so the queue actions are hidden.
            Remaining: add to playlist, go to album, go to artist. */}
        <TrackRowMenu track={track} showQueueActions={false} />
      </div>
    </div>
  );
}

function QueueRow({
  displayIndex,
  track,
  onJump,
  onRemove,
  onMoveUp,
  onMoveDown,
  canMoveUp,
  canMoveDown,
}: {
  displayIndex: number;
  track: Track | undefined;
  onJump: () => void;
  onRemove: () => void;
  onMoveUp: () => void;
  onMoveDown: () => void;
  canMoveUp: boolean;
  canMoveDown: boolean;
}) {
  return (
    <tr onDoubleClick={onJump}>
      <td
        className="col-num is-clickable"
        onClick={onJump}
        role="button"
        tabIndex={0}
        aria-label={`play ${track?.title ?? "item"}`}
      >
        <span className="num-text tabular">{displayIndex}</span>
        <span className="num-play">
          <Play size={14} fill="currentColor" strokeWidth={0} />
        </span>
      </td>
      <td className="col-title is-clickable" onClick={onJump}>
        {track?.title ?? "(unknown — not in local cache)"}
      </td>
      <td className="col-artist">
        {track?.artistId && track.artist ? (
          <Link to={`/artists/${track.artistId}`}>{track.artist}</Link>
        ) : (
          track?.artist ?? "—"
        )}
      </td>
      <td className="col-album">
        {track?.albumId && track.album ? (
          <Link to={`/albums/${track.albumId}`}>{track.album}</Link>
        ) : (
          track?.album ?? "—"
        )}
      </td>
      <td className="col-time">{fmtDuration(track?.duration)}</td>
      <td className="col-menu" onClick={(e) => e.stopPropagation()}>
        <div className="queue-row-actions">
          <button
            className="row-menu-trigger"
            onClick={onMoveUp}
            disabled={!canMoveUp}
            aria-label="move up"
            title="move up"
          >
            <ChevronUp size={14} strokeWidth={1.5} />
          </button>
          <button
            className="row-menu-trigger"
            onClick={onMoveDown}
            disabled={!canMoveDown}
            aria-label="move down"
            title="move down"
          >
            <ChevronDown size={14} strokeWidth={1.5} />
          </button>
          <button
            className="row-menu-trigger"
            onClick={onRemove}
            aria-label="remove from queue"
            title="remove from queue"
          >
            <X size={14} strokeWidth={1.5} />
          </button>
          {/* The menu's queue actions ("play next", "add to queue") are
              hidden — the track is already queued. What's left: add to
              playlist, go to album, go to artist. Only renders when we
              have track metadata in the local cache; without it, the
              menu has nothing meaningful to act on. */}
          {track && <TrackRowMenu track={track} showQueueActions={false} />}
        </div>
      </td>
    </tr>
  );
}
