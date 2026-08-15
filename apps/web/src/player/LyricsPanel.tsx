// Lyrics panel — opened from the player bar, covering the main area while
// the transport stays reachable underneath.
//
// Three behaviours, in the order they matter:
//   1. show the current track's lyrics, following track changes;
//   2. highlight the line being sung;
//   3. click a line to seek there.
//
// Two structural notes worth keeping:
//
// * The panel is portalled to <body> rather than rendered inside the
//   player bar. `.player` sets `backdrop-filter`, which makes it a
//   containing block for fixed-position descendants — a `position: fixed`
//   child would anchor to the 92px bar instead of the viewport.
//
// * The highlight runs off requestAnimationFrame reading audio.currentTime
//   directly, never off React state broadcast from PlayerContext. Same
//   reason the scrubber does: a timeupdate-through-context design
//   re-renders every consumer several times a second. Here we go further
//   and only call setState when the *line index* changes — roughly once
//   every few seconds instead of 60×/s.

import { MicVocal, Minus, Plus, RefreshCw, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useQueryClient } from "@tanstack/react-query";
import {
  LyricsDisabledError,
  LyricsUnavailableError,
  type LyricLine,
  type LyricsDoc,
} from "../api/lyrics";
import { useSync } from "../sync/SyncContext";
import { useToast } from "../toast/ToastContext";
import { activeLineIndex, HIGHLIGHT_LEAD_MS, sortLines } from "./activeLine";
import {
  formatOffset,
  OFFSET_STEP_MS,
  readOffset,
  writeOffset,
} from "./lyricsOffset";
import { usePlayer } from "./PlayerContext";
import { prefetchLyrics, useLyrics } from "./useLyrics";

/** The button that lives in the player bar, plus the panel it opens. Kept
 *  together so PlayerBar only has to mount one thing. */
export function LyricsToggle() {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button
        className={`icon-btn ${open ? "is-on" : ""}`}
        onClick={() => setOpen((o) => !o)}
        aria-label="lyrics"
        aria-pressed={open}
        title="lyrics"
      >
        <MicVocal size={18} strokeWidth={1.5} />
      </button>
      {open && <LyricsPanel onClose={() => setOpen(false)} />}
    </>
  );
}

function LyricsPanel({ onClose }: { onClose: () => void }) {
  const { nowPlaying, audio, isPlaying } = usePlayer();
  const trackId = nowPlaying?.id;
  const { doc, offline, loading, error, retry, refresh, refreshing, refreshError } =
    useLyrics(trackId);
  const toast = useToast();

  // Warm the next queue item's lyrics, but only from here — i.e. only while
  // the panel is actually open. See prefetchLyrics for why this is gated on
  // the panel rather than fired on every track change.
  const qc = useQueryClient();
  const { state } = useSync();
  const cursor = state.playback.now_playing_index;
  const nextId =
    cursor === null ? undefined : state.playback.queue.items[cursor + 1]?.track_id;
  useEffect(() => {
    prefetchLyrics(qc, nextId);
  }, [qc, nextId]);

  // Escape closes, matching every other dismissable surface in the app.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  // Sorted once per document, not once per frame. The gateway sorts what
  // it parses, but a Navidrome-tagged document is passed through as the
  // file provided it — cheap insurance for the binary search's precondition.
  const lines = useMemo(
    () => (doc?.synced && doc.lines ? sortLines(doc.lines) : []),
    [doc],
  );

  // Per-track timing nudge. Held in state as well as localStorage so the
  // highlight re-syncs on the same tick the button is pressed rather than
  // on the next track change.
  const [offset, setOffset] = useState(0);
  const nudge = (delta: number) => {
    if (!trackId) return;
    setOffset(writeOffset(trackId, offset + delta));
  };

  const [active, setActive] = useState(-1);
  // Mirrors `active` so the animation frame can compare without reading
  // state (which would make the effect depend on it and restart the loop).
  const activeRef = useRef(-1);
  const applyIndex = useCallback((idx: number) => {
    if (idx === activeRef.current) return;
    activeRef.current = idx;
    setActive(idx);
  }, []);

  useEffect(() => {
    if (!audio || lines.length === 0) {
      applyIndex(-1);
      return;
    }
    const sync = () => {
      applyIndex(
        activeLineIndex(lines, audio.currentTime * 1000 + HIGHLIGHT_LEAD_MS + offset),
      );
    };
    let raf: number | null = null;
    const tick = () => {
      sync();
      raf = requestAnimationFrame(tick);
    };
    if (isPlaying) tick();
    else sync();
    // While paused there is no frame loop, so an external seek (scrubber,
    // media keys, another device) would otherwise leave the highlight stale.
    audio.addEventListener("seeked", sync);
    return () => {
      if (raf !== null) cancelAnimationFrame(raf);
      audio.removeEventListener("seeked", sync);
    };
  }, [audio, isPlaying, lines, offset, applyIndex]);

  // --- Auto-scroll, and getting out of the user's way ------------------
  //
  // `following` is off as soon as the user scrolls by hand. We detect that
  // from wheel/touch/keyboard rather than the `scroll` event, because
  // scrollIntoView fires `scroll` too and there is no reliable way to tell
  // the two apart — listening to `scroll` would switch following off the
  // first time we scrolled on the user's behalf.
  const [following, setFollowing] = useState(true);
  const lineEls = useRef(new Map<number, HTMLElement>());

  useEffect(() => {
    // A new track starts followed again, and at the top, with whatever
    // nudge that track was last given.
    setFollowing(true);
    applyIndex(-1);
    setOffset(trackId ? readOffset(trackId) : 0);
  }, [trackId, applyIndex]);

  useEffect(() => {
    if (!following || active < 0) return;
    const el = lineEls.current.get(active);
    if (!el) return;
    const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    el.scrollIntoView({ block: "center", behavior: reduced ? "auto" : "smooth" });
  }, [active, following]);

  const resumeFollowing = () => {
    setFollowing(true);
    const el = lineEls.current.get(activeRef.current);
    el?.scrollIntoView({ block: "center", behavior: "smooth" });
  };

  const seekToLine = (idx: number, startMs: number) => {
    if (!audio) return;
    // The offset runs the other way here: it is added to the *position* on
    // lookup, so the audio time a line belongs to is its timestamp minus
    // the nudge. Without this, clicking a line on a nudged track would
    // land somewhere the highlight then immediately corrects away from.
    audio.currentTime = Math.max(0, startMs - offset) / 1000;
    // Move the highlight now rather than waiting for `seeked` — the click
    // should feel instant, and while paused nothing else would update it.
    applyIndex(idx);
    setFollowing(true);
  };

  const onRefresh = () => {
    refresh();
    toast("re-checking for lyrics…");
  };

  // A failed refresh leaves the existing lyrics on screen, so it needs a
  // toast to be noticed at all. The common case is a guest: refreshing
  // rewrites a row the whole household reads, so guests are 403'd.
  useEffect(() => {
    if (refreshError) toast(refreshError.message, { variant: "error" });
  }, [refreshError, toast]);

  return createPortal(
    <aside className="lyrics-panel" aria-label="lyrics">
      <header className="lyrics-head">
        <div className="lyrics-title">
          <MicVocal size={16} strokeWidth={1.5} aria-hidden="true" />
          <div className="lyrics-track">
            <div className="t">{nowPlaying?.title ?? "nothing playing"}</div>
            <div className="a">{nowPlaying?.artist ?? "—"}</div>
          </div>
        </div>
        <div className="lyrics-actions">
          {/* Only for timed lyrics — there is nothing to nudge on a plain
              text document, and offering the control there would suggest
              highlighting that is never coming. */}
          {lines.length > 0 && (
            <div className="lyrics-offset" role="group" aria-label="lyric timing">
              <button
                className="icon-btn"
                onClick={() => nudge(-OFFSET_STEP_MS)}
                aria-label="shift lyrics later"
                title="lyrics running ahead? shift them later"
              >
                <Minus size={14} strokeWidth={2} />
              </button>
              <button
                className="lyrics-offset-value"
                onClick={() => nudge(-offset)}
                disabled={offset === 0}
                title={offset === 0 ? "lyric timing" : "reset timing"}
              >
                {formatOffset(offset) || "sync"}
              </button>
              <button
                className="icon-btn"
                onClick={() => nudge(OFFSET_STEP_MS)}
                aria-label="shift lyrics earlier"
                title="lyrics running behind? shift them earlier"
              >
                <Plus size={14} strokeWidth={2} />
              </button>
            </div>
          )}
          {doc && (
            <button
              className="icon-btn"
              onClick={onRefresh}
              // Re-resolving is a server round-trip by definition, so it
              // has nothing to offer while we are reading the offline copy.
              disabled={refreshing || offline}
              aria-label="look for better lyrics"
              title={offline ? "offline — can't look again" : "wrong lyrics? look again"}
            >
              <RefreshCw
                size={16}
                strokeWidth={1.5}
                className={refreshing ? "is-spinning" : undefined}
              />
            </button>
          )}
          <button className="icon-btn" onClick={onClose} aria-label="close lyrics" title="close">
            <X size={18} strokeWidth={1.5} />
          </button>
        </div>
      </header>

      <div
        className="lyrics-body"
        // Unambiguously user-initiated, unlike `scroll`.
        onWheel={() => setFollowing(false)}
        onTouchMove={() => setFollowing(false)}
      >
        <Body
          loading={loading}
          error={error}
          doc={doc}
          lines={lines}
          active={active}
          onSeek={seekToLine}
          onRetry={retry}
          onRefresh={onRefresh}
          register={(idx, el) => {
            if (el) lineEls.current.set(idx, el);
            else lineEls.current.delete(idx);
          }}
        />
      </div>

      {!following && lines.length > 0 && (
        <button className="lyrics-follow" onClick={resumeFollowing}>
          back to the current line
        </button>
      )}

      {doc && <Attribution doc={doc} offline={offline} />}
    </aside>,
    document.body,
  );
}

interface BodyProps {
  loading: boolean;
  error: Error | null;
  doc: LyricsDoc | undefined;
  lines: LyricLine[];
  active: number;
  onSeek: (idx: number, startMs: number) => void;
  onRetry: () => void;
  onRefresh: () => void;
  register: (idx: number, el: HTMLElement | null) => void;
}

// Every "there is nothing to show" case is its own message, because they
// call for different actions: a config problem an admin fixes, an outage
// worth retrying, a confirmed absence worth a second look, and an
// instrumental track where the right answer is to say so and stop.
function Body({
  loading,
  error,
  doc,
  lines,
  active,
  onSeek,
  onRetry,
  onRefresh,
  register,
}: BodyProps) {
  if (loading) return <p className="lyrics-note">looking for lyrics…</p>;

  if (error instanceof LyricsDisabledError) {
    return <p className="lyrics-note">lyrics are turned off on this gateway.</p>;
  }
  if (error instanceof LyricsUnavailableError) {
    return (
      <div className="lyrics-note">
        <p>couldn&rsquo;t reach a lyrics source just now.</p>
        <button className="lyrics-btn" onClick={onRetry}>
          try again
        </button>
      </div>
    );
  }
  if (error) return <p className="lyrics-note">lyrics failed to load: {error.message}</p>;
  if (!doc) return null;

  if (doc.instrumental) return <p className="lyrics-note">instrumental — no lyrics.</p>;

  if (doc.source === "none") {
    return (
      <div className="lyrics-note">
        <p>no lyrics found for this track.</p>
        <button className="lyrics-btn" onClick={onRefresh}>
          look again
        </button>
      </div>
    );
  }

  if (doc.synced && lines.length > 0) {
    return (
      <ol className="lyrics-lines is-synced">
        {lines.map((line, idx) => (
          <li key={`${line.start_ms}-${idx}`}>
            <button
              ref={(el) => register(idx, el)}
              className={`lyrics-line${idx === active ? " is-active" : ""}${
                idx < active ? " is-past" : ""
              }`}
              onClick={() => onSeek(idx, line.start_ms)}
              aria-current={idx === active ? "true" : undefined}
            >
              {/* A timed line can be empty — an instrumental gap. Keep the
                  row so the highlight still travels through it. */}
              {line.text || " "}
            </button>
          </li>
        ))}
      </ol>
    );
  }

  if (doc.plain) {
    return (
      <div className="lyrics-lines">
        <p className="lyrics-unsynced-note">
          no timings for this one — text only, so no line highlighting.
        </p>
        {doc.plain.split("\n").map((text, idx) => (
          <p key={idx} className="lyrics-line is-static">
            {text || " "}
          </p>
        ))}
      </div>
    );
  }

  return <p className="lyrics-note">no lyrics found for this track.</p>;
}

// Where the words came from. Worth showing: a fuzzy provider match is the
// one case where the lyrics can be confidently wrong, and knowing that is
// what makes the refresh button meaningful rather than mysterious.
//
// `offline` is appended rather than substituted — the provenance still
// holds, it is just being read from the copy saved with the download, which
// explains both why it might be behind the server and why refresh is off.
function Attribution({ doc, offline }: { doc: LyricsDoc; offline: boolean }) {
  if (doc.source === "none") return null;
  const from =
    doc.source === "navidrome"
      ? "from the file's own tags"
      : doc.match_kind === "search"
        ? "from lrclib.net — closest match by title and length"
        : "from lrclib.net";
  return (
    <footer className="lyrics-foot">
      {from}
      {offline && " · saved copy"}
    </footer>
  );
}
