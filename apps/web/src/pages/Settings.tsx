// Settings: user-facing knobs for the "tethered drift" autoplay refill.
//
// Every parameter that shapes a refill is exposed here. They all live
// client-side (localStorage, via AutoplayContext) and ride to the gateway
// in each refill request, so retuning is instant — no rebuild, no restart.
//
// Two conceptual groups mirror the recommender design:
//   • Boundary (leash) — how far a station may wander from your anchors.
//   • Direction (travel) — how fast it moves on to new territory.
// Plus the diversity λ and the master autoplay toggle.

import { RotateCcw, SlidersHorizontal } from "lucide-react";

import { Layout } from "../components/Layout";
import { useAutoplay } from "../player/AutoplayContext";
import {
  AutoplaySettings,
  DEFAULT_AUTOPLAY_SETTINGS,
  SETTINGS_BOUNDS,
} from "../player/autoplaySettings";

interface KnobMeta {
  key: keyof AutoplaySettings;
  label: string;
  help: string;
  /** Short hints for the two ends of the slider. */
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

function fmt(key: keyof AutoplaySettings, v: number): string {
  return key === "frontierWindow" ? String(v) : v.toFixed(2);
}

function KnobRow({
  meta,
  value,
  onChange,
}: {
  meta: KnobMeta;
  value: number;
  onChange: (v: number) => void;
}) {
  const b = SETTINGS_BOUNDS[meta.key];
  return (
    <div style={{ marginBottom: 18 }}>
      <div
        style={{
          display: "flex",
          justifyContent: "space-between",
          alignItems: "baseline",
          marginBottom: 2,
        }}
      >
        <label htmlFor={`knob-${meta.key}`} style={{ fontWeight: 500 }}>
          {meta.label}
        </label>
        <span
          className="text-sm"
          style={{ fontVariantNumeric: "tabular-nums", color: "var(--fg)" }}
        >
          {fmt(meta.key, value)}
        </span>
      </div>
      <input
        id={`knob-${meta.key}`}
        type="range"
        min={b.min}
        max={b.max}
        step={b.step}
        value={value}
        onChange={(e) => onChange(Number(e.target.value))}
        style={{ width: "100%", accentColor: "var(--accent, #f0a020)" }}
      />
      <div
        className="text-fg-muted text-sm"
        style={{ display: "flex", justifyContent: "space-between", marginTop: 2 }}
      >
        <span>{meta.lowHint}</span>
        <span>{meta.highHint}</span>
      </div>
      <p className="text-fg-muted text-sm" style={{ marginTop: 6 }}>
        {meta.help}
      </p>
    </div>
  );
}

export function Settings() {
  const { autoplay, setAutoplay, settings, setSettings } = useAutoplay();

  const update = (key: keyof AutoplaySettings, v: number) =>
    setSettings({ ...settings, [key]: v });

  const renderGroup = (title: string, knobs: KnobMeta[]) => (
    <div className="section" style={{ marginTop: 24 }}>
      <div className="section-head">
        <h3 style={{ margin: 0 }}>{title}</h3>
      </div>
      {knobs.map((m) => (
        <KnobRow
          key={m.key}
          meta={m}
          value={settings[m.key]}
          onChange={(v) => update(m.key, v)}
        />
      ))}
    </div>
  );

  return (
    <Layout breadcrumb="settings">
      <div className="section">
        <div className="section-head">
          <h2>
            <SlidersHorizontal
              size={18}
              strokeWidth={1.5}
              style={{ verticalAlign: "-3px", marginRight: 8 }}
            />
            autoplay
          </h2>
          <button
            type="button"
            onClick={() => setSettings({ ...DEFAULT_AUTOPLAY_SETTINGS })}
            className="text-sm"
            style={{
              display: "inline-flex",
              alignItems: "center",
              gap: 6,
              padding: "6px 10px",
              background: "var(--bg-elevated)",
              border: "1px solid var(--border)",
              borderRadius: "var(--radius-1, 2px)",
              color: "var(--fg)",
              cursor: "pointer",
            }}
          >
            <RotateCcw size={14} strokeWidth={1.5} />
            reset to defaults
          </button>
        </div>
        <label
          style={{
            display: "flex",
            alignItems: "center",
            gap: 10,
            marginBottom: 8,
          }}
        >
          <input
            type="checkbox"
            checked={autoplay}
            onChange={(e) => setAutoplay(e.target.checked)}
            style={{ accentColor: "var(--accent, #f0a020)" }}
          />
          <span style={{ fontWeight: 500 }}>Keep the queue topped up</span>
        </label>
        <p className="text-fg-muted text-sm">
          When on, the player refills the upcoming queue with recommendations
          that travel outward from your picks while staying tethered to them.
          The knobs below shape that drift; defaults are tuned to stay close.
        </p>
      </div>

      {renderGroup("Boundary — how far it strays", BOUNDARY_KNOBS)}
      {renderGroup("Direction — how it travels", TRAVEL_KNOBS)}
      {renderGroup("Diversity", DIVERSITY_KNOBS)}
    </Layout>
  );
}
