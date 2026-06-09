import { HardDrive, RefreshCw } from "lucide-react";
import { useState } from "react";

import { invalidateBrowseCache } from "../../api/diagnostics";
import { useAudioCache } from "../../cache/AudioCacheContext";
import {
  CACHE_BOUNDS,
  CacheSettings,
  DownloadQuality,
  loadCacheSettings,
  normalizeCacheSettings,
  saveCacheSettings,
} from "../../cache/cacheSettings";
import { formatBytes } from "../../cache/format";
import {
  SelectRow,
  SettingsSection,
  SettingsSubgroup,
  SliderRow,
} from "../controls";
import { QUALITY_OPTIONS } from "../quality";

const ICON = { size: 18, strokeWidth: 1.5 } as const;

export function StoragePanel() {
  const cache = useAudioCache();
  const [cacheSettings, setCacheSettingsState] = useState(loadCacheSettings);

  const updateCache = (key: keyof CacheSettings, v: number) => {
    const next = normalizeCacheSettings({ ...cacheSettings, [key]: v });
    setCacheSettingsState(next);
    saveCacheSettings(next);
    // Lowering a cap must evict immediately and visibly (no-op if under).
    void cache.evictToBudget();
  };

  const updateDownloadQuality = (q: DownloadQuality) => {
    const next = { ...cacheSettings, downloadQuality: q };
    setCacheSettingsState(next);
    saveCacheSettings(next);
  };

  return (
    <SettingsSection icon={<HardDrive {...ICON} />} title="offline & cache">
      <p className="text-fg-muted text-sm" style={{ marginBottom: 16 }}>
        Audio is cached in the browser for offline playback. Lowering a budget frees space
        immediately. See what's stored on the{" "}
        <a href="/downloads" style={{ color: "var(--accent, #f0a020)" }}>
          downloads
        </a>{" "}
        page.
      </p>
      <SliderRow
        id="pinned-budget"
        label="Download budget"
        displayValue={formatBytes(cacheSettings.pinnedBudgetBytes)}
        min={CACHE_BOUNDS.pinnedBudgetBytes.min}
        max={CACHE_BOUNDS.pinnedBudgetBytes.max}
        step={CACHE_BOUNDS.pinnedBudgetBytes.step}
        value={cacheSettings.pinnedBudgetBytes}
        help="Space for tracks you save for offline. Never auto-evicted."
        onChange={(v) => updateCache("pinnedBudgetBytes", v)}
      />
      <SliderRow
        id="regular-budget"
        label="Recent cache budget"
        displayValue={formatBytes(cacheSettings.regularBudgetBytes)}
        min={CACHE_BOUNDS.regularBudgetBytes.min}
        max={CACHE_BOUNDS.regularBudgetBytes.max}
        step={CACHE_BOUNDS.regularBudgetBytes.step}
        value={cacheSettings.regularBudgetBytes}
        help="Space for automatically-cached recent plays. Oldest is evicted first when full."
        onChange={(v) => updateCache("regularBudgetBytes", v)}
      />
      <SelectRow
        id="download-quality"
        label="Offline audio quality"
        value={cacheSettings.downloadQuality}
        options={QUALITY_OPTIONS}
        help="Newly cached audio is transcoded server-side to this target — opus 128 packs roughly 8× more music into the budget than FLAC originals. Already-cached tracks keep their current quality. Streaming playback is unaffected."
        onChange={updateDownloadQuality}
      />

      <MetadataCacheActions />
    </SettingsSection>
  );
}

type FlushStatus =
  | { kind: "idle" }
  | { kind: "running" }
  | { kind: "ok"; removed: number }
  | { kind: "error"; message: string };

// Gateway-side metadata cache flush. Lived on the old diagnostics landing;
// it's a maintenance action, so it belongs with the other storage controls.
function MetadataCacheActions() {
  const [status, setStatus] = useState<FlushStatus>({ kind: "idle" });

  async function onClick() {
    setStatus({ kind: "running" });
    try {
      const { removed } = await invalidateBrowseCache();
      setStatus({ kind: "ok", removed });
    } catch (err) {
      setStatus({
        kind: "error",
        message: err instanceof Error ? err.message : String(err),
      });
    }
  }

  return (
    <SettingsSubgroup title="Gateway metadata cache">
      <p className="text-fg-muted text-sm" style={{ marginTop: -6, marginBottom: 12 }}>
        Flush the gateway's metadata cache so new content added in Navidrome shows up immediately.
        Cover-art entries are preserved.
      </p>
      <div className="flex items-center gap-3">
        <button
          type="button"
          onClick={onClick}
          disabled={status.kind === "running"}
          className="text-sm"
          style={{
            display: "inline-flex",
            alignItems: "center",
            gap: 6,
            padding: "6px 12px",
            borderRadius: "var(--radius-2)",
            background: "var(--accent)",
            color: "var(--on-accent)",
            border: 0,
            cursor: status.kind === "running" ? "default" : "pointer",
            opacity: status.kind === "running" ? 0.5 : 1,
            fontWeight: 500,
          }}
        >
          <RefreshCw size={14} strokeWidth={1.5} />
          {status.kind === "running" ? "refreshing…" : "refresh metadata cache"}
        </button>
        {status.kind === "ok" && (
          <span className="text-fg-muted text-sm">
            cleared {status.removed} {status.removed === 1 ? "entry" : "entries"}
          </span>
        )}
        {status.kind === "error" && (
          <span className="text-sm" style={{ color: "var(--danger)" }}>
            {status.message}
          </span>
        )}
      </div>
    </SettingsSubgroup>
  );
}
