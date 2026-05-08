import { useQuery } from "@tanstack/react-query";
import { listAlbums } from "../api/client";
import { AlbumCard } from "../components/AlbumCard";
import { Layout } from "../components/Layout";

export function Albums() {
  const q = useQuery({
    queryKey: ["albums", "newest", 60],
    queryFn: () => listAlbums({ type: "newest", size: 60 }),
  });

  return (
    <Layout>
      <h1 className="text-2xl font-semibold mb-6">Newest</h1>
      {q.isLoading && <p className="text-stone-400">loading…</p>}
      {q.error && (
        <p className="text-red-400">error: {(q.error as Error).message}</p>
      )}
      {q.data && (
        <div className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 gap-6">
          {q.data.map((a) => (
            <AlbumCard key={a.id} album={a} />
          ))}
        </div>
      )}
    </Layout>
  );
}
