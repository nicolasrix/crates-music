// /diagnostics/rum — browser web-vitals + custom marks uploaded by the
// in-app RUM emitter (apps/web/src/rum/).

import { useQuery } from "@tanstack/react-query";

import { ClientEventEntry, fetchClientEvents } from "../../api/diagnostics";
import { fmtMs } from "../../utils/format";
import { DiagSection, ErrorLine, REFRESH_MS, fmtRecentTime } from "./shared";

const CLIENT_EVENTS_LIMIT = 100;

export function Rum() {
  return (
    <div className="section">
      <div className="section-head">
        <h2>client RUM</h2>
        <span className="count">refresh {REFRESH_MS / 1000}s</span>
      </div>

      <DiagSection title="client events">
        <ClientEventsSection />
      </DiagSection>
    </div>
  );
}

function ClientEventsSection() {
  const events = useQuery({
    queryKey: ["diag", "client_events"],
    queryFn: () => fetchClientEvents({ limit: CLIENT_EVENTS_LIMIT }),
    refetchInterval: REFRESH_MS,
  });
  if (events.error) return <ErrorLine error={events.error} />;
  if (!events.data) return null;
  if (events.data.events.length === 0) {
    return <p className="text-fg-faint text-sm">no client events yet.</p>;
  }
  return (
    <div className="overflow-x-auto">
      <table className="tracks tabular" style={{ fontFamily: "var(--font-mono)" }}>
        <thead>
          <tr>
            <th>received</th>
            <th>name</th>
            <th style={{ textAlign: "right" }}>value</th>
            <th>rating</th>
            <th>page</th>
            <th>session</th>
          </tr>
        </thead>
        <tbody>
          {events.data.events.map((e, i) => (
            <ClientEventRow key={`${e.session_id}-${e.received_ms}-${i}`} event={e} />
          ))}
        </tbody>
      </table>
    </div>
  );
}

function ClientEventRow({ event }: { event: ClientEventEntry }) {
  return (
    <tr style={{ cursor: "default" }}>
      <td className="col-time" style={{ textAlign: "left", fontFamily: "var(--font-mono)" }}>
        {fmtRecentTime(event.received_ms)}
      </td>
      <td className="col-title" style={{ fontFamily: "var(--font-mono)" }}>
        {event.name}
      </td>
      <td className="col-time">
        {event.value_ms === null ? "—" : fmtMs(event.value_ms)}
      </td>
      <td>{event.rating ? <RatingPill rating={event.rating} /> : "—"}</td>
      <td className="col-artist" style={{ fontFamily: "var(--font-mono)" }}>
        {event.page_path}
      </td>
      <td className="col-artist" style={{ fontFamily: "var(--font-mono)" }}>
        {event.session_id.slice(0, 8)}
      </td>
    </tr>
  );
}

function RatingPill({ rating }: { rating: "good" | "needs-improvement" | "poor" }) {
  const cls = rating === "good" ? "is-good" : rating === "poor" ? "is-poor" : "is-warn";
  return <span className={`pill ${cls}`}>{rating}</span>;
}
