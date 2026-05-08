import { usePlayer } from "./PlayerContext";

export function PlayerBar() {
  const { nowPlaying, isPlaying, togglePlay, next, prev, hasNext, hasPrev } = usePlayer();
  if (!nowPlaying) return null;
  return (
    <div className="fixed inset-x-0 bottom-0 border-t border-stone-800 bg-stone-900/95 backdrop-blur p-3 flex items-center gap-4">
      <div className="min-w-0 flex-1">
        <div className="truncate font-medium">{nowPlaying.title}</div>
        <div className="truncate text-sm text-stone-400">
          {nowPlaying.artist ?? "—"} · {nowPlaying.album ?? "—"}
        </div>
      </div>
      <div className="flex items-center gap-2">
        <button
          onClick={prev}
          disabled={!hasPrev}
          className="px-2 py-1 rounded hover:bg-stone-800 disabled:opacity-40"
        >
          ‹
        </button>
        <button
          onClick={togglePlay}
          className="px-3 py-1 rounded bg-stone-200 text-stone-900 hover:bg-white"
        >
          {isPlaying ? "Pause" : "Play"}
        </button>
        <button
          onClick={next}
          disabled={!hasNext}
          className="px-2 py-1 rounded hover:bg-stone-800 disabled:opacity-40"
        >
          ›
        </button>
      </div>
    </div>
  );
}
