//! Settings section (section 8) reducer. A form of grouped rows —
//! Playback / Storage / Autoplay / Account / Admin — each edited in place with
//! `enter` (cycle enum / flip toggle / trigger action) or `h`/`l` (adjust a
//! number, cycle an enum). Every edit both mutates `App` (so the form redraws
//! immediately) and emits [`Effect::SaveSettings`], which persists to the
//! config file *and* applies the change live — quality to the next stream,
//! budgets to the cache (evicting when lowered), drift params to the next
//! autoplay refill.
//!
//! `enabled` / `min_upcoming` stay authoritative on `AutoplayState` and
//! output-on-device on `RoomState::output_on`; those rows reuse the existing
//! `autoplay::toggle` / `room::toggle_output` reducers so their side effects
//! (refill kick, silent-remote semantics) aren't duplicated here.

use super::super::msg::{Effect, SettingsSave};
use super::super::state::{App, SettingRow};
use super::{autoplay, room};

// Adjustment grids for the numeric rows. Kept as named constants (not inline
// literals) so the ranges have one place to tune and stay legible; they mirror
// the web `/settings` sliders and `[tui.autoplay]` defaults.
const BUDGET_STEP: u64 = 1 << 30; // 1 GiB per h/l tick
const MIN_UPCOMING: (usize, usize) = (1, 50); // (min, max)
const FRONTIER_WINDOW_MAX: usize = 20;
const LEASH_TAU_STEP: f32 = 0.02;
const LEASH_LAMBDA_STEP: f32 = 1.0;
const LEASH_LAMBDA_MAX: f32 = 64.0;
const FRONTIER_WEIGHT_STEP: f32 = 0.02;
const FRONTIER_DECAY_STEP: f32 = 0.05;
const MMR_LAMBDA_STEP: f32 = 0.05;
/// Most drift knobs are unit-interval fractions.
const UNIT: (f32, f32) = (0.0, 1.0);

pub(super) fn row_count(app: &App) -> usize {
    app.settings_rows().len()
}

/// Entered the section: refresh identity (role gates the admin row + the
/// account card) and select the first row. Nothing loads locally.
pub(super) fn reload(app: &mut App) -> Vec<Effect> {
    app.settings.confirm_signout = false;
    if app.settings.table.selected().is_none() {
        app.settings.table.select(Some(0));
    }
    // Refresh whoami so the account card + admin gating are current; only the
    // gateway can answer, so skip it in direct mode.
    if app.events_enabled {
        vec![Effect::LoadWhoami]
    } else {
        vec![]
    }
}

fn selected_row(app: &App) -> Option<SettingRow> {
    app.settings_rows().get(app.settings.table.selected()?).copied()
}

/// `enter` — cycle an enum, flip a toggle, or trigger an action row.
pub(super) fn activate(app: &mut App) -> Vec<Effect> {
    let Some(row) = selected_row(app) else {
        return vec![];
    };
    if row != SettingRow::SignOut {
        app.settings.confirm_signout = false;
    }
    match row {
        SettingRow::StreamQuality | SettingRow::DownloadQuality => activate_enum(app, row),
        SettingRow::OutputDevice => room::toggle_output(app),
        SettingRow::AutoplayEnabled => toggle_autoplay(app),
        SettingRow::ResetAutoplay => reset_autoplay(app),
        SettingRow::SignOut => sign_out(app),
        SettingRow::InvalidateCache => vec![Effect::CacheInvalidate],
        // Numbers adjust with h/l; `enter` just hints how.
        SettingRow::RegularBudget
        | SettingRow::PinnedBudget
        | SettingRow::MinUpcoming
        | SettingRow::LeashTau
        | SettingRow::LeashLambda
        | SettingRow::FrontierWeight
        | SettingRow::FrontierDecay
        | SettingRow::FrontierWindow
        | SettingRow::MmrLambda => {
            app.set_status("adjust with h / l", false);
            vec![]
        }
    }
}

/// `h` / `l` — decrement / increment the selected row (or cycle enums, flip
/// toggles). `dir` is `-1` for `h`, `+1` for `l`.
pub(super) fn adjust(app: &mut App, dir: i8) -> Vec<Effect> {
    let Some(row) = selected_row(app) else {
        return vec![];
    };
    app.settings.confirm_signout = false;
    let up = dir >= 0;
    match row {
        // Enums cycle in one direction; toggles flip — direction is moot.
        SettingRow::StreamQuality | SettingRow::DownloadQuality => return activate_enum(app, row),
        SettingRow::OutputDevice => return room::toggle_output(app),
        SettingRow::AutoplayEnabled => return toggle_autoplay(app),
        SettingRow::RegularBudget => {
            app.settings.regular_budget_bytes =
                step_u64(app.settings.regular_budget_bytes, up, BUDGET_STEP);
        }
        SettingRow::PinnedBudget => {
            app.settings.pinned_budget_bytes =
                step_u64(app.settings.pinned_budget_bytes, up, BUDGET_STEP);
        }
        SettingRow::MinUpcoming => {
            app.autoplay.min_upcoming =
                step_usize(app.autoplay.min_upcoming, up, 1, MIN_UPCOMING.0, MIN_UPCOMING.1);
        }
        SettingRow::LeashTau => {
            app.settings.leash_tau =
                step_f32(app.settings.leash_tau, up, LEASH_TAU_STEP, UNIT.0, UNIT.1);
        }
        SettingRow::LeashLambda => {
            app.settings.leash_lambda = step_f32(
                app.settings.leash_lambda,
                up,
                LEASH_LAMBDA_STEP,
                0.0,
                LEASH_LAMBDA_MAX,
            );
        }
        SettingRow::FrontierWeight => {
            app.settings.frontier_weight =
                step_f32(app.settings.frontier_weight, up, FRONTIER_WEIGHT_STEP, UNIT.0, UNIT.1);
        }
        SettingRow::FrontierDecay => {
            app.settings.frontier_decay =
                step_f32(app.settings.frontier_decay, up, FRONTIER_DECAY_STEP, UNIT.0, UNIT.1);
        }
        SettingRow::FrontierWindow => {
            app.settings.frontier_window =
                step_usize(app.settings.frontier_window, up, 1, 0, FRONTIER_WINDOW_MAX);
        }
        SettingRow::MmrLambda => {
            app.settings.mmr_lambda =
                step_f32(app.settings.mmr_lambda, up, MMR_LAMBDA_STEP, UNIT.0, UNIT.1);
        }
        // Action rows don't adjust.
        SettingRow::ResetAutoplay | SettingRow::SignOut | SettingRow::InvalidateCache => {
            return vec![];
        }
    }
    // Only the *regular* budget, lowered, can free space — the pinned budget
    // just gates future pins (pinned entries are never LRU-evicted), so an
    // eviction there would be a guaranteed no-op DB scan.
    let evict = row == SettingRow::RegularBudget && !up;
    persist(app, evict)
}

fn activate_enum(app: &mut App, row: SettingRow) -> Vec<Effect> {
    match row {
        SettingRow::StreamQuality => {
            app.settings.stream_quality = app.settings.stream_quality.next();
        }
        SettingRow::DownloadQuality => {
            app.settings.download_quality = app.settings.download_quality.next();
        }
        _ => {}
    }
    persist(app, false)
}

fn toggle_autoplay(app: &mut App) -> Vec<Effect> {
    // Reuse the real toggle (generation bump + possible refill kick), then
    // persist the new `enabled` alongside everything else.
    let mut effects = autoplay::toggle(app);
    effects.push(save_effect(app, false));
    effects
}

fn reset_autoplay(app: &mut App) -> Vec<Effect> {
    let d = crate::config::AutoplayConfig::default();
    app.settings.leash_tau = d.leash_tau;
    app.settings.leash_lambda = d.leash_lambda;
    app.settings.frontier_weight = d.frontier_weight;
    app.settings.frontier_decay = d.frontier_decay;
    app.settings.frontier_window = d.frontier_window;
    app.settings.mmr_lambda = d.mmr_lambda;
    app.autoplay.min_upcoming = d.min_upcoming;
    app.set_status("autoplay drift reset to defaults", false);
    persist(app, false)
}

fn sign_out(app: &mut App) -> Vec<Effect> {
    if app.settings.confirm_signout {
        app.settings.confirm_signout = false;
        vec![Effect::SignOut]
    } else {
        app.settings.confirm_signout = true;
        app.set_status("press enter again to sign out", false);
        vec![]
    }
}

// ── effect completions ────────────────────────────────────────────────────

pub(super) fn on_saved(app: &mut App, result: Result<(), String>) -> Vec<Effect> {
    // The effect already frames the message (config-write vs eviction failure);
    // surface it verbatim rather than double-prefixing.
    if let Err(e) = result {
        app.set_status(e, true);
    }
    vec![]
}

pub(super) fn on_signed_out(app: &mut App, result: Result<(), String>) -> Vec<Effect> {
    match result {
        Ok(()) => {
            app.exit_message =
                Some("Signed out. Run `crates-cli auth login` to sign back in.".to_owned());
            app.should_quit = true;
        }
        Err(e) => app.set_status(format!("sign-out failed: {e}"), true),
    }
    vec![]
}

pub(super) fn on_cache_invalidated(app: &mut App, result: Result<(), String>) -> Vec<Effect> {
    match result {
        Ok(()) => app.set_status("gateway cache invalidated", false),
        Err(e) => app.set_status(format!("cache invalidate failed: {e}"), true),
    }
    vec![]
}

// ── helpers ────────────────────────────────────────────────────────────────

/// Emit the persist-and-apply effect for the current settings state.
fn persist(app: &App, evict: bool) -> Vec<Effect> {
    vec![save_effect(app, evict)]
}

/// Build the [`SettingsSave`] snapshot from current `App` state.
fn save_effect(app: &App, evict_now: bool) -> Effect {
    let s = &app.settings;
    Effect::SaveSettings(SettingsSave {
        stream_quality: s.stream_quality,
        download_quality: s.download_quality,
        regular_budget_bytes: s.regular_budget_bytes,
        pinned_budget_bytes: s.pinned_budget_bytes,
        evict_now,
        autoplay_enabled: app.autoplay.enabled,
        min_upcoming: app.autoplay.min_upcoming,
        frontier_window: s.frontier_window,
        leash_tau_bits: s.leash_tau.to_bits(),
        leash_lambda_bits: s.leash_lambda.to_bits(),
        frontier_weight_bits: s.frontier_weight.to_bits(),
        frontier_decay_bits: s.frontier_decay.to_bits(),
        mmr_lambda_bits: s.mmr_lambda.to_bits(),
    })
}

fn step_u64(v: u64, up: bool, step: u64) -> u64 {
    if up {
        v.saturating_add(step)
    } else {
        v.saturating_sub(step)
    }
}

fn step_usize(v: usize, up: bool, step: usize, lo: usize, hi: usize) -> usize {
    let next = if up {
        v.saturating_add(step)
    } else {
        v.saturating_sub(step)
    };
    next.clamp(lo, hi)
}

#[allow(clippy::float_arithmetic)] // display knob; a rounding wobble is fine
fn step_f32(v: f32, up: bool, step: f32, lo: f32, hi: f32) -> f32 {
    let next = if up { v + step } else { v - step };
    // Round to the step grid so repeated ±0.02 doesn't drift to 0.13999999.
    let snapped = (next / step).round() * step;
    snapped.clamp(lo, hi)
}
