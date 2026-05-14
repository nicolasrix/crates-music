// Natural-language "playlist for ___" surface.
//
// Wires the gateway's /v1/recommend/station endpoint: user types a
// prompt, the gateway calls the CLAP text encoder, and the same content
// ANN that powers /v1/recommend/next returns the top N tracks.
//
// State machine mirrors Album.tsx's station-from-album: a discriminated
// union with idle / loading / unavailable / error so the render branch
// stays exhaustive. We don't cache results with TanStack Query — text
// prompts are one-shot user gestures, and a cached "sunny afternoon"
// from yesterday would silently mask newly-ingested tracks.

import { Radio } from "lucide-react";
import { useState } from "react";

import {
  EmbedderUnavailableError,
  fetchTextStation,
} from "../api/recommend";
import { getSong } from "../api/client";
import { Layout } from "../components/Layout";
import { TrackTable } from "../components/TrackTable";
import type { Track } from "../api/types";
import { usePlayback } from "../sync/usePlayback";

const DEFAULT_N = 20;
const MIN_N = 5;
const MAX_N = 50;

type Status =
  | { kind: "idle" }
  | { kind: "loading" }
  | { kind: "ready"; query: string; tracks: Track[] }
  | { kind: "empty"; query: string }
  | { kind: "unavailable" }
  | { kind: "error"; message: string };

export function Station() {
  const [text, setText] = useState("");
  const [n, setN] = useState(DEFAULT_N);
  const [status, setStatus] = useState<Status>({ kind: "idle" });
  const { playList } = usePlayback();

  async function run() {
    const trimmed = text.trim();
    if (trimmed.length === 0) return;
    setStatus({ kind: "loading" });
    try {
      const rec = await fetchTextStation(trimmed, n);
      // Hydrate ids → Track shapes. Track lookups can 404 (catalog
      // drift); drop those rather than aborting — partial results are
      // still useful, and the user picked the prompt, not the tracks.
      const settled = await Promise.all(
        rec.results.map((r) => getSong(r.track_id).catch(() => null)),
      );
      const tracks = settled.filter((t): t is Track => t !== null);
      if (tracks.length === 0) {
        setStatus({ kind: "empty", query: trimmed });
        return;
      }
      setStatus({ kind: "ready", query: trimmed, tracks });
    } catch (e) {
      if (e instanceof EmbedderUnavailableError) {
        setStatus({ kind: "unavailable" });
      } else {
        setStatus({ kind: "error", message: (e as Error).message });
      }
    }
  }

  function onSubmit(e: React.FormEvent) {
    e.preventDefault();
    void run();
  }

  return (
    <Layout breadcrumb="station">
      <div className="section">
        <div className="section-head">
          <h2>
            <Radio
              size={18}
              strokeWidth={1.5}
              style={{ verticalAlign: "-3px", marginRight: 8 }}
            />
            playlist from a prompt
          </h2>
        </div>
        <p className="text-fg-muted text-sm" style={{ marginBottom: 12 }}>
          describe the mood, era, vibe — anything the CLAP text encoder
          can map into the same space as the audio: "sunny afternoon",
          "late night drive", "raw 90s indie".
        </p>

        <form
          onSubmit={onSubmit}
          style={{ display: "flex", gap: 8, alignItems: "center" }}
        >
          <input
            type="text"
            value={text}
            onChange={(e) => setText(e.target.value)}
            placeholder="sunny afternoon"
            aria-label="station prompt"
            maxLength={500}
            autoFocus
            style={{
              flex: 1,
              padding: "8px 12px",
              fontSize: "0.95em",
              background: "var(--bg-elevated)",
              border: "1px solid var(--border)",
              borderRadius: "var(--radius-1, 2px)",
              color: "var(--fg)",
            }}
          />
          <label
            style={{
              display: "flex",
              alignItems: "center",
              gap: 6,
              color: "var(--fg-muted)",
              fontSize: "0.85em",
            }}
          >
            n
            <input
              type="number"
              value={n}
              min={MIN_N}
              max={MAX_N}
              onChange={(e) =>
                setN(
                  Math.max(
                    MIN_N,
                    Math.min(MAX_N, Number(e.target.value) || DEFAULT_N),
                  ),
                )
              }
              style={{
                width: 60,
                padding: "6px 8px",
                background: "var(--bg-elevated)",
                border: "1px solid var(--border)",
                borderRadius: "var(--radius-1, 2px)",
                color: "var(--fg)",
              }}
            />
          </label>
          <button
            type="submit"
            disabled={
              status.kind === "loading" || text.trim().length === 0
            }
            className="btn-primary"
            style={{ padding: "8px 16px" }}
          >
            {status.kind === "loading" ? "thinking…" : "play"}
          </button>
        </form>

        <StatusBanner status={status} onPlayAll={(tracks) => playList(tracks, 0)} />
      </div>

      {status.kind === "ready" && (
        <div className="section">
          <div className="section-head">
            <h2>tracks for "{status.query}"</h2>
          </div>
          <TrackTable
            tracks={status.tracks}
            showAlbum
            onPlay={(i) => playList(status.tracks, i)}
          />
        </div>
      )}
    </Layout>
  );
}

function StatusBanner({
  status,
  onPlayAll,
}: {
  status: Status;
  onPlayAll: (tracks: Track[]) => void;
}) {
  switch (status.kind) {
    case "idle":
    case "loading":
      return null;
    case "empty":
      return (
        <p className="text-fg-muted text-sm" style={{ marginTop: 12 }}>
          no embedded tracks matched "{status.query}". try a different
          prompt, or check that ingest has run.
        </p>
      );
    case "unavailable":
      return (
        <p className="text-danger text-sm" style={{ marginTop: 12 }}>
          embedder sidecar isn't running — text queries need the CLAP
          text encoder. start the embedder and try again.
        </p>
      );
    case "error":
      return (
        <p className="text-danger text-sm" style={{ marginTop: 12 }}>
          error: {status.message}
        </p>
      );
    case "ready":
      return (
        <p
          className="text-fg-muted text-sm"
          style={{ marginTop: 12, display: "flex", gap: 12, alignItems: "center" }}
        >
          {status.tracks.length} track{status.tracks.length === 1 ? "" : "s"} for
          {" "}"{status.query}".
          <button
            type="button"
            className="btn-secondary"
            onClick={() => onPlayAll(status.tracks)}
            style={{ padding: "4px 12px" }}
          >
            play all
          </button>
        </p>
      );
  }
}
