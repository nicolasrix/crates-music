import { RotateCcw, SlidersHorizontal } from "lucide-react";

import { useAutoplay } from "../../player/AutoplayContext";
import {
  AutoplaySettings,
  DEFAULT_AUTOPLAY_SETTINGS,
  SETTINGS_BOUNDS,
} from "../../player/autoplaySettings";
import {
  SettingsButton,
  SettingsSection,
  SettingsSubgroup,
  SliderRow,
  ToggleRow,
} from "../controls";

const ICON = { size: 18, strokeWidth: 1.5 } as const;

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

export function AutoplayPanel() {
  const { autoplay, setAutoplay, settings, setSettings } = useAutoplay();

  const updateKnob = (key: keyof AutoplaySettings, v: number) =>
    setSettings({ ...settings, [key]: v });

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
  );
}
