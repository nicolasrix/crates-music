//! Reducer tests for the admin-only Diagnostics section: admin gating of the
//! section, sub-tab / window cycling, stale-completion dropping, and "enter
//! plays" on a latent point.

use crate::api::{LatentPoint, LatentSpace, WhoamiInfo};
use crate::tui::msg::{DiagData, Effect, Msg};
use crate::tui::state::{App, DiagTab, DiagWindow, Loadable, Section};

use super::update;

fn app() -> App {
    App::new(None, false, true)
}

fn admin() -> App {
    let mut a = app();
    a.whoami = Some(WhoamiInfo {
        user_id: 1,
        role: "admin".to_owned(),
        username: None,
        display_name: None,
    });
    a
}

#[test]
fn diagnostics_hidden_for_non_admin() {
    let a = app();
    assert!(!a.is_section_visible(Section::Diagnostics));
    assert!(!a.visible_sections().contains(&Section::Diagnostics));
}

#[test]
fn diagnostics_visible_for_admin() {
    let a = admin();
    assert!(a.is_section_visible(Section::Diagnostics));
    assert_eq!(a.visible_sections().last(), Some(&Section::Diagnostics));
}

#[test]
fn tab_cycle_from_settings_skips_diagnostics_for_non_admin() {
    let mut a = app();
    a.section = Section::Settings;
    // Settings is the last visible section for a non-admin → wraps to Library.
    update(&mut a, Msg::NextSection);
    assert_eq!(a.section, Section::Library);
}

#[test]
fn tab_cycle_reaches_diagnostics_for_admin() {
    let mut a = admin();
    a.section = Section::Settings;
    update(&mut a, Msg::NextSection);
    assert_eq!(a.section, Section::Diagnostics);
    // And wraps back to Library from Diagnostics.
    update(&mut a, Msg::NextSection);
    assert_eq!(a.section, Section::Library);
}

#[test]
fn entering_diagnostics_loads_the_active_tab() {
    let mut a = admin();
    let effects = update(&mut a, Msg::GoSection(Section::Diagnostics));
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::LoadDiagnostics {
            tab: DiagTab::Ingest,
            ..
        }
    )));
    // First load shows a spinner (was Idle).
    assert!(matches!(a.diagnostics.ingest, Loadable::Loading));
}

#[test]
fn h_l_cycles_sub_tabs() {
    let mut a = admin();
    a.section = Section::Diagnostics;
    assert_eq!(a.diagnostics.active_tab(), DiagTab::Ingest);
    update(&mut a, Msg::CycleKindNext);
    assert_eq!(a.diagnostics.active_tab(), DiagTab::Recommender);
    // Wrap backwards past the start → last tab.
    update(&mut a, Msg::CycleKindPrev);
    update(&mut a, Msg::CycleKindPrev);
    assert_eq!(a.diagnostics.active_tab(), DiagTab::ClientEvents);
}

#[test]
fn bracket_cycles_the_window() {
    let mut a = admin();
    a.section = Section::Diagnostics;
    assert_eq!(a.diagnostics.window, DiagWindow::H24);
    update(&mut a, Msg::CycleModeNext);
    assert_eq!(a.diagnostics.window, DiagWindow::All);
    update(&mut a, Msg::CycleModeNext);
    assert_eq!(a.diagnostics.window, DiagWindow::M15);
}

#[test]
fn stale_tab_completion_is_dropped() {
    let mut a = admin();
    a.section = Section::Diagnostics;
    // Active tab is Ingest; a completion for a different tab is ignored.
    update(
        &mut a,
        Msg::DiagnosticsLoaded {
            tab: DiagTab::Listening,
            result: Ok(DiagData::Listening(vec![])),
        },
    );
    assert!(matches!(a.diagnostics.listening, Loadable::Idle));
}

#[test]
fn window_is_resolved_to_a_cutoff() {
    // 24h window → a cutoff exactly 24h before now; "all" → no cutoff.
    assert_eq!(DiagWindow::All.cutoff_ms(1_000_000), None);
    assert_eq!(
        DiagWindow::H1.cutoff_ms(3_600_000),
        Some(3_600_000 - 3_600_000)
    );
}

#[test]
fn enter_plays_the_selected_latent_point() {
    let mut a = admin();
    a.section = Section::Diagnostics;
    a.diagnostics.tab = DiagTab::ALL
        .iter()
        .position(|t| *t == DiagTab::LatentSpace)
        .unwrap();
    a.diagnostics.latent = Loadable::Ready(LatentSpace {
        model_version: "m".to_owned(),
        proj_version: Some("m-2d".to_owned()),
        points: vec![LatentPoint {
            track_id: "t1".to_owned(),
            x: 0.0,
            y: 0.0,
            title: Some("Song".to_owned()),
            artist: Some("Artist".to_owned()),
            album: None,
            genre: Some("Jazz".to_owned()),
        }],
    });
    a.diagnostics.latent_table.select(Some(0));

    update(&mut a, Msg::Activate);
    // The point became the (only) queued track.
    assert_eq!(a.queue.len(), 1);
    assert_eq!(a.queue.items()[0].id, "t1");
}
