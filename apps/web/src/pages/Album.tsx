import { useQuery } from "@tanstack/react-query";
import { coverArtUrl, getAlbum } from "../api/client";
import { Layout } from "../components/Layout";
import { useSync } from "../sync/SyncContext";
import type { Track } from "../api/types";

export function Album({ id }: { id: string }) {
  const q = useQuery({
    queryKey: ["album", id],
    queryFn: () => getAlbum(id),
  });
  const sync = useSync();

  function playFrom(tracks: Track[], startIndex: number) {
    // Clear → push N → set cursor → play. The gateway linearizes;
    // local state catches up via the broadcast `applied` frames.
    sync.submit({ type: "clear" });
    for (const t of tracks) sync.pushTrack(t);
    sync.submit({ type: "set_now_playing", index: startIndex });
    sync.submit({ type: "set_playing", is_playing: true });
  }

  if (q.isLoading) {
    return (
      <Layout>
        <p className="text-stone-400">loading…</p>
      </Layout>
    );
  }
  if (q.error || !q.data) {
    return (
      <Layout>
        <p className="text-red-400">
          error: {(q.error as Error | undefined)?.message ?? "not found"}
        </p>
      </Layout>
    );
  }

  const { album, tracks } = q.data;
  const cover = coverArtUrl(album.coverArt, 600);

  return (
    <Layout>
      <div className="flex flex-col md:flex-row gap-6 mb-8">
        <div className="w-48 h-48 md:w-64 md:h-64 bg-stone-800 rounded overflow-hidden shrink-0">
          {cover && (
            <img src={cover} alt={album.name} className="w-full h-full object-cover" />
          )}
        </div>
        <div>
          <h1 className="text-3xl font-semibold">{album.name}</h1>
          <div className="text-stone-400 mt-1">
            {album.artist ?? "—"}
            {album.year ? ` · ${album.year}` : ""}
          </div>
          <button
            onClick={() => playFrom(tracks, 0)}
            className="mt-4 px-4 py-2 rounded bg-stone-200 text-stone-900 hover:bg-white"
          >
            Play album
          </button>
        </div>
      </div>

      <table className="w-full text-left">
        <thead className="text-stone-400 text-sm border-b border-stone-800">
          <tr>
            <th className="w-10 py-2">#</th>
            <th className="py-2">Title</th>
            <th className="py-2 hidden md:table-cell">Artist</th>
            <th className="py-2 w-16 text-right">Time</th>
          </tr>
        </thead>
        <tbody>
          {tracks.map((t, i) => (
            <tr
              key={t.id}
              onDoubleClick={() => playFrom(tracks, i)}
              className="border-b border-stone-900 hover:bg-stone-900 cursor-pointer"
            >
              <td className="py-2 text-stone-500">{t.track ?? i + 1}</td>
              <td className="py-2">{t.title}</td>
              <td className="py-2 text-stone-400 hidden md:table-cell">
                {t.artist ?? "—"}
              </td>
              <td className="py-2 text-stone-400 text-right">
                {t.duration ? formatDuration(t.duration) : "—"}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </Layout>
  );
}

function formatDuration(seconds: number): string {
  const m = Math.floor(seconds / 60);
  const s = seconds % 60;
  return `${m}:${String(s).padStart(2, "0")}`;
}
