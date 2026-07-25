//! Settings reducer tests. No disk, no HTTP — every persist is asserted as an
//! `Effect::SaveSettings` *description*; the config write + live-apply happen
//! behind the effect boundary and never run here.

use crate::api::WhoamiInfo;
use crate::config::Quality;

use super::super::msg::{Effect, Msg};
use super::super::state::{App, Section, SettingRow};
use super::update;

fn app() -> App {
    let mut a = App::new(None, false, true);
    a.section = Section::Settings;
    a.settings.table.select(Some(0));
    a
}

/// Select the row at `pos` in the (non-admin) settings list.
fn select(a: &mut App, row: SettingRow) {
    let pos = a.settings_rows().iter().position(|r| *r == row).unwrap();
    a.settings.table.select(Some(pos));
}

fn admin(a: &mut App) {
    a.whoami = Some(WhoamiInfo {
        user_id: 1,
        role: "admin".to_owned(),
        username: Some("owner".to_owned()),
        display_name: None,
    });
}

fn saved_effect(effects: &[Effect]) -> &crate::tui::msg::SettingsSave {
    effects
        .iter()
        .find_map(|e| match e {
            Effect::SaveSettings(s) => Some(s),
            _ => None,
        })
        .expect("a SaveSettings effect")
}

#[test]
fn go_section_selects_first_row_and_refreshes_identity() {
    let mut a = App::new(None, false, true);
    let effects = update(&mut a, Msg::GoSection(Section::Settings));
    assert_eq!(a.settings.table.selected(), Some(0));
    assert!(effects.contains(&Effect::LoadWhoami));
}

#[test]
fn direct_mode_settings_does_not_fetch_whoami() {
    let mut a = App::new(None, false, false); // no gateway
    let effects = update(&mut a, Msg::GoSection(Section::Settings));
    assert!(!effects.contains(&Effect::LoadWhoami));
}

#[test]
fn cycling_stream_quality_persists_new_value() {
    let mut a = app();
    select(&mut a, SettingRow::StreamQuality);
    assert_eq!(a.settings.stream_quality, Quality::Original);
    let effects = update(&mut a, Msg::Activate);
    assert_eq!(a.settings.stream_quality, Quality::Opus128);
    assert_eq!(saved_effect(&effects).stream_quality, Quality::Opus128);
}

#[test]
fn adjust_download_quality_with_l_cycles_and_persists() {
    let mut a = app();
    select(&mut a, SettingRow::DownloadQuality);
    let effects = update(&mut a, Msg::CycleKindNext);
    assert_eq!(a.settings.download_quality, Quality::Opus128);
    assert_eq!(saved_effect(&effects).download_quality, Quality::Opus128);
}

#[test]
fn lowering_regular_budget_evicts_immediately() {
    let mut a = app();
    select(&mut a, SettingRow::RegularBudget);
    let before = a.settings.regular_budget_bytes;
    let effects = update(&mut a, Msg::CycleKindPrev); // h = decrement
    assert!(a.settings.regular_budget_bytes < before);
    let save = saved_effect(&effects);
    assert!(save.evict_now, "a lowered budget must trigger an eviction");
    assert_eq!(save.regular_budget_bytes, a.settings.regular_budget_bytes);
}

#[test]
fn raising_budget_does_not_evict() {
    let mut a = app();
    select(&mut a, SettingRow::PinnedBudget);
    let effects = update(&mut a, Msg::CycleKindNext); // l = increment
    assert!(!saved_effect(&effects).evict_now);
}

#[test]
fn budget_floors_at_zero() {
    let mut a = app();
    a.settings.regular_budget_bytes = 0;
    select(&mut a, SettingRow::RegularBudget);
    update(&mut a, Msg::CycleKindPrev);
    assert_eq!(a.settings.regular_budget_bytes, 0);
}

#[test]
fn min_upcoming_adjusts_and_clamps_at_floor() {
    let mut a = app();
    a.autoplay.min_upcoming = 1;
    select(&mut a, SettingRow::MinUpcoming);
    update(&mut a, Msg::CycleKindPrev);
    assert_eq!(a.autoplay.min_upcoming, 1, "clamped, not 0");
    let effects = update(&mut a, Msg::CycleKindNext);
    assert_eq!(a.autoplay.min_upcoming, 2);
    assert_eq!(saved_effect(&effects).min_upcoming, 2);
}

#[test]
fn drift_param_steps_and_clamps() {
    let mut a = app();
    a.settings.leash_tau = 0.0;
    select(&mut a, SettingRow::LeashTau);
    update(&mut a, Msg::CycleKindPrev); // can't go below 0
    assert!((a.settings.leash_tau - 0.0).abs() < 1e-6);
    let effects = update(&mut a, Msg::CycleKindNext);
    assert!(a.settings.leash_tau > 0.0);
    // The float rides the effect as bits and decodes back.
    let save = saved_effect(&effects);
    assert!((save.autoplay().leash_tau - a.settings.leash_tau).abs() < 1e-6);
}

#[test]
fn enter_on_numeric_row_hints_without_persisting() {
    let mut a = app();
    select(&mut a, SettingRow::MmrLambda);
    let effects = update(&mut a, Msg::Activate);
    assert!(effects.is_empty());
    assert!(a.status.is_some());
}

#[test]
fn output_row_reuses_toggle_output_semantics() {
    // The row delegates to `room::toggle_output`, which refuses without a live
    // sync room (a unit-test App is offline) — so it no-ops with a status
    // rather than flipping. That's the existing, intentional behavior.
    let mut a = app();
    a.sync.output_on = false;
    select(&mut a, SettingRow::OutputDevice);
    let effects = update(&mut a, Msg::Activate);
    assert!(effects.is_empty());
    assert!(!a.sync.output_on);
    assert!(a.status.is_some());
}

#[test]
fn autoplay_row_toggles_and_persists_enabled() {
    let mut a = app();
    a.autoplay.enabled = false;
    select(&mut a, SettingRow::AutoplayEnabled);
    let effects = update(&mut a, Msg::Activate);
    assert!(a.autoplay.enabled);
    assert!(saved_effect(&effects).autoplay_enabled);
}

#[test]
fn reset_autoplay_restores_defaults_and_persists() {
    let mut a = app();
    a.settings.leash_tau = 0.99;
    a.autoplay.min_upcoming = 42;
    select(&mut a, SettingRow::ResetAutoplay);
    let effects = update(&mut a, Msg::Activate);
    let d = crate::config::AutoplayConfig::default();
    assert!((a.settings.leash_tau - d.leash_tau).abs() < 1e-6);
    assert_eq!(a.autoplay.min_upcoming, d.min_upcoming);
    assert!(!effects.is_empty()); // persisted
}

#[test]
fn sign_out_is_two_step() {
    let mut a = app();
    select(&mut a, SettingRow::SignOut);
    // First enter arms it — no effect yet.
    let first = update(&mut a, Msg::Activate);
    assert!(first.is_empty());
    assert!(a.settings.confirm_signout);
    // Second confirms.
    let second = update(&mut a, Msg::Activate);
    assert_eq!(second, vec![Effect::SignOut]);
    assert!(!a.settings.confirm_signout);
}

#[test]
fn navigating_away_disarms_sign_out() {
    let mut a = app();
    select(&mut a, SettingRow::SignOut);
    update(&mut a, Msg::Activate); // arm
    assert!(a.settings.confirm_signout);
    // A cursor move (not just another action) must disarm — otherwise a stray
    // enter after navigating back would sign out on the first press.
    update(&mut a, Msg::NavUp);
    assert!(!a.settings.confirm_signout);
}

#[test]
fn lowering_pinned_budget_does_not_evict() {
    // Pinned entries are never LRU-evicted, so a pinned-budget change must not
    // fire an eviction (which would be a guaranteed no-op DB scan).
    let mut a = app();
    select(&mut a, SettingRow::PinnedBudget);
    let effects = update(&mut a, Msg::CycleKindPrev); // lower it
    assert!(!saved_effect(&effects).evict_now);
}

#[test]
fn signed_out_exits_to_auth_screen() {
    let mut a = app();
    update(&mut a, Msg::SignedOut { result: Ok(()) });
    assert!(a.should_quit);
    assert!(a.exit_message.as_deref().unwrap().contains("auth login"));
}

#[test]
fn signed_out_failure_stays_in_session() {
    let mut a = app();
    update(
        &mut a,
        Msg::SignedOut {
            result: Err("gateway down".to_owned()),
        },
    );
    assert!(!a.should_quit);
    assert!(a.status.is_some());
}

#[test]
fn admin_row_only_present_for_admins() {
    let mut a = app();
    assert!(!a.settings_rows().contains(&SettingRow::InvalidateCache));
    admin(&mut a);
    assert!(a.settings_rows().contains(&SettingRow::InvalidateCache));
}

#[test]
fn invalidate_cache_row_emits_effect() {
    let mut a = app();
    admin(&mut a);
    select(&mut a, SettingRow::InvalidateCache);
    let effects = update(&mut a, Msg::Activate);
    assert_eq!(effects, vec![Effect::CacheInvalidate]);
}

#[test]
fn nav_moves_settings_cursor() {
    let mut a = app();
    update(&mut a, Msg::NavDown);
    assert_eq!(a.settings.table.selected(), Some(1));
    update(&mut a, Msg::NavBottom);
    assert_eq!(
        a.settings.table.selected(),
        Some(a.settings_rows().len() - 1)
    );
}
