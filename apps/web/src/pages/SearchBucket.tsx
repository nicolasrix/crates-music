// Per-bucket search results: /search/artists, /search/albums, /search/tracks.
// Reached via the "see all N →" link on the main /search page when a
// bucket has more results than the compact overview shows.
//
// Same data pipeline as <Search> (searchAll → server-ranked results,
// hydrated), but with raised caps so larger libraries actually return
// more than the 20/40/60 the overview uses. The bucket pages
// intentionally don't paginate beyond what one /v1/search call returns —
// searching is already a typo-tolerance / discovery surface, not a way to
// browse the whole library; that's what /albums, /tracks, /artists are for.

import { useQuery } from "@tanstack/react-query";
import { useMemo } from "react";
import { listArtists, searchAll } from "../api/client";
import { AlbumTable } from "../components/AlbumTable";
import { ArtistTable } from "../components/ArtistTable";
import { Layout } from "../components/Layout";
import { TrackTable } from "../components/TrackTable";
import { useRoute } from "../router";
import { usePlayback } from "../sync/usePlayback";

export type BucketKind = "artists" | "albums" | "tracks";

const HEADING: Record<BucketKind, string> = {
  artists: "artists",
  albums: "albums",
  tracks: "tracks",
};

// Expanded caps for the bucket pages. Different cache key from the
// overview so the two coexist — overview stays snappy with its small
// payload, bucket pages get the bigger one.
const EXPANDED_OPTS = {
  artistCount: 100,
  albumCount: 100,
  songCount: 200,
};

export function SearchBucket({ bucket }: { bucket: BucketKind }) {
  const { search } = useRoute();
  const query = new URLSearchParams(search).get("q")?.trim() ?? "";
  const { playSingle, playAlbum } = usePlayback();

  const q = useQuery({
    queryKey: ["search", query, "expanded"],
    queryFn: () => searchAll(query, EXPANDED_OPTS),
    enabled: query.length > 0,
    staleTime: 60_000,
    refetchOnWindowFocus: false,
  });

  // Same canonical-artist registry as the overview. Without this,
  // derived artists (those synthesized from track/album hits) show
  // "—" for albumCount because the search response only carries it on
  // named artist hits — and derived ones aren't among those.
  const knownArtistsQ = useQuery({
    queryKey: ["artists"],
    queryFn: listArtists,
    staleTime: 5 * 60_000,
  });

  // Render the gateway's server-ranked order as-is (re-ranking here would
  // re-drop the typo hits /v1/search recovered); only hydrate artists from
  // the canonical registry to fill fields the response omits (albumCount).
  const ranked = useMemo(() => {
    if (!q.data) return null;
    const known = new Map((knownArtistsQ.data ?? []).map((a) => [a.id, a]));
    return {
      ...q.data,
      artists: q.data.artists.map((a) => {
        const k = known.get(a.id);
        return k ? { ...k, ...a } : a;
      }),
    };
  }, [q.data, knownArtistsQ.data]);

  if (query.length === 0) {
    return (
      <Layout breadcrumb={`search · ${HEADING[bucket]}`}>
        <div className="section">
          <div className="section-head">
            <h2>{HEADING[bucket]}</h2>
          </div>
          <p className="lead">no query — type a term in the sidebar.</p>
        </div>
      </Layout>
    );
  }

  const items = ranked?.[bucket] ?? [];

  return (
    <Layout breadcrumb={`search · ${query} · ${HEADING[bucket]}`}>
      <div className="section">
        <div className="section-head">
          <h2>{HEADING[bucket]}</h2>
          {ranked && (
            <span className="count tabular">{items.length}</span>
          )}
        </div>
        <p className="lead">
          {HEADING[bucket]} matching{" "}
          <span className="font-mono">“{query}”</span>.
        </p>

        {q.isLoading && <p className="text-fg-muted text-sm">searching…</p>}
        {q.error && (
          <p className="text-danger text-sm">
            error: {(q.error as Error).message}
          </p>
        )}
        {ranked && items.length === 0 && !q.isLoading && (
          <p className="text-fg-muted text-sm">no results.</p>
        )}
      </div>

      {bucket === "artists" && items.length > 0 && (
        <div className="section">
          <ArtistTable artists={ranked!.artists} />
        </div>
      )}

      {bucket === "albums" && items.length > 0 && (
        <div className="section">
          <AlbumTable
            albums={ranked!.albums}
            onPlayAlbum={(a) => void playAlbum(a.id)}
          />
        </div>
      )}

      {bucket === "tracks" && items.length > 0 && (
        <div className="section">
          <TrackTable
            tracks={[...ranked!.tracks]}
            showAlbum
            onPlay={(i) => playSingle(ranked!.tracks[i]!)}
          />
        </div>
      )}
    </Layout>
  );
}
