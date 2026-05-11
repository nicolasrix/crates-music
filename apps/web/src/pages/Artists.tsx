import { useQuery } from "@tanstack/react-query";
import { listAlbums, listArtists } from "../api/client";
import { ArtistCard } from "../components/ArtistCard";
import { Layout } from "../components/Layout";
import type { Album, Artist } from "../api/types";
import { ListMode, MODE_ALBUM_TYPE, MODE_LABEL } from "./listMode";

// Subsonic doesn't have per-mode artist endpoints, so we derive every
// non-default mode from the equivalent album list. This is the right
// reading: "recent artists" = artists with the freshest releases,
// "most played artists" = artists with the most-played albums, etc.

export function Artists({ mode = "all" }: { mode?: ListMode }) {
  // The default "all" view is the canonical alphabetical artist list —
  // the natural a–z library browse. Sub-modes (recent, most-played,
  // random) are derived from album lists since Subsonic doesn't have
  // per-mode artist endpoints.
  if (mode === "all") return <ArtistsAlphabetical />;
  return <ArtistsDerived mode={mode} />;
}

function ArtistsAlphabetical() {
  const q = useQuery({
    queryKey: ["artists"],
    queryFn: listArtists,
  });
  return (
    <Layout breadcrumb={`artists · ${MODE_LABEL.all}`}>
      <div className="section">
        <div className="section-head">
          <h2>{MODE_LABEL.all}</h2>
          {q.data && (
            <span className="count tabular">{q.data.length} total</span>
          )}
        </div>
        {q.isLoading && <p className="text-fg-muted text-sm">loading…</p>}
        {q.error && (
          <p className="text-danger text-sm">
            error: {(q.error as Error).message}
          </p>
        )}
        {q.data && (
          <div className="tile-grid">
            {q.data.map((a) => (
              <ArtistCard key={a.id} artist={a} />
            ))}
          </div>
        )}
      </div>
    </Layout>
  );
}

function ArtistsDerived({ mode }: { mode: Exclude<ListMode, "all"> }) {
  const subsonicType = MODE_ALBUM_TYPE[mode];
  const albumsQ = useQuery({
    queryKey: ["albums", subsonicType, 60],
    queryFn: () => listAlbums({ type: subsonicType, size: 60 }),
    staleTime: mode === "random" ? 0 : 60_000,
    refetchOnMount: mode === "random" ? "always" : true,
  });
  // Canonical list joined for cover art / album counts — same trick as Home.
  const artistsQ = useQuery({
    queryKey: ["artists"],
    queryFn: listArtists,
    staleTime: 5 * 60_000,
  });

  const derived = deriveArtistsFromAlbums(
    albumsQ.data ?? [],
    artistsQ.data ?? [],
    60
  );

  return (
    <Layout breadcrumb={`artists · ${MODE_LABEL[mode]}`}>
      <div className="section">
        <div className="section-head">
          <h2>{MODE_LABEL[mode]}</h2>
          {derived.length > 0 && (
            <span className="count tabular">{derived.length}</span>
          )}
        </div>
        {albumsQ.isLoading && (
          <p className="text-fg-muted text-sm">loading…</p>
        )}
        {albumsQ.error && (
          <p className="text-danger text-sm">
            error: {(albumsQ.error as Error).message}
          </p>
        )}
        {!albumsQ.isLoading && derived.length === 0 && (
          <p className="text-fg-muted text-sm">
            {mode === "most_played"
              ? "no plays recorded yet — once tracks have been listened to, the most-played artists will show up here."
              : "no artists to show."}
          </p>
        )}
        {derived.length > 0 && (
          <div className="tile-grid">
            {derived.map((a) => (
              <ArtistCard key={a.id} artist={a} />
            ))}
          </div>
        )}
      </div>
    </Layout>
  );
}

function deriveArtistsFromAlbums(
  albums: Album[],
  canonical: Artist[],
  limit: number
): Artist[] {
  const lookup = new Map<string, Artist>();
  for (const a of canonical) lookup.set(a.id, a);
  const seen = new Set<string>();
  const out: Artist[] = [];
  for (const a of albums) {
    if (!a.artistId || !a.artist) continue;
    if (seen.has(a.artistId)) continue;
    seen.add(a.artistId);
    const cn = lookup.get(a.artistId);
    out.push({
      id: a.artistId,
      name: cn?.name ?? a.artist,
      ...(cn?.coverArt ?? a.coverArt
        ? { coverArt: cn?.coverArt ?? a.coverArt }
        : {}),
      ...(cn?.albumCount !== undefined ? { albumCount: cn.albumCount } : {}),
    });
    if (out.length >= limit) break;
  }
  return out;
}
