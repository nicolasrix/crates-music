// Windowed track list. Same outer shape as TrackTable (header + tbody)
// but only the rows currently in (or near) the viewport are rendered.
// We use the "padding row" pattern rather than absolute-positioning
// rows: an aria-hidden <tr> with explicit height props takes up the
// space above the visible window, and another below. This keeps native
// table layout intact (column widths stay consistent across all
// rendered rows) while letting the scrollbar still represent the full
// list height.
//
// The virtualizer is anchored to <main> — the app shell makes <body>
// non-scrolling and <main> the actual scroll container. Window
// virtualization would silently watch a frozen viewport and render
// everything.
//
// Row rendering is shared with TrackTable via <TrackRow>.

import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { usePlayer } from "../player/PlayerContext";
import { TrackRow } from "./TrackRow";
import type { Track } from "../api/types";

// Row-height estimate. Real heights are measured (via measureElement
// callback) so this only matters for initial paint; over-/underestimate
// just shifts the moment of corrective re-flow.
const ROW_HEIGHT = 40;
// Buffer above/below the visible window. 12 rows ~ 480 px, a comfortable
// scroll margin that absorbs fast scroll without flashing empty rows.
const OVERSCAN = 12;

interface Props {
  tracks: Track[];
  showAlbum?: boolean;
  showCover?: boolean;
  onPlay: (index: number) => void;
}

export function VirtualTrackTable({
  tracks,
  showAlbum = false,
  showCover,
  onPlay,
}: Props) {
  const { nowPlaying } = usePlayer();
  const playingId = nowPlaying?.id ?? null;
  const renderCover = showCover ?? showAlbum;

  const containerRef = useRef<HTMLDivElement>(null);
  const [scroller, setScroller] = useState<HTMLElement | null>(null);
  const [scrollMargin, setScrollMargin] = useState(0);

  // Walk up to <main>, the actual scroll container. Done in a
  // layout-effect so the virtualizer's first render still has a chance
  // to read both before the user can scroll. If <main> isn't found
  // (different mounting context), virtualization degrades to "render
  // everything", which is correct if slow.
  useLayoutEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    let el: HTMLElement | null = container.parentElement;
    while (el && el.tagName !== "MAIN") el = el.parentElement;
    if (!el) return;
    setScroller(el);
    // scrollMargin is the offset from the top of the scroll container's
    // *content* to our list's top edge. Computed via rect differences
    // because offsetTop returns position relative to offsetParent, not
    // the scroll container — they don't always coincide.
    const cRect = container.getBoundingClientRect();
    const sRect = el.getBoundingClientRect();
    setScrollMargin(cRect.top - sRect.top + el.scrollTop);
  }, []);

  // If content above the table changes height (image loads, async
  // section loads, etc.), our scrollMargin would drift. ResizeObserver
  // on <main> recomputes whenever the shell layout shifts. Cheap; only
  // fires on actual layout changes.
  useEffect(() => {
    if (!scroller) return;
    const container = containerRef.current;
    if (!container) return;
    const ro = new ResizeObserver(() => {
      const cRect = container.getBoundingClientRect();
      const sRect = scroller.getBoundingClientRect();
      setScrollMargin(cRect.top - sRect.top + scroller.scrollTop);
    });
    ro.observe(scroller);
    return () => ro.disconnect();
  }, [scroller]);

  const virtualizer = useVirtualizer({
    count: tracks.length,
    getScrollElement: () => scroller,
    estimateSize: () => ROW_HEIGHT,
    overscan: OVERSCAN,
    scrollMargin,
  });

  const virtualItems = virtualizer.getVirtualItems();
  const totalSize = virtualizer.getTotalSize();
  // Base columns: num + title + artist + time + menu = 5.
  // Optional: +1 if showAlbum, +1 if renderCover.
  const colCount = 5 + (showAlbum ? 1 : 0) + (renderCover ? 1 : 0);

  // Convert absolute-from-document positions into list-relative paddings.
  // (vi.start includes scrollMargin; subtracting gives the position
  // within the list's own coordinate space, where 0 = first row.)
  const first = virtualItems[0];
  const last = virtualItems[virtualItems.length - 1];
  const paddingTop = first ? first.start - scrollMargin : 0;
  const paddingBottom = last ? totalSize - (last.end - scrollMargin) : 0;

  return (
    <div ref={containerRef}>
      <table className="tracks">
        <thead>
          <tr>
            <th className="col-num">#</th>
            {renderCover && <th className="col-cover" aria-hidden />}
            <th className="col-title">title</th>
            <th className="col-artist">artist</th>
            {showAlbum && <th className="col-album">album</th>}
            <th className="col-time">time</th>
            <th className="col-menu" aria-hidden />
          </tr>
        </thead>
        <tbody>
          {paddingTop > 0 && (
            <tr aria-hidden style={{ height: paddingTop }}>
              <td colSpan={colCount} />
            </tr>
          )}
          {virtualItems.map((vi) => {
            const t = tracks[vi.index]!;
            return (
              <TrackRow
                key={t.id}
                ref={virtualizer.measureElement}
                data-index={vi.index}
                track={t}
                index={vi.index}
                isPlaying={t.id === playingId}
                showAlbum={showAlbum}
                showCover={renderCover}
                onPlay={onPlay}
              />
            );
          })}
          {paddingBottom > 0 && (
            <tr aria-hidden style={{ height: paddingBottom }}>
              <td colSpan={colCount} />
            </tr>
          )}
        </tbody>
      </table>
    </div>
  );
}
