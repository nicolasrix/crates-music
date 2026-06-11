// "Cache liked content" — a manual, bulk warm of the *regular* (auto-evicted)
// audio cache from everything the user has liked: liked songs, every track of
// liked albums, and every track of every album by liked artists.
//
// Distinct from DownloadAllButton, which *pins* (the never-evicted budget).
// This fills the regular budget via cache.cacheTrack(), so if the liked set
// exceeds that budget the cache keeps the most-recently-warmed budget-worth
// and the rest rolls off LRU — the normal auto-cache contract, no "budget
// full" failure (that's pin-only). Use it to pre-seed offline listening
// without committing pinned storage.
//
// Only track *ids* are needed (fetchTrackBlob keys off the id), so the
// album/artist expansion skips the cover-art hydration the Liked page does.

import { Download } from "lucide-react";
import { useState } from "react";

import { getAlbum, getArtist } from "../api/client";
import { getRatings } from "../api/library";
import { useAudioCache } from "../cache/AudioCacheContext";
import { useToast } from "../toast/ToastContext";

/** Resolve every liked song + liked-album track + liked-artist-album track
 *  into a deduped list of track ids. Individual album/artist lookups that fail
 *  (catalog gap, offline) are tolerated rather than failing the whole set. */
export async function collectLikedTrackIds(): Promise<string[]> {
  const rows = await getRatings();
  const likedIds = (kind: "track" | "album" | "artist") =>
    rows.filter((r) => r.kind === kind && r.rating === "like").map((r) => r.id);

  const ids = new Set<string>();

  // Liked songs — ids straight through.
  for (const id of likedIds("track")) ids.add(id);

  // Liked albums — expand to their tracks.
  const albums = await Promise.allSettled(likedIds("album").map((id) => getAlbum(id)));
  for (const a of albums) {
    if (a.status === "fulfilled") for (const t of a.value.tracks) ids.add(t.id);
  }

  // Liked artists — expand to their albums, then those albums' tracks.
  const artists = await Promise.allSettled(likedIds("artist").map((id) => getArtist(id)));
  const artistAlbumIds = artists.flatMap((r) =>
    r.status === "fulfilled" ? r.value.albums.map((al) => al.id) : [],
  );
  const artistAlbums = await Promise.allSettled(artistAlbumIds.map((id) => getAlbum(id)));
  for (const a of artistAlbums) {
    if (a.status === "fulfilled") for (const t of a.value.tracks) ids.add(t.id);
  }

  return [...ids];
}

type Phase =
  | { kind: "idle" }
  | { kind: "preparing" }
  | { kind: "running"; done: number; total: number; failed: number }
  | { kind: "done"; cached: number; total: number; failed: number };

export function CacheLikedButton() {
  const cache = useAudioCache();
  const toast = useToast();
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });

  async function run() {
    if (phase.kind === "preparing" || phase.kind === "running") return;
    setPhase({ kind: "preparing" });

    let ids: string[];
    try {
      ids = await collectLikedTrackIds();
    } catch {
      setPhase({ kind: "idle" });
      toast("Couldn't load your liked items — check your connection and try again.", {
        variant: "error",
      });
      return;
    }
    if (ids.length === 0) {
      setPhase({ kind: "idle" });
      toast("Nothing liked yet — like some songs, albums, or artists first.");
      return;
    }

    let failed = 0;
    for (let i = 0; i < ids.length; i++) {
      try {
        await cache.cacheTrack(ids[i]!);
      } catch {
        failed++; // offline / auth / catalog gap — skip and keep going
      }
      setPhase({ kind: "running", done: i + 1, total: ids.length, failed });
    }
    setPhase({ kind: "done", cached: ids.length - failed, total: ids.length, failed });
  }

  const busy = phase.kind === "preparing" || phase.kind === "running";

  return (
    <div style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap" }}>
      <button
        type="button"
        className="text-sm"
        onClick={() => void run()}
        disabled={busy}
        style={{
          display: "inline-flex",
          alignItems: "center",
          gap: 6,
          padding: "6px 10px",
          background: "var(--bg-elevated)",
          border: "1px solid var(--border)",
          borderRadius: "var(--radius-1, 2px)",
          color: "var(--fg)",
          cursor: busy ? "default" : "pointer",
          opacity: busy ? 0.6 : 1,
        }}
      >
        <Download size={14} strokeWidth={1.5} />
        cache liked content
      </button>
      <StatusText phase={phase} />
    </div>
  );
}

function StatusText({ phase }: { phase: Phase }) {
  const tabular = { fontVariantNumeric: "tabular-nums" as const };
  if (phase.kind === "preparing")
    return (
      <span className="text-fg-muted text-sm">gathering liked tracks…</span>
    );
  if (phase.kind === "running")
    return (
      <span className="text-fg-muted text-sm" style={tabular}>
        caching {phase.done}/{phase.total}
        {phase.failed > 0 ? ` · ${phase.failed} skipped` : ""}…
      </span>
    );
  if (phase.kind === "done")
    return (
      <span className="text-fg-muted text-sm" style={tabular}>
        cached {phase.cached} of {phase.total} liked track
        {phase.total === 1 ? "" : "s"}
        {phase.failed > 0 ? ` · ${phase.failed} couldn't be fetched` : ""}.
      </span>
    );
  return null;
}
