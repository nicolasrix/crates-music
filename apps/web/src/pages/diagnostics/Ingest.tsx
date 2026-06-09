// /diagnostics/ingest — embedder backlog tiles, refreshed every 5s.

import { useQuery } from "@tanstack/react-query";

import { fetchQueueDepth } from "../../api/diagnostics";
import { DiagSection, ErrorLine, REFRESH_MS } from "./shared";

export function Ingest() {
  const queue = useQuery({
    queryKey: ["diag", "queue_depth"],
    queryFn: () => fetchQueueDepth({}),
    refetchInterval: REFRESH_MS,
  });

  return (
    <div className="section">
      <div className="section-head">
        <h2>ingest</h2>
        <span className="count">refresh {REFRESH_MS / 1000}s</span>
      </div>

      <DiagSection title="ingest queue">
        {queue.error && <ErrorLine error={queue.error} />}
        {queue.data && <QueueDepth data={queue.data} />}
      </DiagSection>
    </div>
  );
}

function QueueDepth({
  data,
}: {
  data: {
    model_version: string;
    not_started: number;
    in_progress: number;
    done: number;
    failed: number;
  };
}) {
  const tiles = [
    { label: "not started", value: data.not_started, tone: "" },
    {
      label: "in progress",
      value: data.in_progress,
      tone: data.in_progress > 0 ? "is-warn" : "",
    },
    { label: "done", value: data.done, tone: "" },
    {
      label: "failed",
      value: data.failed,
      tone: data.failed > 0 ? "is-danger" : "",
    },
  ];
  return (
    <div>
      <p className="text-fg-faint text-xs mb-2">model: {data.model_version}</p>
      <div className="grid grid-cols-2 sm:grid-cols-4 gap-3">
        {tiles.map((t) => (
          <div key={t.label} className={`tile-stat ${t.tone}`}>
            <span className="stat-label">{t.label}</span>
            <span className="stat-value">{t.value}</span>
          </div>
        ))}
      </div>
    </div>
  );
}
