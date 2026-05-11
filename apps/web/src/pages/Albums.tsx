import { useQuery } from "@tanstack/react-query";
import { listAlbums, listAllAlbums } from "../api/client";
import { AlbumCard } from "../components/AlbumCard";
import { Layout } from "../components/Layout";
import { ListMode, MODE_ALBUM_TYPE, MODE_LABEL } from "./listMode";

const HIGHLIGHT_SIZE = 60;

export function Albums({ mode = "all" }: { mode?: ListMode }) {
  const subsonicType = MODE_ALBUM_TYPE[mode];
  // "all" paginates through the entire alphabetical listing; sub-modes
  // are bounded "highlight" views (top 60 by their respective ordering).
  const q = useQuery({
    queryKey: ["albums", subsonicType, mode === "all" ? "all" : HIGHLIGHT_SIZE],
    queryFn: () =>
      mode === "all"
        ? listAllAlbums(subsonicType)
        : listAlbums({ type: subsonicType, size: HIGHLIGHT_SIZE }),
    // Random shouldn't hit the cache — every visit should reshuffle.
    staleTime: mode === "random" ? 0 : 60_000,
    refetchOnMount: mode === "random" ? "always" : true,
  });

  const refreshedAt = q.dataUpdatedAt
    ? humanizeAge(Date.now() - q.dataUpdatedAt)
    : null;

  return (
    <Layout breadcrumb={`albums · ${MODE_LABEL[mode]}`}>
      <div className="section">
        <div className="section-head">
          <h2>{MODE_LABEL[mode]}</h2>
          {q.data && (
            <span className="count tabular">
              {mode === "all"
                ? `${q.data.length} albums`
                : `${q.data.length} of ${HIGHLIGHT_SIZE}`}
            </span>
          )}
        </div>
        {refreshedAt && mode !== "random" && (
          <p className="lead">refreshed from the gateway {refreshedAt}.</p>
        )}
        {q.isLoading && <p className="text-fg-muted text-sm">loading…</p>}
        {q.error && (
          <p className="text-danger text-sm">
            error: {(q.error as Error).message}
          </p>
        )}
        {q.data && q.data.length === 0 && !q.isLoading && (
          <EmptyState mode={mode} />
        )}
        {q.data && q.data.length > 0 && (
          <div className="tile-grid">
            {q.data.map((a) => (
              <AlbumCard key={a.id} album={a} />
            ))}
          </div>
        )}
      </div>
    </Layout>
  );
}

function EmptyState({ mode }: { mode: ListMode }) {
  const message =
    mode === "most_played"
      ? "no plays recorded yet — once tracks have been listened to, the most-played albums will show up here."
      : "no albums to show.";
  return <p className="text-fg-muted text-sm">{message}</p>;
}

function humanizeAge(ms: number): string {
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${s} seconds ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} minute${m === 1 ? "" : "s"} ago`;
  const h = Math.floor(m / 60);
  return `${h} hour${h === 1 ? "" : "s"} ago`;
}
