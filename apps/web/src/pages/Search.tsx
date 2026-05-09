// Library-wide search results. Reads ?q= from the URL — the sidebar input
// owns the input field and navigates here on submit. Keeping the input
// outside this page means the search box stays mounted (and focused)
// across navigation between /search?q=… and other routes.
//
// Layout is a single-screen overview: artists and albums sit side-by-side
// at the top, tracks span full width below. Each section is capped so
// the entire page is visible without scrolling — top 3 hero cards plus
// (for tracks) a small table of next-best results. "see all N →" links
// route to /search/{artists,albums,tracks}?q=… for full bucket browsing.

import { useQuery } from "@tanstack/react-query";
import { useMemo } from "react";
import { searchAll } from "../api/client";
import { AlbumCard } from "../components/AlbumCard";
import { ArtistCard } from "../components/ArtistCard";
import { Layout } from "../components/Layout";
import { TrackHeroCard } from "../components/TrackHeroCard";
import { TrackTable } from "../components/TrackTable";
import { Link, useRoute } from "../router";
import { useSync } from "../sync/SyncContext";
import { playSingle } from "../sync/playbackHelpers";
import { rankResults } from "./searchRanking";

const TOP_RESULTS = 3;
// In the compact overview, tracks get a small table after the hero
// strip. Tile-style buckets (artists/albums) show only the hero —
// adding more tiles below would push the next section off-screen,
// defeating the "all three sections visible at once" goal.
const TRACKS_REST_LIMIT = 5;

export function Search() {
  const { search } = useRoute();
  const query = new URLSearchParams(search).get("q")?.trim() ?? "";
  const sync = useSync();

  const q = useQuery({
    queryKey: ["search", query],
    queryFn: () => searchAll(query),
    // No keystroke debounce here — the input doesn't navigate per keystroke.
    // Once we land on /search?q=foo, the query is stable until the user
    // submits a new term. Don't refetch on focus.
    enabled: query.length > 0,
    staleTime: 60_000,
    refetchOnWindowFocus: false,
  });

  // Re-rank + derive every time the underlying data or the query changes.
  // Cheap (O(n) over a few hundred items at most), deterministic, and
  // keeping it out of useQuery keeps the cache key honest — the same
  // raw search3 response is reused across re-renders.
  const ranked = useMemo(
    () => (q.data ? rankResults(q.data, query) : null),
    [q.data, query]
  );

  if (query.length === 0) {
    return (
      <Layout breadcrumb="search">
        <div className="section">
          <div className="section-head">
            <h2>search</h2>
          </div>
          <p className="lead">
            type a query in the sidebar to search your library.
          </p>
        </div>
      </Layout>
    );
  }

  const totalHits = ranked
    ? ranked.artists.length + ranked.albums.length + ranked.tracks.length
    : 0;
  const qParam = `?q=${encodeURIComponent(query)}`;

  return (
    <Layout breadcrumb={`search · ${query}`}>
      <div className="section">
        <div className="section-head">
          <h2>search</h2>
          {ranked && <span className="count tabular">{totalHits} hits</span>}
        </div>
        <p className="lead">
          results for <span className="font-mono">“{query}”</span>.
        </p>

        {q.isLoading && <p className="text-fg-muted text-sm">searching…</p>}
        {q.error && (
          <p className="text-danger text-sm">
            error: {(q.error as Error).message}
          </p>
        )}
      </div>

      {ranked && (() => {
        const artistsSection = ranked.artists.length > 0 && (
          <SectionWithTop
            heading="artists"
            items={ranked.artists}
            restLimit={0}
            seeAllHref={`/search/artists${qParam}`}
            renderHero={(a) => <ArtistCard key={a.id} artist={a} />}
            renderRest={() => null}
          />
        );
        const albumsSection = ranked.albums.length > 0 && (
          <SectionWithTop
            heading="albums"
            items={ranked.albums}
            restLimit={0}
            seeAllHref={`/search/albums${qParam}`}
            renderHero={(a) => <AlbumCard key={a.id} album={a} />}
            renderRest={() => null}
          />
        );
        // Two-column wrap only when both buckets have content. Otherwise
        // a single bucket flows full-width like a normal section so the
        // page doesn't end up with a half-empty grid row.
        if (artistsSection && albumsSection) {
          return (
            <div className="search-grid">
              {artistsSection}
              {albumsSection}
            </div>
          );
        }
        return (
          <>
            {artistsSection}
            {albumsSection}
          </>
        );
      })()}

      {ranked && ranked.tracks.length > 0 && (
        <SectionWithTop
          heading="tracks"
          items={ranked.tracks}
          heroVariant="rows"
          restLimit={TRACKS_REST_LIMIT}
          seeAllHref={`/search/tracks${qParam}`}
          renderHero={(t) => (
            <TrackHeroCard
              key={t.id}
              track={t}
              onPlay={() => playSingle(sync, t)}
            />
          )}
          renderRest={(rest) => {
            // TrackTable types `tracks` as mutable; the slice we pass in
            // is read-only. Spread to a fresh array to satisfy the
            // signature without changing TrackTable.
            const arr = [...rest];
            return (
              <TrackTable
                tracks={arr}
                showAlbum
                onPlay={(i) => playSingle(sync, arr[i]!)}
              />
            );
          }}
        />
      )}

      {ranked && totalHits === 0 && !q.isLoading && (
        <div className="section">
          <p className="text-fg-muted text-sm">no results.</p>
        </div>
      )}
    </Layout>
  );
}

// One section with a "top results" hero strip plus a capped remainder
// and a "see all" link when there's more behind the cap. Keeps the
// three sections (artists / albums / tracks) consistent without
// repeating the splitting + see-all logic at every call site.
function SectionWithTop<T>({
  heading,
  items,
  renderHero,
  renderRest,
  heroVariant = "tiles",
  restLimit,
  seeAllHref,
}: {
  heading: string;
  items: readonly T[];
  renderHero: (item: T) => React.ReactNode;
  renderRest: (items: readonly T[]) => React.ReactNode;
  /** "tiles" — vertical cards (artists/albums); columns capped at
   *  ~240px so covers don't dominate. "rows" — horizontal cards
   *  (tracks); cells stretch within a capped strip width. */
  heroVariant?: "tiles" | "rows";
  /** How many items to render after the hero strip. 0 = none. Items
   *  beyond TOP_RESULTS + restLimit are reachable via seeAllHref. */
  restLimit: number;
  /** Destination for the "see all N →" link in the section head. Shown
   *  only when items.length exceeds what the section displays. */
  seeAllHref: string;
}) {
  const top = items.slice(0, TOP_RESULTS);
  const rest = items.slice(TOP_RESULTS, TOP_RESULTS + restLimit);
  const visibleCount = top.length + rest.length;
  const hasMore = items.length > visibleCount;
  const stripClass =
    heroVariant === "rows" ? "hero-strip is-rows" : "hero-strip";
  return (
    <div className="section">
      <div className="section-head">
        <h2>{heading}</h2>
        {hasMore ? (
          <Link to={seeAllHref} className="section-more">
            see all {items.length} →
          </Link>
        ) : (
          <span className="count tabular">{items.length}</span>
        )}
      </div>
      <div className={stripClass}>{top.map((item) => renderHero(item))}</div>
      {rest.length > 0 && renderRest(rest)}
    </div>
  );
}
