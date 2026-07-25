// Autoplay tuning settings: the knobs that shape the "tethered drift"
// refill. Persisted in localStorage and sent to the gateway on every
// refill via the from-seeds request + queue_context.
//
// Two concerns, deliberately separated (see autoplaySeeds.ts + the
// gateway's `leash` module):
//
//   • Direction — the recency frontier (β, decay, window). Built
//     CLIENT-side: the most recent queue items re-enter the seed pool at
//     a decayed weight so the Σ-similarity aggregation drifts toward
//     where the session has been heading. These never touch the leash.
//   • Boundary  — the anchor leash (τ, λ). Applied SERVER-side: every
//     candidate is demoted by λ·max(0, τ − sim(candidate, nearest
//     anchor))², keeping the drift within a soft radius of the user's
//     anchored tracks.
//
// All values are user-tunable from the Settings page. They are clamped on
// load and save so a hand-edited localStorage blob (or an out-of-range
// slider) can never push the recommender into a degenerate state.

export interface AutoplaySettings {
  /** Leash radius τ (cosine, whitened space). Higher = stay closer to the
   *  anchors. 0 disables the radius (everything is "inside"); 1 is the
   *  tightest possible leash. */
  leashTau: number;
  /** Leash strength λ. Higher = harder pushback past τ. 0 disables the
   *  leash entirely (pure free drift). */
  leashLambda: number;
  /** Frontier weight β: the base seed weight of the most-recent queue
   *  item. 0 disables travel (anchor-only, the old behaviour). */
  frontierWeight: number;
  /** Frontier decay ∈ [0, 1]: each older frontier item is weighted
   *  β·decayᵃᵍᵉ. Lower = only the very last track steers; higher = a
   *  longer tail of recent tracks contributes. */
  frontierDecay: number;
  /** Frontier window K: how many recent queue items feed the frontier. */
  frontierWindow: number;
  /** MMR relevance/novelty tradeoff λ ∈ [0, 1] for the diversity walk. */
  mmrLambda: number;
}

export const DEFAULT_AUTOPLAY_SETTINGS: AutoplaySettings = {
  // Derived from the offline stay-close sweep on the live 768-dim whitened
  // CLaMP 3 corpus (see CLAUDE.md). These keep a station travelling to 60+
  // distinct tracks while holding ~0.4 end-similarity to the anchor.
  leashTau: 0.28,
  leashLambda: 16,
  frontierWeight: 0.15,
  frontierDecay: 0.55,
  frontierWindow: 3,
  mmrLambda: 0.8,
};

const STORAGE_KEY = "crates-music.autoplay.settings";

/** Bounds for each knob, used for clamping + the Settings sliders. */
export const SETTINGS_BOUNDS = {
  leashTau: { min: 0, max: 1, step: 0.01 },
  leashLambda: { min: 0, max: 50, step: 0.5 },
  frontierWeight: { min: 0, max: 3, step: 0.05 },
  frontierDecay: { min: 0, max: 1, step: 0.05 },
  frontierWindow: { min: 0, max: 10, step: 1 },
  mmrLambda: { min: 0, max: 1, step: 0.05 },
} as const;

function clampField(key: keyof AutoplaySettings, value: unknown): number {
  const b = SETTINGS_BOUNDS[key];
  const n = typeof value === "number" && Number.isFinite(value) ? value : NaN;
  if (Number.isNaN(n)) return DEFAULT_AUTOPLAY_SETTINGS[key];
  const clamped = Math.min(b.max, Math.max(b.min, n));
  // frontierWindow is an integer count.
  return key === "frontierWindow" ? Math.round(clamped) : clamped;
}

/** Validate + clamp an arbitrary object into a full AutoplaySettings,
 *  filling missing/invalid fields from the defaults. */
export function normalizeSettings(raw: unknown): AutoplaySettings {
  const obj = (raw ?? {}) as Record<string, unknown>;
  return {
    leashTau: clampField("leashTau", obj.leashTau),
    leashLambda: clampField("leashLambda", obj.leashLambda),
    frontierWeight: clampField("frontierWeight", obj.frontierWeight),
    frontierDecay: clampField("frontierDecay", obj.frontierDecay),
    frontierWindow: clampField("frontierWindow", obj.frontierWindow),
    mmrLambda: clampField("mmrLambda", obj.mmrLambda),
  };
}

export function loadAutoplaySettings(): AutoplaySettings {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return { ...DEFAULT_AUTOPLAY_SETTINGS };
    return normalizeSettings(JSON.parse(raw));
  } catch {
    return { ...DEFAULT_AUTOPLAY_SETTINGS };
  }
}

export function saveAutoplaySettings(settings: AutoplaySettings): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(normalizeSettings(settings)));
  } catch {
    /* localStorage may be unavailable (private mode); ignore */
  }
}
