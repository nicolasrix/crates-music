// Library-wide search results. Reads ?q= from the URL — the sidebar input
// owns the input field and navigates here on submit. Keeping the input
// outside this page means the search box stays mounted (and focused)
// across navigation between /search?q=… and other routes.

import { useQuery } from "@tanstack/react-query";
import { searchAll } from "../api/client";
import { AlbumCard } from "../components/AlbumCard";
import { ArtistCard } from "../components/ArtistCard";
import { Layout } from "../components/Layout";
import { TrackTable } from "../components/TrackTable";
import { useRoute } from "../router";
import { useSync } from "../sync/SyncContext";
import { playSingle } from "../sync/playbackHelpers";

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

  if (query.length === 0) {
    return (
      <Layout breadcrumb="search">
        <div className="section">
          <div className="section-head">
            <h2>search</h2>
          </div>
          <p className="lead">type a query in the sidebar to search your library.</p>
        </div>
      </Layout>
    );
  }

  const r = q.data;
  const totalHits = r ? r.artists.length + r.albums.length + r.tracks.length : 0;

  return (
    <Layout breadcrumb={`search · ${query}`}>
      <div className="section">
        <div className="section-head">
          <h2>search</h2>
          {r && <span className="count tabular">{totalHits} hits</span>}
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

      {r && r.artists.length > 0 && (
        <div className="section">
          <div className="section-head">
            <h2>artists</h2>
            <span className="count tabular">{r.artists.length}</span>
          </div>
          <div className="tile-grid">
            {r.artists.map((a) => (
              <ArtistCard key={a.id} artist={a} />
            ))}
          </div>
        </div>
      )}

      {r && r.albums.length > 0 && (
        <div className="section">
          <div className="section-head">
            <h2>albums</h2>
            <span className="count tabular">{r.albums.length}</span>
          </div>
          <div className="tile-grid">
            {r.albums.map((a) => (
              <AlbumCard key={a.id} album={a} />
            ))}
          </div>
        </div>
      )}

      {r && r.tracks.length > 0 && (
        <div className="section">
          <div className="section-head">
            <h2>tracks</h2>
            <span className="count tabular">{r.tracks.length}</span>
          </div>
          <TrackTable
            tracks={r.tracks}
            showAlbum
            onPlay={(i) => playSingle(sync, r.tracks[i]!)}
          />
        </div>
      )}

      {r && totalHits === 0 && !q.isLoading && (
        <div className="section">
          <p className="text-fg-muted text-sm">no results.</p>
        </div>
      )}
    </Layout>
  );
}
