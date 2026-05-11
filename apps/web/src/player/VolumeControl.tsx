// Compact volume control for the player bar's right cluster.
// Speaker icon (click to toggle mute) + horizontal slider, both
// driving audio.volume / audio.muted on the shared <audio> element
// exposed via PlayerContext.
//
// The slider's pointer model mirrors Scrubber's: pointerdown captures
// and starts a drag, pointermove updates a preview ratio, pointerup
// releases. Click-without-drag is just a degenerate drag — same path.
//
// State is local. Volume + mute are persisted to localStorage so the
// level survives reloads; they're hydrated synchronously inside the
// useState initializer, so the audio element's first read picks up
// the user's last-saved level rather than the browser default of 1.

import { Volume1, Volume2, VolumeX } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { usePlayer } from "./PlayerContext";

const STORAGE_KEY = "player.volume";

interface Saved {
  volume: number;
  muted: boolean;
}

function loadSaved(): Saved {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return { volume: 1, muted: false };
    const parsed = JSON.parse(raw) as Partial<Saved>;
    const volume =
      typeof parsed.volume === "number"
        ? Math.max(0, Math.min(1, parsed.volume))
        : 1;
    const muted = parsed.muted === true;
    return { volume, muted };
  } catch {
    return { volume: 1, muted: false };
  }
}

export function VolumeControl() {
  const { audio } = usePlayer();
  const [{ volume, muted }, setState] = useState<Saved>(loadSaved);
  const draggingRef = useRef(false);
  const [dragPct, setDragPct] = useState<number | null>(null);

  // Reflect local state onto the audio element. audio.muted and
  // audio.volume are independent, which is exactly what we want — the
  // slider keeps its position when muted so the unmute target is
  // visible.
  useEffect(() => {
    if (!audio) return;
    audio.volume = volume;
    audio.muted = muted;
  }, [audio, volume, muted]);

  // Persist on every change. Cheap (one localStorage write per
  // pointer release), and avoids the "I set my volume to 30% and
  // then refreshed and it was full again" papercut.
  useEffect(() => {
    try {
      localStorage.setItem(
        STORAGE_KEY,
        JSON.stringify({ volume, muted } satisfies Saved)
      );
    } catch {
      // Quota / private-browsing — non-fatal; just lose persistence.
    }
  }, [volume, muted]);

  function ratioFromEvent(
    e: React.PointerEvent<HTMLDivElement>,
    el: HTMLDivElement
  ): number {
    const rect = el.getBoundingClientRect();
    const ratio = (e.clientX - rect.left) / rect.width;
    return Math.max(0, Math.min(1, ratio));
  }

  function commit(ratio: number) {
    // Adjusting the slider implies the user wants to hear sound, so
    // dragging unmutes — matches every desktop OS volume mixer.
    setState({ volume: ratio, muted: false });
  }

  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    e.currentTarget.setPointerCapture(e.pointerId);
    draggingRef.current = true;
    const ratio = ratioFromEvent(e, e.currentTarget);
    setDragPct(ratio * 100);
    commit(ratio);
  };
  const onPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!draggingRef.current) return;
    const ratio = ratioFromEvent(e, e.currentTarget);
    setDragPct(ratio * 100);
    commit(ratio);
  };
  const onPointerUp = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!draggingRef.current) return;
    draggingRef.current = false;
    if (e.currentTarget.hasPointerCapture(e.pointerId)) {
      e.currentTarget.releasePointerCapture(e.pointerId);
    }
    setDragPct(null);
  };

  const toggleMute = () => {
    setState((s) => {
      // Edge case: if volume is 0 and the user clicks the (already-X)
      // speaker icon, jumping straight to a fresh non-zero volume is
      // friendlier than "unmute to silence". 0.5 is a reasonable
      // "default loud-enough" guess.
      if (s.muted) return { volume: s.volume === 0 ? 0.5 : s.volume, muted: false };
      return { ...s, muted: true };
    });
  };

  const displayPct = muted ? 0 : (dragPct ?? volume * 100);
  const Icon = muted || volume === 0 ? VolumeX : volume < 0.5 ? Volume1 : Volume2;

  return (
    <div className="volume">
      <button
        type="button"
        className="icon-btn volume-btn"
        onClick={toggleMute}
        aria-label={muted ? "unmute" : "mute"}
        title={muted ? "unmute" : "mute"}
      >
        <Icon size={18} strokeWidth={1.5} />
      </button>
      <div
        className={`volume-bar ${dragPct !== null ? "is-dragging" : ""}`}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
        role="slider"
        aria-label="volume"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.round(displayPct)}
      >
        <div className="fill" style={{ width: `${displayPct}%` }} />
        <div className="thumb" style={{ left: `${displayPct}%` }} />
      </div>
    </div>
  );
}
