// /diagnostics/listening — scrobble window with per-track repeat counts.
//
// Why a window selector + repeat count: B1 needs to pick a recency-window
// default for MMR's "don't surface the same track twice" penalty. The
// signal that matters is "how many tracks repeat inside window W?" — if
// a 24h window has lots of repeats, MMR shouldn't bother penalising.
// If a 7-day window still has noticeable repeats, that's where the
// penalty kicks in.

import { useQuery } from "@tanstack/react-query";
import { useMemo, useState } from "react";

import {
  RecentlyPlayedEntry,
  fetchRecentlyPlayed,
} from "../../api/diagnostics";
import { Layout } from "../../components/Layout";
import { Link } from "../../router";
import { fmtRelativePast } from "../../utils/format";
import { DiagSection, ErrorLine, REFRESH_MS, fmtRecentTime } from "./shared";

const RECENT_LIMIT = 200;

const WINDOW_OPTIONS: ReadonlyArray<{ label: string; ms: number | null }> = [
  { label: "1h", ms: 60 * 60 * 1000 },
  { label: "24h", ms: 24 * 60 * 60 * 1000 },
  { label: "7d", ms: 7 * 24 * 60 * 60 * 1000 },
  { label: "30d", ms: 30 * 24 * 60 * 60 * 1000 },
  { label: "all", ms: null },
];

export function Listening() {
  return (
    <Layout breadcrumb="diagnostics / listening">
      <div className="section">
        <div className="section-head">
          <h2>listening</h2>
          <span className="count">refresh {REFRESH_MS / 1000}s</span>
        </div>

        <DiagSection title="recently played">
          <RecentlyPlayedSection />
        </DiagSection>
      </div>
    </Layout>
  );
}

function RecentlyPlayedSection() {
  const [windowIdx, setWindowIdx] = useState(2); // default 7d
  const win = WINDOW_OPTIONS[windowIdx]!;
  // Recompute since_ms only when the window changes — avoids invalidating
  // the query on every render due to a fresh Date.now().
  const sinceMs = useMemo(
    () => (win.ms == null ? undefined : Date.now() - win.ms),
    [win.ms]
  );
  const recently = useQuery({
    queryKey: ["diag", "recently_played", win.label],
    queryFn: () =>
      fetchRecentlyPlayed(
        sinceMs === undefined
          ? { limit: RECENT_LIMIT }
          : { limit: RECENT_LIMIT, sinceMs }
      ),
    refetchInterval: REFRESH_MS,
  });

  if (recently.error) return <ErrorLine error={recently.error} />;
  const events = recently.data?.events ?? [];

  const uniqueTracks = new Set(events.map((e) => e.track_id)).size;
  const repeats = events.length - uniqueTracks;

  return (
    <div>
      <div className="flex items-center gap-2 mb-3">
        <label className="text-fg-muted text-sm">window:</label>
        <select
          className="search-input"
          style={{ width: "auto", paddingLeft: 12 }}
          value={windowIdx}
          onChange={(e) => setWindowIdx(Number(e.target.value))}
        >
          {WINDOW_OPTIONS.map((w, i) => (
            <option key={w.label} value={i}>
              {w.label}
            </option>
          ))}
        </select>
        <span className="text-fg-faint text-xs">
          limit {RECENT_LIMIT} · {events.length} events · {uniqueTracks} unique ·{" "}
          {repeats} repeats
        </span>
      </div>
      {events.length === 0 ? (
        <p className="text-fg-faint text-sm">no scrobbles in window.</p>
      ) : (
        <RecentlyPlayedTable events={events} />
      )}
    </div>
  );
}

function RecentlyPlayedTable({ events }: { events: RecentlyPlayedEntry[] }) {
  // Per-track repeat counter so the table can flag "you played this 3 times
  // in this window" — the eyeball signal for window-size choice.
  const repeatCount = useMemo(() => {
    const counts = new Map<string, number>();
    for (const e of events) counts.set(e.track_id, (counts.get(e.track_id) ?? 0) + 1);
    return counts;
  }, [events]);
  return (
    <div className="overflow-x-auto">
      <table className="tracks">
        <thead>
          <tr>
            <th className="col-time" style={{ textAlign: "left" }}>
              when
            </th>
            <th className="col-title">title</th>
            <th className="col-artist">artist</th>
            <th className="col-album">album</th>
            <th className="col-time" style={{ textAlign: "right" }}>
              repeats
            </th>
          </tr>
        </thead>
        <tbody>
          {events.map((e) => {
            const repeats = repeatCount.get(e.track_id) ?? 1;
            // Album column collapses album name + year into one cell so
            // we don't grow the table to six columns. "Music for
            // Airports · 1978" is the format; either half may be
            // missing.
            const albumLabel = e.album
              ? e.year != null
                ? `${e.album} · ${e.year}`
                : e.album
              : null;
            return (
              <tr
                key={`${e.track_id}-${e.occurred_at_ms}-${e.received_at_ms}`}
                style={{ cursor: "default" }}
              >
                <td className="col-time" style={{ textAlign: "left" }}>
                  {fmtRelativePast(new Date(e.occurred_at_ms).toISOString()) ??
                    fmtRecentTime(e.occurred_at_ms)}
                </td>
                <td className="col-title">
                  {e.title ?? (
                    <span style={{ fontFamily: "var(--font-mono)" }}>{e.track_id}</span>
                  )}
                </td>
                <td className="col-artist">
                  {e.artist_id && e.artist ? (
                    <Link to={`/artists/${e.artist_id}`}>{e.artist}</Link>
                  ) : (
                    (e.artist ?? "—")
                  )}
                </td>
                <td className="col-album">
                  {e.album_id && albumLabel ? (
                    <Link to={`/albums/${e.album_id}`}>{albumLabel}</Link>
                  ) : (
                    (albumLabel ?? "—")
                  )}
                </td>
                <td className="col-time">{repeats > 1 ? `×${repeats}` : "—"}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
