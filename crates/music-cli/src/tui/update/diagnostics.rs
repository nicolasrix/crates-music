//! Reducer for the admin-only Diagnostics section (parity plan Phase 9). Owns
//! sub-tab (`h`/`l`) and window (`[`/`]`) cycling, the throttled auto-refresh,
//! latent-space neighbour fetching, and "enter plays" on a scatter point.
//!
//! Each sub-tab keeps its own `Loadable`, so switching tabs shows the last
//! data instantly while a fresh fetch runs; a spinner is shown only on a tab's
//! first load (matches the web `/diagnostics`, which never blanks on refetch).

use ratatui::widgets::TableState;

use crate::api::{LatentNeighbour, LatentPoint};
use crate::tui::msg::{DiagData, Effect};
use crate::tui::state::{App, DiagTab, DiagWindow, DiagnosticsState, Loadable};

use super::{loadable_from, loaded_len, select_first};

/// Auto-refresh cadence: ~5 s at the 250 ms tick.
const REFRESH_TICKS: u64 = 20;

/// Enter the section: load the active tab.
pub(super) fn reload(app: &mut App) -> Vec<Effect> {
    load_active(app)
}

/// `h`/`l` — cycle the active sub-tab, then load it.
pub(super) fn cycle_tab(app: &mut App, delta: i64) -> Vec<Effect> {
    let n = i64::try_from(DiagTab::ALL.len()).unwrap_or(1);
    let cur = i64::try_from(app.diagnostics.tab % DiagTab::ALL.len()).unwrap_or(0);
    app.diagnostics.tab = usize::try_from((cur + delta).rem_euclid(n)).unwrap_or(0);
    load_active(app)
}

/// `[`/`]` — cycle the time window (the `since_ms` inspectors), then reload
/// the active tab.
pub(super) fn cycle_window(app: &mut App, delta: i64) -> Vec<Effect> {
    let n = i64::try_from(DiagWindow::ALL.len()).unwrap_or(1);
    let cur = i64::try_from(app.diagnostics.window.index()).unwrap_or(0);
    let idx = usize::try_from((cur + delta).rem_euclid(n)).unwrap_or(0);
    app.diagnostics.window = DiagWindow::ALL[idx];
    load_active(app)
}

/// Enter: on the Latent-space tab, play the selected scatter point; elsewhere
/// the tabs are read-only viewers, so Enter is inert.
pub(super) fn activate(app: &mut App) -> Vec<Effect> {
    if app.diagnostics.active_tab() != DiagTab::LatentSpace {
        return vec![];
    }
    let Some(point) = selected_latent_point(app) else {
        return vec![];
    };
    let qt = music_player::QueuedTrack {
        id: point.track_id.clone(),
        title: point
            .title
            .clone()
            .unwrap_or_else(|| point.track_id.clone()),
        artist: point.artist.clone(),
        album: point.album.clone(),
        artist_id: None,
        album_id: None,
        duration: None,
    };
    let label = qt.title.clone();
    app.set_status(format!("playing {label}"), false);
    super::playback::play_new_queue(app, vec![qt], 0)
}

/// The (row count, table) the cursor keys act on for the active tab.
pub(super) fn focused_list(app: &mut App) -> (usize, &mut TableState) {
    match app.diagnostics.active_tab() {
        DiagTab::Listening => (
            loaded_len(&app.diagnostics.listening),
            &mut app.diagnostics.listening_table,
        ),
        DiagTab::Tracing => (
            app.diagnostics.tracing.ready().map_or(0, |t| t.traces.len()),
            &mut app.diagnostics.tracing_table,
        ),
        DiagTab::LatentSpace => (
            app.diagnostics.latent.ready().map_or(0, |l| l.points.len()),
            &mut app.diagnostics.latent_table,
        ),
        DiagTab::ClientEvents => (
            loaded_len(&app.diagnostics.client_events),
            &mut app.diagnostics.client_events_table,
        ),
        // Non-navigable dashboards: hand back the scratch table so the cursor
        // keys no-op without clobbering the latent selection.
        DiagTab::Ingest | DiagTab::Recommender => (0, &mut app.diagnostics.scratch_table),
    }
}

/// 250 ms tick: throttled auto-refresh of the active tab + latent-neighbour
/// refetch when the selected point changed.
pub(super) fn on_tick(app: &mut App) -> Vec<Effect> {
    let mut effects = Vec::new();
    if app.tick.saturating_sub(app.diagnostics.last_refresh_tick) >= REFRESH_TICKS {
        effects.extend(load_active(app));
    }
    effects.extend(maybe_fetch_neighbours(app));
    effects
}

/// A diagnostics sub-tab load completed.
pub(super) fn on_loaded(
    app: &mut App,
    tab: DiagTab,
    result: Result<DiagData, String>,
) -> Vec<Effect> {
    // Drop a completion for a tab the user already switched away from.
    if tab != app.diagnostics.active_tab() {
        return vec![];
    }
    let data = match result {
        Ok(data) => data,
        Err(e) => {
            set_failed(&mut app.diagnostics, tab, e);
            return vec![];
        }
    };
    let d = &mut app.diagnostics;
    match data {
        DiagData::Ingest(q) => d.ingest = Loadable::Ready(q),
        DiagData::Recommender(p) => d.recommender = Loadable::Ready(*p),
        DiagData::Listening(v) => {
            let len = v.len();
            d.listening = Loadable::Ready(v);
            select_first(&mut d.listening_table, len);
        }
        DiagData::Tracing(t) => {
            let len = t.traces.len();
            d.tracing = Loadable::Ready(t);
            select_first(&mut d.tracing_table, len);
        }
        DiagData::Latent(l) => {
            let len = l.points.len();
            d.latent = Loadable::Ready(*l);
            // Keep the current selection if still in range, else select first.
            if d.latent_table.selected().is_none_or(|s| s >= len) {
                select_first(&mut d.latent_table, len);
            }
            // Fetch neighbours for the (re)selected point.
            return maybe_fetch_neighbours(app);
        }
        DiagData::ClientEvents(v) => {
            let len = v.len();
            d.client_events = Loadable::Ready(v);
            select_first(&mut d.client_events_table, len);
        }
    }
    vec![]
}

/// Latent-space neighbours resolved for a point; dropped if the selection has
/// moved on since the request went out.
pub(super) fn on_neighbours(
    app: &mut App,
    seed: &str,
    result: Result<Vec<LatentNeighbour>, String>,
) -> Vec<Effect> {
    if app.diagnostics.neighbour_seed.as_deref() != Some(seed) {
        return vec![];
    }
    app.diagnostics.neighbours = loadable_from(result);
    vec![]
}

// ── internals ──────────────────────────────────────────────────────────────

fn load_active(app: &mut App) -> Vec<Effect> {
    let tab = app.diagnostics.active_tab();
    set_loading_if_idle(app, tab);
    app.diagnostics.last_refresh_tick = app.tick;
    vec![Effect::LoadDiagnostics {
        tab,
        window: app.diagnostics.window,
    }]
}

/// Show a spinner only when a tab has no data yet; a refresh over existing
/// data stays silent (the old data remains until the new load lands).
fn set_loading_if_idle(app: &mut App, tab: DiagTab) {
    let d = &mut app.diagnostics;
    let idle = match tab {
        DiagTab::Ingest => matches!(d.ingest, Loadable::Idle),
        DiagTab::Recommender => matches!(d.recommender, Loadable::Idle),
        DiagTab::Listening => matches!(d.listening, Loadable::Idle),
        DiagTab::Tracing => matches!(d.tracing, Loadable::Idle),
        DiagTab::LatentSpace => matches!(d.latent, Loadable::Idle),
        DiagTab::ClientEvents => matches!(d.client_events, Loadable::Idle),
    };
    if !idle {
        return;
    }
    match tab {
        DiagTab::Ingest => d.ingest = Loadable::Loading,
        DiagTab::Recommender => d.recommender = Loadable::Loading,
        DiagTab::Listening => d.listening = Loadable::Loading,
        DiagTab::Tracing => d.tracing = Loadable::Loading,
        DiagTab::LatentSpace => d.latent = Loadable::Loading,
        DiagTab::ClientEvents => d.client_events = Loadable::Loading,
    }
}

fn set_failed(d: &mut DiagnosticsState, tab: DiagTab, e: String) {
    match tab {
        DiagTab::Ingest => d.ingest = Loadable::Failed(e),
        DiagTab::Recommender => d.recommender = Loadable::Failed(e),
        DiagTab::Listening => d.listening = Loadable::Failed(e),
        DiagTab::Tracing => d.tracing = Loadable::Failed(e),
        DiagTab::LatentSpace => d.latent = Loadable::Failed(e),
        DiagTab::ClientEvents => d.client_events = Loadable::Failed(e),
    }
}

fn selected_latent_point(app: &App) -> Option<&LatentPoint> {
    let sel = app.diagnostics.latent_table.selected()?;
    app.diagnostics.latent.ready()?.points.get(sel)
}

/// Issue a neighbour fetch when the selected latent point differs from the one
/// the side list currently reflects (no-op otherwise, so it's cheap to call on
/// every tick).
fn maybe_fetch_neighbours(app: &mut App) -> Vec<Effect> {
    if app.diagnostics.active_tab() != DiagTab::LatentSpace {
        return vec![];
    }
    let Some(id) = selected_latent_point(app).map(|p| p.track_id.clone()) else {
        return vec![];
    };
    if app.diagnostics.neighbour_seed.as_deref() == Some(id.as_str()) {
        return vec![];
    }
    app.diagnostics.neighbour_seed = Some(id.clone());
    app.diagnostics.neighbours = Loadable::Loading;
    vec![Effect::LoadLatentNeighbours { track_id: id }]
}
