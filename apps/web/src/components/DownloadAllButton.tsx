// Bulk "download for offline" action for an album or playlist. The pin
// loop itself lives in cache/downloadTracks (the album ⋯ menu runs the
// same one); this is the hero-row button around it, adding inline N/M
// progress. Designed to drop into a hero `.actions` row next to the
// play / station / queue icon buttons.

import { Download } from "lucide-react";
import { useState } from "react";

import { useAudioCache } from "../cache/AudioCacheContext";
import { downloadTracks } from "../cache/downloadTracks";
import { formatBytes } from "../cache/format";
import { useToast } from "../toast/ToastContext";
import type { Track } from "../api/types";

export function DownloadAllButton({
  tracks,
  label = "download for offline",
}: {
  tracks: Track[];
  label?: string;
}) {
  const cache = useAudioCache();
  const toast = useToast();
  const [progress, setProgress] = useState<{ done: number; total: number } | null>(null);

  async function downloadAll() {
    if (progress || tracks.length === 0) return;
    setProgress({ done: 0, total: tracks.length });
    const result = await downloadTracks(
      cache,
      tracks.map((t) => t.id),
      (done, total) => setProgress({ done, total }),
    );
    setProgress(null);
    if (result.shortBy !== null) {
      toast(
        `Download budget full — short by ${formatBytes(result.shortBy)}. ` +
          `Raise it in Settings to finish.`,
        { variant: "error" },
      );
    } else if (result.failed > 0) {
      toast(
        `saved ${result.saved}/${tracks.length} for offline — ${result.failed} failed`,
        { variant: "error" },
      );
    } else {
      toast(`${result.saved} tracks saved for offline`, { variant: "success" });
    }
  }

  return (
    <>
      <button
        className="icon-btn"
        onClick={downloadAll}
        disabled={tracks.length === 0 || progress !== null}
        aria-label={label}
        title={label}
      >
        <Download size={18} strokeWidth={1.5} />
      </button>
      {progress && (
        <span className="text-fg-muted text-sm" style={{ fontVariantNumeric: "tabular-nums" }}>
          {progress.done}/{progress.total}
        </span>
      )}
    </>
  );
}
