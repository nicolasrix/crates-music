// Bulk "download for offline" action for an album or playlist. Pins each
// track via the AudioCacheContext, tolerating individual failures, and shows
// inline progress. Designed to drop into a hero `.actions` row next to the
// play / station / queue icon buttons.

import { Download } from "lucide-react";
import { useState } from "react";

import { useAudioCache } from "../cache/AudioCacheContext";
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
    let budgetHit = false;
    let failed = 0;
    for (let i = 0; i < tracks.length; i++) {
      try {
        const outcome = await cache.download(tracks[i]!.id);
        if (outcome.kind === "would-exceed-budget") {
          budgetHit = true;
          toast(
            `Download budget full — short by ${formatBytes(outcome.overBy)}. ` +
              `Raise it in Settings to finish.`,
            { variant: "error" },
          );
          break;
        }
      } catch {
        // Tolerate per-track failures (offline / catalog gap) but count
        // them — completing "N/N" while tracks are missing lies to the
        // user about what's actually playable offline.
        failed++;
      }
      setProgress({ done: i + 1, total: tracks.length });
    }
    setProgress(null);
    if (!budgetHit) {
      if (failed > 0) {
        toast(
          `saved ${tracks.length - failed}/${tracks.length} for offline — ${failed} failed`,
          { variant: "error" },
        );
      } else {
        toast(`${tracks.length} tracks saved for offline`, { variant: "success" });
      }
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
