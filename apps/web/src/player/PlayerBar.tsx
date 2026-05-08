import { usePlayer } from "./PlayerContext";

export function PlayerBar() {
  const { queue, index, playing, togglePlay, next, prev } = usePlayer();
  const track = queue[index];
  if (!track) return null;
  return (
    <div className="fixed inset-x-0 bottom-0 border-t border-stone-800 bg-stone-900/95 backdrop-blur p-3 flex items-center gap-4">
      <div className="min-w-0 flex-1">
        <div className="truncate font-medium">{track.title}</div>
        <div className="truncate text-sm text-stone-400">
          {track.artist ?? "—"} · {track.album ?? "—"}
        </div>
      </div>
      <div className="flex items-center gap-2">
        <button
          onClick={prev}
          disabled={index === 0}
          className="px-2 py-1 rounded hover:bg-stone-800 disabled:opacity-40"
        >
          ‹
        </button>
        <button
          onClick={togglePlay}
          className="px-3 py-1 rounded bg-stone-200 text-stone-900 hover:bg-white"
        >
          {playing ? "Pause" : "Play"}
        </button>
        <button
          onClick={next}
          disabled={index >= queue.length - 1}
          className="px-2 py-1 rounded hover:bg-stone-800 disabled:opacity-40"
        >
          ›
        </button>
      </div>
    </div>
  );
}
