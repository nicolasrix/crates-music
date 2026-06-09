// Settings — one card per concern, general → advanced:
//   Account · Playback · Autoplay · Offline & storage · Appearance · App
//
// Persistence is split across small localStorage-backed stores (autoplay
// knobs via AutoplayContext, cache budgets via cacheSettings, stream quality
// via settings/playback, theme via settings/theme). Every control writes
// through immediately, so retuning is instant — no rebuild, no restart.

import {
  Activity,
  HardDrive,
  Info,
  LogOut,
  Palette,
  RotateCcw,
  SlidersHorizontal,
  Smartphone,
  User,
  Volume2,
} from "lucide-react";
import { useState } from "react";

import { useAuth } from "../auth/AuthContext";
import { useAudioCache } from "../cache/AudioCacheContext";
import {
  CACHE_BOUNDS,
  CacheSettings,
  DOWNLOAD_QUALITIES,
  DownloadQuality,
  loadCacheSettings,
  normalizeCacheSettings,
  saveCacheSettings,
} from "../cache/cacheSettings";
import { formatBytes } from "../cache/format";
import { Layout } from "../components/Layout";
import { useAutoplay } from "../player/AutoplayContext";
import {
  AutoplaySettings,
  DEFAULT_AUTOPLAY_SETTINGS,
  SETTINGS_BOUNDS,
} from "../player/autoplaySettings";
import { useInstallPrompt } from "../pwa/installPrompt";
import {
  InfoRow,
  SelectRow,
  SettingsButton,
  SettingsSection,
  SettingsSubgroup,
  SliderRow,
  ToggleRow,
} from "../settings/controls";
import { loadStreamQuality, saveStreamQuality, StreamQuality } from "../settings/playback";
import { loadTheme, saveTheme, Theme } from "../settings/theme";

interface KnobMeta {
  key: keyof AutoplaySettings;
  label: string;
  help: string;
  lowHint: string;
  highHint: string;
}

const BOUNDARY_KNOBS: KnobMeta[] = [
  {
    key: "leashTau",
    label: "Vibe radius (τ)",
    help: "How close recommendations must stay to your anchored tracks. Higher keeps the station tightly on-vibe; lower lets it roam.",
    lowHint: "roam",
    highHint: "stay close",
  },
  {
    key: "leashLambda",
    label: "Leash strength (λ)",
    help: "How hard the station is pulled back once it crosses the radius. 0 turns the leash off entirely (free drift).",
    lowHint: "off",
    highHint: "firm",
  },
];

const TRAVEL_KNOBS: KnobMeta[] = [
  {
    key: "frontierWeight",
    label: "Travel weight (β)",
    help: "How strongly recently-played tracks steer the next picks. 0 freezes the station on your original anchors.",
    lowHint: "anchored",
    highHint: "moves on",
  },
  {
    key: "frontierDecay",
    label: "Travel decay",
    help: "How much older recent tracks still count. Low = only the last track steers; high = a longer recent tail contributes.",
    lowHint: "last only",
    highHint: "long tail",
  },
  {
    key: "frontierWindow",
    label: "Travel window",
    help: "How many recently-played tracks feed the travel direction.",
    lowHint: "narrow",
    highHint: "wide",
  },
];

const DIVERSITY_KNOBS: KnobMeta[] = [
  {
    key: "mmrLambda",
    label: "Diversity λ",
    help: "Relevance-vs-variety tradeoff in the selection walk. Higher favours the closest matches; lower spreads the slate wider.",
    lowHint: "varied",
    highHint: "closest",
  },
];

function fmtKnob(key: keyof AutoplaySettings, v: number): string {
  return key === "frontierWindow" ? String(v) : v.toFixed(2);
}

const QUALITY_LABELS: Record<DownloadQuality, string> = {
  original: "original (no transcode)",
  opus128: "opus · 128 kbps (smallest)",
  mp3128: "mp3 · 128 kbps (most compatible)",
};

const QUALITY_OPTIONS = DOWNLOAD_QUALITIES.map((q) => ({ value: q, label: QUALITY_LABELS[q] }));

const THEME_OPTIONS: { value: Theme; label: string }[] = [
  { value: "system", label: "System" },
  { value: "dark", label: "Dark" },
  { value: "light", label: "Light" },
];

const ICON = { size: 18, strokeWidth: 1.5 } as const;

export function Settings() {
  const { autoplay, setAutoplay, settings, setSettings } = useAutoplay();
  const cache = useAudioCache();
  const { tokens, logout } = useAuth();

  const [cacheSettings, setCacheSettingsState] = useState(loadCacheSettings);
  const [streamQuality, setStreamQuality] = useState<StreamQuality>(loadStreamQuality);
  const [theme, setTheme] = useState<Theme>(loadTheme);

  const updateKnob = (key: keyof AutoplaySettings, v: number) =>
    setSettings({ ...settings, [key]: v });

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

  const updateStreamQuality = (q: StreamQuality) => {
    setStreamQuality(q);
    saveStreamQuality(q);
  };

  const updateTheme = (t: Theme) => {
    setTheme(t);
    saveTheme(t); // also applies <html data-theme> immediately
  };

  const renderKnobs = (title: string, knobs: KnobMeta[]) => (
    <SettingsSubgroup title={title}>
      {knobs.map((m) => {
        const b = SETTINGS_BOUNDS[m.key];
        return (
          <SliderRow
            key={m.key}
            id={`knob-${m.key}`}
            label={m.label}
            displayValue={fmtKnob(m.key, settings[m.key])}
            min={b.min}
            max={b.max}
            step={b.step}
            value={settings[m.key]}
            lowHint={m.lowHint}
            highHint={m.highHint}
            help={m.help}
            onChange={(v) => updateKnob(m.key, v)}
          />
        );
      })}
    </SettingsSubgroup>
  );

  return (
    <Layout breadcrumb="settings">
      {/* ── Account ─────────────────────────────────────────────── */}
      <SettingsSection
        icon={<User {...ICON} />}
        title="account"
        action={
          <SettingsButton danger onClick={() => void logout()}>
            <LogOut size={14} strokeWidth={1.5} />
            sign out
          </SettingsButton>
        }
      >
        <InfoRow label="Status" value="Signed in" />
        <InfoRow label="Server" value={location.origin} />
        {tokens && (
          <InfoRow label="Session expires" value={new Date(tokens.expiresAt).toLocaleString()} />
        )}
      </SettingsSection>

      {/* ── Playback ────────────────────────────────────────────── */}
      <SettingsSection icon={<Volume2 {...ICON} />} title="playback">
        <SelectRow
          id="stream-quality"
          label="Streaming quality"
          value={streamQuality}
          options={QUALITY_OPTIONS}
          help="Transcode target for live playback of tracks that aren't cached. Lower it on a metered connection — opus 128 streams ~8× lighter than FLAC. Independent of offline-download quality; affects the next track loaded."
          onChange={updateStreamQuality}
        />
      </SettingsSection>

      {/* ── Autoplay ────────────────────────────────────────────── */}
      <SettingsSection
        icon={<SlidersHorizontal {...ICON} />}
        title="autoplay"
        action={
          <SettingsButton onClick={() => setSettings({ ...DEFAULT_AUTOPLAY_SETTINGS })}>
            <RotateCcw size={14} strokeWidth={1.5} />
            reset autoplay
          </SettingsButton>
        }
      >
        <ToggleRow
          label="Keep the queue topped up"
          checked={autoplay}
          help="When on, the player refills the upcoming queue with recommendations that travel outward from your picks while staying tethered to them. The knobs below shape that drift; defaults are tuned to stay close."
          onChange={setAutoplay}
        />
        {renderKnobs("Boundary — how far it strays", BOUNDARY_KNOBS)}
        {renderKnobs("Direction — how it travels", TRAVEL_KNOBS)}
        {renderKnobs("Diversity", DIVERSITY_KNOBS)}
      </SettingsSection>

      {/* ── Offline & storage ───────────────────────────────────── */}
      <SettingsSection icon={<HardDrive {...ICON} />} title="offline & storage">
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
      </SettingsSection>

      {/* ── Appearance ──────────────────────────────────────────── */}
      <SettingsSection icon={<Palette {...ICON} />} title="appearance">
        <SelectRow
          id="theme"
          label="Theme"
          value={theme}
          options={THEME_OPTIONS}
          help="System follows your device's light/dark preference."
          onChange={updateTheme}
        />
      </SettingsSection>

      {/* ── App ─────────────────────────────────────────────────── */}
      <AppSection />
    </Layout>
  );
}

// Install offer + diagnostics link + build stamp. Install button is shown
// only when actionable: hidden once running standalone, and on browsers that
// neither fire beforeinstallprompt nor have a manual path (everything but iOS).
function AppSection() {
  const { canInstall, isStandalone, isIos, promptInstall } = useInstallPrompt();
  const showInstall = !isStandalone && (canInstall || isIos);

  return (
    <SettingsSection icon={<Info {...ICON} />} title="app">
      {showInstall && (
        <div style={{ marginBottom: 18 }}>
          <p className="text-fg-muted text-sm" style={{ marginBottom: 12 }}>
            <Smartphone
              size={16}
              strokeWidth={1.5}
              style={{ verticalAlign: "-3px", marginRight: 6 }}
            />
            Install crates to your home screen / app list — it opens in its own window and launches
            offline.
          </p>
          {canInstall ? (
            <SettingsButton onClick={() => void promptInstall()}>
              <Smartphone size={14} strokeWidth={1.5} />
              install
            </SettingsButton>
          ) : (
            <p className="text-fg-muted text-sm">
              On iOS: open the <strong>Share</strong> menu and choose{" "}
              <strong>Add to Home Screen</strong>.
            </p>
          )}
        </div>
      )}

      <InfoRow
        label="Diagnostics"
        value={
          <a href="/diagnostics" style={{ color: "var(--accent, #f0a020)" }}>
            <Activity size={13} strokeWidth={1.5} style={{ verticalAlign: "-2px", marginRight: 4 }} />
            open
          </a>
        }
      />
      <InfoRow label="Build" value={__GIT_SHA__} />
      <InfoRow label="Built" value={new Date(__BUILD_TIME__).toLocaleString()} />
    </SettingsSection>
  );
}
