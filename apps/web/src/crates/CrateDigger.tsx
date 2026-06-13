// Face-view crate digger: the albums of one crate rendered as LP sleeves
// standing in a box, flipped through one at a time. CSS 3D only — sleeves
// are flat textured rectangles, which is exactly what the DOM compositor
// is good at; no canvas/WebGL.
//
// Scene layout (side view):                 painting order (z-index):
//        ___ behind stack (leans back)        crate back wall    50
//   |  /|/|/|                                 behind sleeves     ~100
//   | ║ ← focused (upright, lifted)           focused sleeve     200
//   |_‗_____ flipped stack (tips forward)     flipped sleeves    ~150
//   [_box__]                                  crate front panel  250
//
// The crate's front panel paints OVER the sleeve bottoms (pointer-events:
// none), which is what sells "the records are in a box". The focused
// sleeve is lifted above the rim; pull-out lifts it fully before
// navigating.
//
// Windowed rendering: only ~14 sleeves around the dig position are in the
// DOM. Keys are album ids, so React reuses nodes as the window slides and
// the transform transition animates each step ("the thunk").

import {
  useEffect,
  useRef,
  useState,
  type CSSProperties,
  type PointerEvent as ReactPointerEvent,
} from "react";
import type { Album } from "../api/types";
import { Cover } from "../components/Cover";

/** Sleeves rendered standing behind the focused one. */
const BEHIND_WINDOW = 8;
/** Already-flipped sleeves leaning against the crate front. */
const FLIPPED_WINDOW = 4;
/** Accumulated wheel deltaY per dig step. */
const WHEEL_STEP = 60;
/** Vertical drag distance per dig step. */
const DRAG_STEP = 64;
/** Horizontal drag distance that counts as a crate-change swipe. */
const SWIPE_X = 70;
/** Pull-out animation duration before navigating (kept minimal). */
const PULL_MS = 200;

interface CrateDiggerProps {
  albums: Album[];
  /** Masking-tape label on the crate front. */
  label?: string;
  index: number;
  onIndexChange: (index: number) => void;
  /** Pull-out: the focused sleeve was clicked — open the album. */
  onOpen: (album: Album) => void;
  /** Horizontal swipe / arrow-key crate change. */
  onCrateStep?: (dir: 1 | -1) => void;
  /** Whether a neighbor crate exists on each side — drag-follow stiffens
   *  and never commits toward a missing one. */
  hasPrevCrate?: boolean;
  hasNextCrate?: boolean;
  /** Direction this crate was entered from (1 = stepped right/next,
   *  -1 = left/previous, 0 = no slide). Drives the slide-in animation;
   *  the parent remounts the digger per crate (key=crate.id) so it
   *  replays on every switch. */
  enterFrom?: 1 | -1 | 0;
}

export function CrateDigger({
  albums,
  label,
  index,
  onIndexChange,
  onOpen,
  onCrateStep,
  hasPrevCrate = true,
  hasNextCrate = true,
  enterFrom = 0,
}: CrateDiggerProps) {
  const stageRef = useRef<HTMLDivElement>(null);
  const [pullingId, setPullingId] = useState<string | null>(null);

  // Refs for the event handlers — wheel is a native non-passive listener
  // (React's root-delegated wheel can't reliably preventDefault, and the
  // stage must swallow scroll), so it reads everything through refs.
  const indexRef = useRef(index);
  indexRef.current = index;
  const lenRef = useRef(albums.length);
  lenRef.current = albums.length;
  const onIndexRef = useRef(onIndexChange);
  onIndexRef.current = onIndexChange;
  const onCrateStepRef = useRef(onCrateStep);
  onCrateStepRef.current = onCrateStep;

  function step(delta: number) {
    const next = clamp(indexRef.current + delta, 0, lenRef.current - 1);
    if (next !== indexRef.current) onIndexRef.current(next);
  }

  useEffect(() => {
    const stage = stageRef.current;
    if (!stage) return;
    let acc = 0;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      acc += e.deltaY;
      while (acc >= WHEEL_STEP) {
        step(1);
        acc -= WHEEL_STEP;
      }
      while (acc <= -WHEEL_STEP) {
        step(-1);
        acc += WHEEL_STEP;
      }
    };
    stage.addEventListener("wheel", onWheel, { passive: false });
    return () => stage.removeEventListener("wheel", onWheel);
  }, []);

  // Pointer drag: vertical = dig (live, one step per DRAG_STEP px),
  // horizontal = crate swipe (decided at release). The axis locks on
  // whichever direction first exceeds the wobble threshold.
  const drag = useRef<{
    id: number;
    startX: number;
    startY: number;
    lastSteppedY: number;
    axis: "x" | "y" | null;
    moved: boolean;
  } | null>(null);

  function onPointerDown(e: ReactPointerEvent) {
    // Primary button / single touch only.
    if (e.button !== 0) return;
    drag.current = {
      id: e.pointerId,
      startX: e.clientX,
      startY: e.clientY,
      lastSteppedY: e.clientY,
      axis: null,
      moved: false,
    };
    stageRef.current?.setPointerCapture(e.pointerId);
  }

  function onPointerMove(e: ReactPointerEvent) {
    const d = drag.current;
    if (!d || d.id !== e.pointerId) return;
    const dx = e.clientX - d.startX;
    const dy = e.clientY - d.startY;
    if (!d.axis) {
      if (Math.abs(dx) < 10 && Math.abs(dy) < 10) return;
      d.axis = Math.abs(dx) > Math.abs(dy) ? "x" : "y";
    }
    d.moved = true;
    if (d.axis === "y") {
      // Drag up = dig deeper (next), drag down = back — matches wheel.
      let travelled = d.lastSteppedY - e.clientY;
      while (travelled >= DRAG_STEP) {
        step(1);
        d.lastSteppedY -= DRAG_STEP;
        travelled -= DRAG_STEP;
      }
      while (travelled <= -DRAG_STEP) {
        step(-1);
        d.lastSteppedY += DRAG_STEP;
        travelled += DRAG_STEP;
      }
    } else {
      // Live drag-follow: the whole crate tracks the finger (damped), so
      // the swipe has weight before it commits. Imperative style writes —
      // re-rendering the sleeve tree per pointer event would be waste.
      const headingOffEnd =
        (dx > 0 && !hasPrevCrate) || (dx < 0 && !hasNextCrate);
      const damp = headingOffEnd ? 0.15 : 0.5;
      const stage = stageRef.current;
      if (stage) {
        stage.style.transition = "none";
        stage.style.transform = `translateX(${dx * damp}px)`;
      }
    }
  }

  function onPointerUp(e: ReactPointerEvent) {
    const d = drag.current;
    if (!d || d.id !== e.pointerId) return;
    if (d.axis === "x") {
      const dx = e.clientX - d.startX;
      // Swipe left = next crate (content follows the finger).
      const commit: 1 | -1 | 0 =
        dx <= -SWIPE_X && hasNextCrate
          ? 1
          : dx >= SWIPE_X && hasPrevCrate
            ? -1
            : 0;
      const stage = stageRef.current;
      if (stage) {
        if (commit) {
          // The parent remounts the digger for the new crate (key change),
          // which discards these inline styles with the old DOM node.
          stage.style.transition = "";
          stage.style.transform = "";
        } else {
          // Spring back from wherever the finger left it.
          stage.style.transition = "transform 0.22s var(--ease-out-soft)";
          stage.style.transform = "translateX(0px)";
          window.setTimeout(() => {
            if (stageRef.current) {
              stageRef.current.style.transition = "";
              stageRef.current.style.transform = "";
            }
          }, 240);
        }
      }
      if (commit) onCrateStepRef.current?.(commit);
    }
    drag.current = null;
  }

  function onKeyDown(e: React.KeyboardEvent) {
    if (e.key === "ArrowDown" || e.key === "PageDown") {
      e.preventDefault();
      step(1);
    } else if (e.key === "ArrowUp" || e.key === "PageUp") {
      e.preventDefault();
      step(-1);
    } else if (e.key === "ArrowRight") {
      e.preventDefault();
      onCrateStep?.(1);
    } else if (e.key === "ArrowLeft") {
      e.preventDefault();
      onCrateStep?.(-1);
    } else if (e.key === "Enter" && albums[index]) {
      e.preventDefault();
      pullOut(albums[index]);
    }
  }

  // Pull-out: minimal lift, then navigate. The timeout is short enough
  // that an unmount race is harmless (navigate is idempotent), but clear
  // it anyway on unmount.
  const pullTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(
    () => () => {
      if (pullTimer.current) clearTimeout(pullTimer.current);
    },
    [],
  );
  function pullOut(album: Album) {
    if (pullingId) return;
    setPullingId(album.id);
    pullTimer.current = setTimeout(() => onOpen(album), PULL_MS);
  }

  function onSleeveClick(album: Album, offset: number) {
    if (drag.current?.moved) return; // tail end of a drag, not a click
    if (offset === 0) pullOut(album);
    else onIndexChange(clamp(index + offset, 0, albums.length - 1));
  }

  // Window: one extra sleeve on each side renders at opacity 0 so entering/
  // leaving the window fades instead of popping.
  const lo = Math.max(0, index - FLIPPED_WINDOW - 1);
  const hi = Math.min(albums.length - 1, index + BEHIND_WINDOW + 1);
  const visible: { album: Album; offset: number }[] = [];
  for (let i = lo; i <= hi; i++) {
    visible.push({ album: albums[i]!, offset: i - index });
  }

  function onPointerCancel() {
    drag.current = null;
    const stage = stageRef.current;
    if (stage) {
      stage.style.transition = "transform 0.22s var(--ease-out-soft)";
      stage.style.transform = "translateX(0px)";
    }
  }

  const enterClass =
    enterFrom === 1
      ? "crate-enter-right"
      : enterFrom === -1
        ? "crate-enter-left"
        : "";

  return (
    <div
      ref={stageRef}
      className={`crate-stage ${enterClass}`}
      role="listbox"
      aria-label="dig through the crate"
      aria-activedescendant={albums[index] ? `sleeve-${albums[index].id}` : undefined}
      tabIndex={0}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerCancel}
      onKeyDown={onKeyDown}
    >
      <div className="crate-box" aria-hidden="true">
        <span className="crate-box-back" />
        <span className="crate-box-front" />
        {label && <span className="crate-tape">{label}</span>}
      </div>
      {visible.map(({ album, offset }) => (
        <div
          key={album.id}
          id={`sleeve-${album.id}`}
          role="option"
          aria-selected={offset === 0}
          aria-label={`${album.name} — ${album.artist ?? "unknown artist"}`}
          className={`sleeve ${offset === 0 ? "is-focused" : ""}`}
          style={sleeveStyle(offset, pullingId === album.id)}
          onClick={() => onSleeveClick(album, offset)}
        >
          <Cover
            coverArt={album.coverArt}
            seed={album.name}
            size={300}
            alt=""
          />
        </div>
      ))}
      <div className="crate-counter tabular" aria-hidden="true">
        {albums.length === 0 ? "0 / 0" : `${index + 1} / ${albums.length}`}
      </div>
    </div>
  );
}

// Transform per offset-from-focus. transform-origin is bottom-center (set
// in CSS) — records pivot where they rest in the box, like real flipping.
// Inline styles (not classes) so the transition animates every step of a
// fast riffle, and exaggerated easing in CSS supplies the settle.
function sleeveStyle(offset: number, pulling: boolean): CSSProperties {
  if (pulling) {
    return {
      transform: "translate3d(0, -88px, 130px) rotateX(0deg) scale(1.04)",
      zIndex: 300,
      transitionDuration: "0.18s",
    };
  }
  if (offset === 0) {
    return {
      transform: "translate3d(0, -36px, 64px) rotateX(-6deg)",
      zIndex: 200,
    };
  }
  if (offset > 0) {
    // Standing in the crate behind the focused record, leaning back a
    // touch; each deeper sleeve sits higher + further away, so you see a
    // strip of every top edge — the classic crate silhouette.
    const depth = Math.min(offset, BEHIND_WINDOW);
    return {
      transform: `translate3d(0, ${-depth * 26}px, ${-depth * 48}px) rotateX(13deg)`,
      zIndex: 160 - offset,
      opacity: offset > BEHIND_WINDOW ? 0 : 1 - depth * 0.05,
    };
  }
  // Already flipped: tipped forward against the inside of the front
  // panel — the satisfying part is the swing-forward transition itself,
  // not a persistent leaning stack. Settled records sink low and nearly
  // flat so their projected top edges stay behind the panel instead of
  // clipping through its rim, and everything past the most recent flip
  // fades out entirely (the transition turns that into a clean tuck-away
  // rather than a pop).
  const k = Math.min(-offset, FLIPPED_WINDOW);
  return {
    transform: `translate3d(0, ${78 + k * 2}px, ${-46 - k * 8}px) rotateX(-87deg)`,
    zIndex: 150 + offset,
    opacity: k >= 2 ? 0 : 0.85,
  };
}

function clamp(v: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, v));
}
