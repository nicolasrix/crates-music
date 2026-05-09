import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { useEffect, useRef } from "react";
import {
  listMostPlayedTracks,
  listRandomTracks,
  listRecentTracks,
  listTracksPage,
} from "../api/client";
import { Layout } from "../components/Layout";
import { TrackTable } from "../components/TrackTable";
import { VirtualTrackTable } from "../components/VirtualTrackTable";
import { usePlayback } from "../sync/usePlayback";
import type { Track } from "../api/types";
import { ListMode, MODE_LABEL } from "./listMode";

// Page size for the all-tracks paginator. 200 is a reasonable bite —
// big enough that a 1000-track library is one click away from fully
// loaded, small enough that each fetch returns in a fraction of a
// second on a single-user gateway.
const ALL_PAGE_SIZE = 200;

// Highlight modes (recent / most_played / random) are bounded views,
// not paginated — they show a single page sized to fit the heading.
const HIGHLIGHT_PAGE_SIZE: Record<Exclude<ListMode, "all">, number> = {
  recent: 200,
  most_played: 100,
  random: 100,
};

export function Tracks({ mode = "all" }: { mode?: ListMode }) {
  if (mode === "all") return <TracksAll />;
  return <TracksHighlight mode={mode} />;
}

function TracksAll() {
  const q = useInfiniteQuery({
    queryKey: ["tracks", "all"],
    queryFn: ({ pageParam }) =>
      listTracksPage({ size: ALL_PAGE_SIZE, offset: pageParam as number }),
    initialPageParam: 0,
    // A page shorter than ALL_PAGE_SIZE means we've reached the tail
    // (search3 has no total field). Returning undefined flips
    // hasNextPage to false.
    getNextPageParam: (lastPage, allPages) =>
      lastPage.length < ALL_PAGE_SIZE
        ? undefined
        : allPages.length * ALL_PAGE_SIZE,
    staleTime: 60_000,
  });
  const { playSingle } = usePlayback();
  const tracks: Track[] = q.data?.pages.flat() ?? [];

  // Prefetch sentinel. An IntersectionObserver fires next-page fetches
  // when the user scrolls within ~400 px of the load-more button. The
  // explicit button stays as a fallback (e.g. for keyboard users who
  // don't scroll past the bottom). We capture the latest query state
  // in a ref so the observer callback always sees current values
  // without re-binding on every render.
  const sentinelRef = useRef<HTMLDivElement>(null);
  const queryStateRef = useRef(q);
  queryStateRef.current = q;

  useEffect(() => {
    if (!q.hasNextPage) return;
    const el = sentinelRef.current;
    if (!el) return;
    const observer = new IntersectionObserver(
      (entries) => {
        const entry = entries[0];
        if (!entry || !entry.isIntersecting) return;
        const s = queryStateRef.current;
        if (s.hasNextPage && !s.isFetchingNextPage) {
          void s.fetchNextPage();
        }
      },
      { rootMargin: "400px" }
    );
    observer.observe(el);
    return () => observer.disconnect();
  }, [q.hasNextPage]);

  return (
    <Layout breadcrumb={`tracks · ${MODE_LABEL.all}`}>
      <div className="section">
        <div className="section-head">
          <h2>{MODE_LABEL.all}</h2>
          {tracks.length > 0 && (
            <span className="count tabular">{tracks.length} loaded</span>
          )}
        </div>
        <p className="lead">every track in your library.</p>
        {q.isLoading && <p className="text-fg-muted text-sm">loading…</p>}
        {q.error && (
          <p className="text-danger text-sm">
            error: {(q.error as Error).message}
          </p>
        )}
        {tracks.length > 0 && (
          <VirtualTrackTable
            tracks={tracks}
            showAlbum
            onPlay={(i) => playSingle(tracks[i]!)}
          />
        )}
        {q.hasNextPage && (
          <div className="load-more-wrap">
            <div ref={sentinelRef} aria-hidden className="load-more-sentinel" />
            <button
              className="load-more"
              onClick={() => void q.fetchNextPage()}
              disabled={q.isFetchingNextPage}
              type="button"
            >
              {q.isFetchingNextPage ? "loading…" : "load more"}
            </button>
          </div>
        )}
      </div>
    </Layout>
  );
}

function TracksHighlight({ mode }: { mode: Exclude<ListMode, "all"> }) {
  const fetcher = HIGHLIGHT_FETCHERS[mode];
  const q = useQuery({
    queryKey: ["tracks", mode],
    queryFn: fetcher,
    staleTime: mode === "random" ? 0 : 60_000,
    refetchOnMount: mode === "random" ? "always" : true,
  });
  const { playSingle } = usePlayback();

  const lead =
    mode === "recent"
      ? "tracks added recently."
      : mode === "random"
        ? "a fresh shuffle of your library on every visit."
        : "tracks you've listened to most.";

  return (
    <Layout breadcrumb={`tracks · ${MODE_LABEL[mode]}`}>
      <div className="section">
        <div className="section-head">
          <h2>{MODE_LABEL[mode]}</h2>
          {q.data && <span className="count tabular">{q.data.length}</span>}
        </div>
        <p className="lead">{lead}</p>
        {q.isLoading && <p className="text-fg-muted text-sm">loading…</p>}
        {q.error && (
          <p className="text-danger text-sm">
            error: {(q.error as Error).message}
          </p>
        )}
        {q.data && q.data.length === 0 && !q.isLoading && (
          <p className="text-fg-muted text-sm">
            {mode === "most_played"
              ? "no plays recorded yet — once tracks have been listened to, the most-played tracks will show up here."
              : "no tracks to show."}
          </p>
        )}
        {q.data && q.data.length > 0 && (
          <TrackTable
            tracks={q.data}
            showAlbum
            onPlay={(i) => playSingle(q.data![i]!)}
          />
        )}
      </div>
    </Layout>
  );
}

const HIGHLIGHT_FETCHERS: Record<
  Exclude<ListMode, "all">,
  () => Promise<Track[]>
> = {
  recent: () => listRecentTracks(HIGHLIGHT_PAGE_SIZE.recent),
  random: () => listRandomTracks(HIGHLIGHT_PAGE_SIZE.random),
  most_played: () => listMostPlayedTracks(HIGHLIGHT_PAGE_SIZE.most_played),
};
